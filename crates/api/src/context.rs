use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use parking_lot::Mutex as PLMutex;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use wilkes_core::embed::{dispatch, Embedder};
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::index::watcher::IndexWatcher;
use wilkes_core::path::is_under;
use wilkes_core::embed::worker::manager::{ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{
    EmbedderModel, EmbeddingEngine, FileEntry, IndexStatus, IndexingConfig, PreviewData, SearchMode,
    SearchQuery, SemanticSettings, Settings,
};

use crate::commands::search::{start_search, SearchHandle};
use crate::commands::settings::{get_settings, update_settings};

// ── EventEmitter ──────────────────────────────────────────────────────────────

/// Platform-agnostic event sink. The desktop implements this with Tauri's
/// `app.emit()`; the server implements it with a broadcast channel.
pub trait EventEmitter: Send + Sync + 'static {
    fn emit(&self, name: &str, payload: serde_json::Value);
}

// ── Embed task handle ─────────────────────────────────────────────────────────

pub struct EmbedTaskHandle {
    pub cancel: CancellationToken,
    pub join: JoinHandle<anyhow::Result<()>>,
}

// ── AppContext ────────────────────────────────────────────────────────────────

/// Shared application state and lifecycle logic. Both the desktop (Tauri) and
/// the server (axum) create exactly one `Arc<AppContext>` and delegate all
/// business operations to it.
pub struct AppContext {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    embedder: PLMutex<Option<Arc<dyn Embedder>>>,
    index: PLMutex<Arc<Mutex<Option<SemanticIndex>>>>,
    watcher: PLMutex<Option<IndexWatcher>>,
    embed_task: PLMutex<Option<EmbedTaskHandle>>,
    pub worker_manager: WorkerManager,
    events: Arc<dyn EventEmitter>,
    settings_lock: tokio::sync::Mutex<()>,
}

impl AppContext {
    pub fn new(
        data_dir: PathBuf,
        settings_path: PathBuf,
        paths: WorkerPaths,
        events: Arc<dyn EventEmitter>,
    ) -> (Arc<Self>, mpsc::Receiver<ManagerEvent>, impl std::future::Future<Output = ()> + Send) {
        let (worker_manager, event_rx, loop_fut) = WorkerManager::new(paths);
        let ctx = Arc::new(Self {
            data_dir,
            settings_path,
            embedder: PLMutex::new(None),
            index: PLMutex::new(Arc::new(Mutex::new(None))),
            watcher: PLMutex::new(None),
            embed_task: PLMutex::new(None),
            worker_manager,
            events,
            settings_lock: tokio::sync::Mutex::new(()),
        });
        (ctx, event_rx, loop_fut)
    }

    /// Spawns the required background tasks for the application context.
    pub fn spawn_background_tasks(self: Arc<Self>, event_rx: mpsc::Receiver<ManagerEvent>, loop_fut: impl std::future::Future<Output = ()> + Send + 'static) {
        tokio::spawn(loop_fut);
        
        let ctx1 = Arc::clone(&self);
        tokio::spawn(async move {
            ctx1.run_event_forwarder(event_rx).await;
        });

        let ctx2 = Arc::clone(&self);
        tokio::spawn(async move {
            ctx2.restore_state().await;
        });
    }

    /// Forward manager-loop events through the EventEmitter. Run this as a
    /// background task after `new`.
    pub async fn run_event_forwarder(self: Arc<Self>, mut rx: mpsc::Receiver<ManagerEvent>) {
        while let Some(event) = rx.recv().await {
            let name = match event {
                ManagerEvent::WorkerStarting => "WorkerStarting",
                ManagerEvent::ReindexingDone => "ReindexingDone",
            };
            self.events.emit("manager-event", serde_json::json!(name));
        }
    }

    fn emit_embed_error(&self, operation: &str, message: impl Into<String>) {
        let message = message.into();
        if message.is_empty() {
            info!("{operation} cancelled");
        } else {
            error!("{operation} failed: {message}");
        }
        self.events.emit("embed-error", serde_json::json!({
            "operation": operation,
            "message": message,
        }));
    }

    // ── Business Logic ────────────────────────────────────────────────────────

    pub async fn get_settings(&self) -> Settings {
        get_settings(&self.settings_path).await.unwrap_or_default()
    }

    pub async fn list_files(&self, root: PathBuf) -> anyhow::Result<Vec<FileEntry>> {
        let s = self.get_settings().await;
        crate::commands::files::list_files(root, s.supported_extensions, s.max_file_size).await
    }

    pub async fn open_file(&self, path: PathBuf) -> anyhow::Result<PreviewData> {
        if !is_under(&path, &self.data_dir) {
            anyhow::bail!("Access denied: path outside data directory");
        }
        let s = self.get_settings().await;
        crate::commands::files::open_file(path, s.supported_extensions).await
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    async fn settings(&self) -> Settings {
        self.get_settings().await
    }

    fn stop_watcher(&self) {
        if let Some(mut w) = self.watcher.lock().take() {
            w.stop();
        }
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    pub async fn update_semantic_settings<F>(&self, f: F)
    where
        F: FnOnce(SemanticSettings) -> SemanticSettings,
    {
        let _lock = self.settings_lock.lock().await;
        let current = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => { error!("update_semantic_settings: read: {e:#}"); return; }
        };
        let semantic = f(current.semantic);
        if let Err(e) = update_settings(
            &self.settings_path,
            serde_json::json!({ "semantic": semantic }),
        ).await {
            error!("update_semantic_settings: write: {e:#}");
        }
    }

    pub async fn update_settings(&self, patch: serde_json::Value) -> anyhow::Result<wilkes_core::types::Settings> {
        let _lock = self.settings_lock.lock().await;
        update_settings(&self.settings_path, patch).await
    }

    pub fn is_semantic_ready(&self) -> bool {
        self.embedder.lock().is_some()
            && self.index.lock().lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Resolve semantic state (if needed) and start the search. Handles both
    /// Grep and Semantic modes; callers do not branch on mode.
    pub async fn start_search(self: Arc<Self>, mut query: SearchQuery) -> Result<SearchHandle, String> {
        let settings = self.settings().await;
        query.supported_extensions = settings.supported_extensions.clone();

        let (embedder, index) = if query.mode == SearchMode::Semantic {
            // Block if a build task is currently running.
            {
                let mut guard = self.embed_task.lock();
                if let Some(task) = guard.as_ref() {
                    if !task.join.is_finished() {
                        return Err("Semantic index is currently being built. Please wait.".into());
                    }
                    *guard = None;
                }
            }

            let embedder = self.embedder.lock().clone()
                .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;

            let index_arc = self.index.lock().clone();

            // If the query root differs from the indexed root, trigger a
            // background reindex and continue with the current index.
            let query_root_canonical =
                std::fs::canonicalize(&query.root).unwrap_or_else(|_| query.root.clone());
            let root_mismatch = {
                let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
                match guard.as_ref() {
                    Some(idx) => match idx.status().root_path {
                        Some(p) => std::fs::canonicalize(&p).unwrap_or(p) != query_root_canonical,
                        None => true,
                    },
                    None => true,
                }
            };
            if root_mismatch {
                let already_building = {
                    let guard = self.embed_task.lock();
                    guard.as_ref().is_some_and(|t| !t.join.is_finished())
                };
                if already_building {
                    info!("start_search: root changed but reindex already in progress, skipping");
                } else {
                    info!("start_search: root changed, triggering background reindex");
                    let model = EmbedderModel(embedder.model_id().to_string());
                    let engine = {
                        let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
                        guard.as_ref().map(|idx| idx.status().engine).unwrap_or_default()
                    };
                    let ctx = Arc::clone(&self);
                    let root_str = query_root_canonical.to_string_lossy().to_string();
                    tokio::spawn(async move {
                        if let Err(e) = ctx.start_build_index(root_str, model, engine).await {
                            error!("background reindex failed: {e}");
                        }
                    });
                }
            }

            (Some(embedder), Some(index_arc))
        } else {
            (None, None)
        };

        Ok(start_search(query, embedder, index))
    }

    // ── Build index ───────────────────────────────────────────────────────────

    /// Start an index build in the background. Progress, completion, and errors
    /// are emitted through the `EventEmitter` as `embed-progress`, `embed-done`,
    /// and `embed-error` events. Returns as soon as the task is spawned.
    pub async fn start_build_index(
        self: Arc<Self>,
        root: String,
        model: EmbedderModel,
        engine: EmbeddingEngine,
    ) -> Result<(), String> {
        {
            let guard = self.embed_task.lock();
            if guard.as_ref().is_some_and(|t| !t.join.is_finished()) {
                return Err("A build is already in progress.".into());
            }
        }

        let settings = self.settings().await;
        let device = settings.semantic.device_for(engine).to_string();
        let chunk_size = settings.semantic.chunk_size;
        let chunk_overlap = settings.semantic.chunk_overlap;
        let supported_extensions = settings.supported_extensions.clone();

        self.stop_watcher();
        self.events.emit("manager-event", serde_json::json!("Reindexing"));

        let manager = self.worker_manager.clone();
        let data_dir = self.data_dir.clone();
        let ctx = Arc::clone(&self);
        let model_clone = model.clone();

        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<EmbedProgress>(128);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();

        let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            // Always emit ReindexingDone when this task exits.
            struct DoneGuard(Arc<dyn EventEmitter>);
            impl Drop for DoneGuard {
                fn drop(&mut self) {
                    self.0.emit("manager-event", serde_json::json!("ReindexingDone"));
                }
            }
            let _guard = DoneGuard(Arc::clone(&ctx.events));

            let root_path = PathBuf::from(&root);
            let options = crate::commands::embed::BuildIndexOptions {
                manager: Some(manager.clone()),
                device: Some(device.clone()),
                data_dir: data_dir.clone(),
                tx: progress_tx,
                chunk_size,
                chunk_overlap,
                supported_extensions: supported_extensions.clone(),
            };
            let build_fut = crate::commands::embed::build_index(
                root_path.clone(),
                engine,
                model_clone.clone(),
                options,
            );
            tokio::pin!(build_fut);
            let mut progress_rx = progress_rx;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel_for_task.cancelled() => {
                        let _ = std::fs::remove_file(data_dir.join("semantic_index.db"));
                        ctx.emit_embed_error("Build", "");
                        return Ok(());
                    }

                    res = &mut build_fut => {
                        match res {
                            Ok(embedder) => {
                                let dim = embedder.dimension();
                                *ctx.embedder.lock() = Some(Arc::clone(&embedder));

                                let data_dir_c = data_dir.clone();
                                let m = model_clone.model_id().to_string();
                                match tokio::task::spawn_blocking(move || {
                                    SemanticIndex::open(&data_dir_c, &m, dim)
                                }).await {
                                    Ok(Ok(idx)) => {
                                        let actual_dim = idx.status().dimension;
                                        let index_arc = Arc::new(Mutex::new(Some(idx)));
                                        *ctx.index.lock() = Arc::clone(&index_arc);

                                        let mut registry = ExtractorRegistry::new();
                                        registry.register(Box::new(PdfExtractor::new()));

                                        let ev1 = Arc::clone(&ctx.events);
                                        let ev2 = Arc::clone(&ctx.events);
                                        let indexing = IndexingConfig {
                                            chunk_size,
                                            chunk_overlap,
                                            supported_extensions: supported_extensions.clone(),
                                        };
                                        match IndexWatcher::start(
                                            root_path,
                                            index_arc,
                                            Arc::new(registry),
                                            embedder,
                                            indexing,
                                            move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                                            move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
                                        ) {
                                            Ok(w) => *ctx.watcher.lock() = Some(w),
                                            Err(e) => error!("watcher start failed: {e:#}"),
                                        }

                                        ctx.update_semantic_settings(|s| SemanticSettings {
                                            index_path: Some(data_dir.join("semantic_index.db")),
                                            dimension: actual_dim,
                                            enabled: true,
                                            ..s
                                        }).await;

                                        ctx.events.emit("embed-done", serde_json::json!({ "operation": "Build" }));
                                    }
                                    Ok(Err(e)) => {
                                        ctx.emit_embed_error("Build", e.to_string());
                                    }
                                    Err(e) => {
                                        ctx.emit_embed_error("Build", e.to_string());
                                    }
                                }
                            }
                            Err(e) => {
                                ctx.emit_embed_error("Build", e.to_string());
                            }
                        }
                        break;
                    }

                    Some(p) = progress_rx.recv() => {
                        ctx.events.emit("embed-progress", serde_json::to_value(&p).unwrap_or_default());
                    }
                }
            }
            *ctx.embed_task.lock() = None;
            Ok(())
        });

        let cancel = CancellationToken::new();
        *self.embed_task.lock() = Some(EmbedTaskHandle { cancel, join });
        Ok(())
    }

    // ── Download model ────────────────────────────────────────────────────────

    /// Download a model in the background and load it into state on success.
    pub async fn start_download_model(
        self: Arc<Self>,
        model: EmbedderModel,
        engine: EmbeddingEngine,
    ) -> Result<(), String> {
        {
            let guard = self.embed_task.lock();
            if guard.as_ref().is_some_and(|t| !t.join.is_finished()) {
                return Err("A build is already in progress.".into());
            }
        }

        let settings = self.settings().await;
        let device = settings.semantic.device_for(engine).to_string();
        let data_dir = self.data_dir.clone();
        let manager = self.worker_manager.clone();
        let ctx = Arc::clone(&self);
        let model_clone = model.clone();

        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

        let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            let ev = Arc::clone(&ctx.events);
            let forward = tokio::spawn(async move {
                let mut rx = progress_rx;
                while let Some(p) = rx.recv().await {
                    ev.emit("embed-progress", serde_json::to_value(&p).unwrap_or_default());
                }
            });

            let result = crate::commands::embed::download_model(
                engine,
                model_clone.clone(),
                manager.clone(),
                device.clone(),
                data_dir.clone(),
                progress_tx,
            ).await;

            let _ = forward.await;

            match result {
                Ok(()) => {
                    // Probe model dimensions by running install again (no-op if cached).
                    let (probe_tx, _) = mpsc::channel(1);
                    let installer = dispatch::get_installer(engine, model_clone.clone(), manager, device);
                    if let Err(e) = installer.install(&data_dir, probe_tx).await {
                        ctx.emit_embed_error("Download", format!("Failed to probe model dimensions: {e:#}"));
                        *ctx.embed_task.lock() = None;
                        return Ok(());
                    }
                    match installer.build(&data_dir) {
                        Ok(embedder) => {
                            *ctx.embedder.lock() = Some(embedder);
                            ctx.update_semantic_settings(|s| SemanticSettings {
                                enabled: true,
                                engine,
                                model: model_clone,
                                ..s
                            }).await;
                            ctx.events.emit("embed-done", serde_json::json!({ "operation": "Download" }));
                        }
                        Err(e) => {
                            ctx.emit_embed_error("Download", e.to_string());
                        }
                    }
                }
                Err(e) => {
                    ctx.emit_embed_error("Download", e.to_string());
                }
            }

            *ctx.embed_task.lock() = None;
            Ok(())
        });

        let cancel = CancellationToken::new();
        *self.embed_task.lock() = Some(EmbedTaskHandle { cancel, join });
        Ok(())
    }

    // ── Embed lifecycle ───────────────────────────────────────────────────────

    pub fn cancel_embed(&self) {
        self.worker_manager.kill_active();
        if let Some(task) = self.embed_task.lock().take() {
            task.cancel.cancel();
        }
    }

    pub async fn delete_index(&self) -> anyhow::Result<()> {
        self.stop_watcher();
        *self.index.lock() = Arc::new(Mutex::new(None));
        *self.embedder.lock() = None;
        crate::commands::embed::delete_index(&self.data_dir).await?;
        self.update_semantic_settings(|s| SemanticSettings { index_path: None, ..s }).await;
        Ok(())
    }

    pub async fn get_index_status(&self) -> anyhow::Result<IndexStatus> {
        crate::commands::embed::get_index_status(&self.data_dir).await
    }

    // ── Worker management ─────────────────────────────────────────────────────

    pub fn get_worker_status(&self) -> WorkerStatus {
        self.worker_manager.status()
    }

    pub fn kill_worker(&self) {
        self.worker_manager.kill_active();
        let _ = self.worker_manager.try_send(ManagerCommand::KillWorker);
    }

    pub async fn set_worker_timeout(&self, secs: u64) -> anyhow::Result<()> {
        self.worker_manager.send(ManagerCommand::SetTimeout(secs)).await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    // ── Startup restore ───────────────────────────────────────────────────────

    /// Reload the embedder and index from disk if they were previously built,
    /// and restart the filesystem watcher. Run this once after `new`.
    pub async fn restore_state(self: Arc<Self>) {
        let settings = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => { error!("restore_state: read settings: {e:#}"); return; }
        };

        let db_status = match tokio::task::spawn_blocking({
            let d = self.data_dir.clone();
            move || SemanticIndex::read_status_from_path(&d)
        }).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                info!("restore_state: no index DB ({e:#}), nothing to restore");
                // DB missing or unreadable — clear any stale enabled/index_path so the
                // UI doesn't think an index exists when none can be loaded.
                if settings.semantic.enabled || settings.semantic.index_path.is_some() {
                    self.update_semantic_settings(|s| SemanticSettings {
                        enabled: false,
                        index_path: None,
                        ..s
                    }).await;
                }
                return;
            }
            Err(e) => {
                error!("restore_state: spawn_blocking panicked: {e}");
                return;
            }
        };

        let model = settings.semantic.model.clone();
        let engine = settings.semantic.engine;

        if db_status.model_id != model.model_id() {
            info!(
                "restore_state: index model '{}' != settings model '{}', clearing stale index reference",
                db_status.model_id, model.model_id()
            );
            self.update_semantic_settings(|s| SemanticSettings {
                enabled: false,
                index_path: None,
                ..s
            }).await;
            return;
        }

        let installer = dispatch::get_installer(
            engine,
            model.clone(),
            self.worker_manager.clone(),
            settings.semantic.device_for(engine).to_string(),
        );

        let (probe_tx, _) = tokio::sync::mpsc::channel(1);
        if let Err(e) = installer.install(&self.data_dir, probe_tx).await {
            error!("restore_state: install probe failed: {e:#}");
            return;
        }
        if !installer.is_available(&self.data_dir) {
            info!("restore_state: model files absent, skipping");
            return;
        }

        let data_dir = self.data_dir.clone();
        let embedder = match tokio::task::spawn_blocking(move || installer.build(&data_dir)).await {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => { error!("restore_state: build embedder: {e:#}"); return; }
            Err(e) => { error!("restore_state: build embedder panicked: {e}"); return; }
        };

        let data_dir = self.data_dir.clone();
        let m = model.model_id().to_string();
        let expected_dim = embedder.dimension();
        let index = match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir, &m, expected_dim)).await {
            Ok(Ok(idx)) => idx,
            Ok(Err(e)) => { error!("restore_state: open index: {e:#}"); return; }
            Err(e) => { error!("restore_state: open index panicked: {e}"); return; }
        };

        *self.embedder.lock() = Some(Arc::clone(&embedder));
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);

        if let Some(root) = settings.last_directory {
            let mut registry = ExtractorRegistry::new();
            registry.register(Box::new(PdfExtractor::new()));

            let ev1 = Arc::clone(&self.events);
            let ev2 = Arc::clone(&self.events);
            let indexing = IndexingConfig {
                chunk_size: settings.semantic.chunk_size,
                chunk_overlap: settings.semantic.chunk_overlap,
                supported_extensions: settings.supported_extensions.clone(),
            };
            match IndexWatcher::start(
                root,
                index_arc,
                Arc::new(registry),
                Arc::clone(&embedder),
                indexing,
                move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
            ) {
                Ok(w) => *self.watcher.lock() = Some(w),
                Err(e) => error!("restore_state: watcher: {e:#}"),
            }
        }

        let db_path = self.data_dir.join("semantic_index.db");
        let dim = db_status.dimension;
        self.update_semantic_settings(|s| SemanticSettings {
            enabled: true,
            index_path: Some(db_path),
            dimension: dim,
            ..s
        }).await;

        info!("restore_state: embedder and index restored");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tracing::subscriber;
    use tracing_subscriber::prelude::*;
    use wilkes_core::embed::MockEmbedder;

    struct MockEmitter {
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }
    impl EventEmitter for MockEmitter {
        fn emit(&self, name: &str, payload: Value) {
            self.events.lock().unwrap().push((name.to_string(), payload));
        }
    }

    #[test]
    fn test_emit_embed_error_logs_and_emits() {
        wilkes_core::logging::clear_logs();

        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter { events: events.clone() });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let subscriber = tracing_subscriber::registry().with(wilkes_core::logging::BufferLayer);
        subscriber::with_default(subscriber, || {
            ctx.emit_embed_error("Build", "Worker error");
        });

        let logs = wilkes_core::logging::get_logs();
        assert!(logs.iter().any(|line| line.contains("Build failed: Worker error")));

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"] == "Worker error"
        }));
    }

    #[tokio::test]
    async fn test_app_context_new() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (ctx, _event_rx, _loop_fut) = AppContext::new(
            data_dir,
            settings_path.clone(),
            paths,
            emitter,
        );

        ctx.update_semantic_settings(|s| SemanticSettings {
            enabled: true,
            chunk_size: 1234,
            ..s
        }).await;

        let updated = get_settings(&settings_path).await.unwrap();
        assert_eq!(updated.semantic.enabled, true);
        assert_eq!(updated.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_event_forwarder() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter { events: events.clone() });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter.clone(),
        );

        let (tx, rx) = mpsc::channel(1);
        let forwarder = tokio::spawn(ctx.run_event_forwarder(rx));

        tx.send(ManagerEvent::WorkerStarting).await.unwrap();
        tx.send(ManagerEvent::ReindexingDone).await.unwrap();
        drop(tx);
        forwarder.await.unwrap();

        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 2);
        assert_eq!(events_guard[0].0, "manager-event");
        assert_eq!(events_guard[0].1, serde_json::json!("WorkerStarting"));
        assert_eq!(events_guard[1].1, serde_json::json!("ReindexingDone"));
    }

    #[tokio::test]
    async fn test_stop_watcher() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            ctx.index.lock().clone(),
            Arc::new(ExtractorRegistry::new()),
            Arc::new(MockEmbedder::default()),
            IndexingConfig { chunk_size: 100, chunk_overlap: 10, supported_extensions: vec![] },
            || {},
            || {},
        ).unwrap();

        *ctx.watcher.lock() = Some(watcher);
        ctx.stop_watcher();
        assert!(ctx.watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_is_semantic_ready() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        assert!(!ctx.is_semantic_ready());
    }

    #[tokio::test]
    async fn test_cancel_embed() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.cancel_embed(); // Should not panic
    }

    #[tokio::test]
    async fn test_delete_index() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
        let res = ctx.delete_index().await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_index_status() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let res = ctx.get_index_status().await;
        assert!(res.is_err()); // No index exists
    }

    #[tokio::test]
    async fn test_kill_worker() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.kill_worker(); // Should not panic
    }

    #[tokio::test]
    async fn test_set_worker_timeout() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let res = ctx.set_worker_timeout(100).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_settings_operations() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let initial = ctx.get_settings().await;
        assert_eq!(initial.context_lines, 2);

        let patch = serde_json::json!({ "context_lines": 5 });
        let updated = ctx.update_settings(patch).await.unwrap();
        assert_eq!(updated.context_lines, 5);
        
        let _updated_semantic = ctx.update_semantic_settings(|s| {
            SemanticSettings { enabled: true, ..s }
        }).await;
        
        // Settings should have been saved to disk
        let disk_content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        assert!(disk_content.contains("\"context_lines\": 5"));
        assert!(disk_content.contains("\"enabled\": true"));
    }

    #[tokio::test]
    async fn test_file_operations() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        
        tokio::fs::write(root.join("test.txt"), "hello").await.unwrap();
        tokio::fs::write(root.join("test.pdf"), "fake pdf").await.unwrap();
        tokio::fs::create_dir(root.join("subdir")).await.unwrap();
        
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let files = ctx.list_files(root.to_path_buf()).await.unwrap();
        assert!(files.len() >= 2);
        
        let preview = ctx.open_file(root.join("test.txt")).await.unwrap();
        match preview {
            PreviewData::Text { content, .. } => assert!(content.contains("hello")),
            _ => panic!("Expected Text preview"),
        }
    }

    #[tokio::test]
    async fn test_start_search_grep() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            supported_extensions: vec![],
        };

        let handle = ctx.clone().start_search(query).await.unwrap();
        // SearchHandle only has rx field (mpsc::Receiver)
        drop(handle);
    }

    #[tokio::test]
    async fn test_start_search_semantic_missing() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Semantic,
            supported_extensions: vec![],
        };

        let res = ctx.clone().start_search(query).await;
        match res {
            Err(e) => assert!(e.contains("No semantic index found")),
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_get_worker_status() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let status = ctx.get_worker_status();
        assert_eq!(status.active, false);
    }

    #[tokio::test]
    async fn test_spawn_background_tasks() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, rx, loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.spawn_background_tasks(rx, loop_fut);
        // Just verify it doesn't panic and tasks are spawned
    }

    #[tokio::test]
    async fn test_restore_state_no_index() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // No index on disk, no settings
        ctx.clone().restore_state().await;
        assert!(!ctx.is_semantic_ready());
    }

    #[tokio::test]
    async fn test_start_build_index_already_in_progress() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle { cancel, join });

        let res = ctx.start_build_index(
            "root".to_string(),
            EmbedderModel("m".to_string()),
            EmbeddingEngine::Candle
        ).await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_start_build_index_root_not_found() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter { events: events.clone() });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let res = ctx.start_build_index(
            "/non/existent/path/for/sure/12345".to_string(),
            EmbedderModel("m".to_string()),
            EmbeddingEngine::Candle
        ).await;

        // The call itself is Ok because it spawns a task
        assert!(res.is_ok());
        
        // But it should eventually emit an error
        let mut found = false;
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let events_guard = events.lock().unwrap();
            if events_guard.iter().any(|e| e.0 == "embed-error") {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    #[tokio::test]
    async fn test_start_search_semantic_build_in_progress() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle { cancel, join });

        let query = SearchQuery {
            mode: SearchMode::Semantic,
            root: dir.path().to_path_buf(),
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            supported_extensions: vec![],
        };

        let res = ctx.start_search(query).await;
        match res {
            Err(e) => assert!(e.contains("Semantic index is currently being built")),
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_start_download_model_already_in_progress() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle { cancel, join });

        let res = ctx.start_download_model(
            EmbedderModel("m".to_string()),
            EmbeddingEngine::Candle
        ).await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_open_file_denied() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();
        
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        
        let res = ctx.open_file(outside).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("Access denied"));
    }

    #[tokio::test]
    async fn test_start_search_semantic_root_mismatch() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let embedder = Arc::new(MockEmbedder::default());
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();
        
        // Mock an embedder and index
        *ctx.embedder.lock() = Some(embedder);
        
        // Create an index on disk so we can open it
        let root1 = dir.path().join("root1");
        std::fs::create_dir_all(&root1).unwrap();
        let idx = SemanticIndex::create(&data_dir, &model_id, dimension, EmbeddingEngine::Candle, Some(&root1)).unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

        // Search in a different root
        let root2 = dir.path().join("root2");
        std::fs::create_dir_all(&root2).unwrap();
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root2.clone(),
            file_type_filters: vec![],
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Semantic,
            supported_extensions: vec![],
        };

        // This should trigger a background reindex because root2 != root1
        let _handle = ctx.clone().start_search(query).await.unwrap();
        
        // Check if embed_task was set (reindex triggered)
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        assert!(ctx.embed_task.lock().is_some());
    }

    #[tokio::test]
    async fn test_restore_state_model_mismatch() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Write settings with model A
        let settings = Settings {
            semantic: SemanticSettings {
                model: EmbedderModel("model-A".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        tokio::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).await.unwrap();

        // Write index status with model B
        SemanticIndex::create(&data_dir, "model-B", 1, EmbeddingEngine::Candle, None).unwrap();

        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path.clone(),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        ctx.clone().restore_state().await;
        
        // Should have cleared the stale index reference in settings
        let updated_settings = ctx.get_settings().await;
        assert_eq!(updated_settings.semantic.enabled, false);
        assert!(updated_settings.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_update_semantic_settings_error() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("invalid.json");
        std::fs::write(&settings_path, "{ broken }").unwrap();
        
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.update_semantic_settings(|s| s).await;
    }

    #[tokio::test]
    async fn test_restore_state_model_mismatch_clears_settings() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        
        let settings = Settings {
            semantic: SemanticSettings {
                model: EmbedderModel("model-A".to_string()),
                enabled: true,
                index_path: Some(data_dir.join("semantic_index.db")),
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        wilkes_core::embed::index::SemanticIndex::create(&data_dir, "model-B", 1, EmbeddingEngine::Candle, None).unwrap();

        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        ctx.clone().restore_state().await;
        let updated = ctx.get_settings().await;
        assert_eq!(updated.semantic.enabled, false);
        assert!(updated.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_worker_operations() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let status = ctx.get_worker_status();
        assert!(!status.active);

        ctx.kill_worker();
        
        // set_worker_timeout sends to the manager loop, which is running, so it should succeed.
        let res = ctx.set_worker_timeout(10).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_delete_index_operation() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        // Create a fake index
        std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
        // Note: delete_index currently only removes the .db file.
        std::fs::write(data_dir.join("semantic_index.status.json"), r#"{"model_id": "m", "dimension": 1, "engine": "Candle"}"#).unwrap();

        ctx.delete_index().await.unwrap();
        assert!(!data_dir.join("semantic_index.db").exists());
        
        let settings = ctx.get_settings().await;
        assert!(settings.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_get_index_status_not_found() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        let res = ctx.get_index_status().await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_update_settings_patch() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        let patch = serde_json::json!({
            "supported_extensions": ["rs", "txt"]
        });
        ctx.update_settings(patch).await.unwrap();
        
        let settings = ctx.get_settings().await;
        assert_eq!(settings.supported_extensions, vec!["rs", "txt"]);
    }

    #[tokio::test]
    async fn test_update_semantic_settings_patch() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        ctx.update_semantic_settings(|mut s| {
            s.chunk_size = 1234;
            s
        }).await;
        
        let settings = ctx.get_settings().await;
        assert_eq!(settings.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_cancel_embed_operation() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter { events: events.clone() });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );

        // Start a fake build index
        ctx.clone().start_build_index(
            dir.path().to_string_lossy().to_string(),
            EmbedderModel("m".to_string()),
            EmbeddingEngine::Candle
        ).await.unwrap();

        // Immediately cancel
        ctx.cancel_embed();
        
        // Give it some time to process
        let mut success = false;
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let events_guard = events.lock().unwrap();
            if events_guard.iter().any(|e| 
                e.0 == "embed-error" || 
                e.0 == "embed-done" ||
                (e.0 == "manager-event" && e.1 == serde_json::json!("ReindexingDone"))
            ) {
                success = true;
                break;
            }
        }
        assert!(success);
    }

    #[tokio::test]
    async fn test_start_download_model_error() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter { events: events.clone() });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths { 
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );

        // Requesting download of non-existent model should eventually emit error
        ctx.clone().start_download_model(
            EmbedderModel("invalid-model".to_string()),
            EmbeddingEngine::Fastembed
        ).await.unwrap();
        
        // Wait for task to fail
        let mut found = false;
        for _ in 0..10 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let events_guard = events.lock().unwrap();
            if events_guard.iter().any(|e| e.0 == "embed-error") {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    #[tokio::test]
    async fn test_restore_state_complex() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        
        // Seed settings.json with a different model
        let mut initial_settings = wilkes_core::types::Settings::default();
        initial_settings.semantic.model = EmbedderModel("m1".to_string());
        std::fs::write(&settings_path, serde_json::to_string(&initial_settings).unwrap()).unwrap();

        // Create an index status file matching that model
        let index_dir = dir.path().join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let status = wilkes_core::types::IndexStatus {
            model_id: "m1".to_string(),
            engine: EmbeddingEngine::Candle,
            dimension: 128,
            indexed_files: 1,
            total_chunks: 10,
            built_at: Some(12345678),
            build_duration_ms: Some(1000),
            root_path: Some(dir.path().to_path_buf()),
            db_size_bytes: Some(1024),
        };
        std::fs::write(index_dir.join("status.json"), serde_json::to_string(&status).unwrap()).unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        // AppContext::new might have already called restore_state internally,
        // but let's be explicit and check if it sticks.
        ctx.clone().restore_state().await;
        
        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.model.0, "m1");
    }

    #[tokio::test]
    async fn test_restore_state_open_index_fail() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a corrupted index path (a directory where a file should be)
        let index_path = data_dir.join("semantic_index.db");
        std::fs::create_dir(&index_path).unwrap();

        let settings = Settings {
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(index_path),
                model: EmbedderModel("m".to_string()),
                engine: EmbeddingEngine::Candle,
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        ctx.clone().restore_state().await;
        
        let updated = ctx.get_settings().await;
        assert_eq!(updated.semantic.enabled, false);
    }

    #[tokio::test]
    async fn test_update_semantic_settings_success() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("s.json");
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter { events: Arc::new(Mutex::new(Vec::new())) }),
        );

        ctx.update_semantic_settings(|s| SemanticSettings {
            enabled: true,
            ..s
        }).await;

        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.enabled, true);
        assert!(settings_path.exists());
    }
}
