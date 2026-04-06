use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use wilkes_core::embed::{dispatch, Embedder};
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::index::watcher::{IndexWatcher, WatcherConfig};
use wilkes_core::path::is_under;
use wilkes_core::embed::worker::manager::{ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{
    EmbedderModel, EmbeddingEngine, FileEntry, IndexStatus, PreviewData, SearchMode, SearchQuery,
    SemanticSettings, Settings,
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
    embedder: Mutex<Option<Arc<dyn Embedder>>>,
    index: Mutex<Arc<Mutex<Option<SemanticIndex>>>>,
    watcher: Mutex<Option<IndexWatcher>>,
    embed_task: Mutex<Option<EmbedTaskHandle>>,
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
            embedder: Mutex::new(None),
            index: Mutex::new(Arc::new(Mutex::new(None))),
            watcher: Mutex::new(None),
            embed_task: Mutex::new(None),
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
        if let Some(mut w) = self.watcher.lock().unwrap().take() {
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
        self.embedder.lock().unwrap().is_some() && self.index.lock().unwrap().lock().unwrap().is_some()
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
                let mut guard = self.embed_task.lock().unwrap();
                if let Some(task) = guard.as_ref() {
                    if !task.join.is_finished() {
                        return Err("Semantic index is currently being built. Please wait.".into());
                    }
                    *guard = None;
                }
            }

            let embedder = self.embedder.lock().unwrap().clone()
                .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;

            let index_arc = self.index.lock().unwrap().clone();

            // If the query root differs from the indexed root, trigger a
            // background reindex and continue with the current index.
            let query_root_canonical =
                std::fs::canonicalize(&query.root).unwrap_or_else(|_| query.root.clone());
            let root_mismatch = {
                let guard = index_arc.lock().unwrap();
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
                    let guard = self.embed_task.lock().unwrap();
                    guard.as_ref().is_some_and(|t| !t.join.is_finished())
                };
                if already_building {
                    info!("start_search: root changed but reindex already in progress, skipping");
                } else {
                    info!("start_search: root changed, triggering background reindex");
                    let model = EmbedderModel(embedder.model_id().to_string());
                    let engine = {
                        let guard = index_arc.lock().unwrap();
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
            let guard = self.embed_task.lock().unwrap();
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
            let build_fut = crate::commands::embed::build_index(
                root_path.clone(),
                engine,
                model_clone.clone(),
                manager.clone(),
                device.clone(),
                data_dir.clone(),
                progress_tx,
                chunk_size,
                chunk_overlap,
                supported_extensions.clone(),
            );
            tokio::pin!(build_fut);
            let mut progress_rx = progress_rx;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel_for_task.cancelled() => {
                        let _ = std::fs::remove_file(data_dir.join("semantic_index.db"));
                        ctx.events.emit("embed-error", serde_json::json!({
                            "operation": "Build", "message": ""
                        }));
                        return Ok(());
                    }

                    res = &mut build_fut => {
                        match res {
                            Ok(embedder) => {
                                let dim = embedder.dimension();
                                *ctx.embedder.lock().unwrap() = Some(Arc::clone(&embedder));

                                let data_dir_c = data_dir.clone();
                                let m = model_clone.model_id().to_string();
                                match tokio::task::spawn_blocking(move || {
                                    SemanticIndex::open(&data_dir_c, &m, dim)
                                }).await {
                                    Ok(Ok(idx)) => {
                                        let actual_dim = idx.status().dimension;
                                        let index_arc = Arc::new(Mutex::new(Some(idx)));
                                        *ctx.index.lock().unwrap() = Arc::clone(&index_arc);

                                        let mut registry = ExtractorRegistry::new();
                                        registry.register(Box::new(PdfExtractor::new()));

                                        let watcher_config = if engine == EmbeddingEngine::SBERT {
                                            Some(WatcherConfig {
                                                manager: manager.clone(),
                                                model_id: model_clone.model_id().to_string(),
                                                data_dir: data_dir.clone(),
                                                device: device.clone(),
                                                supported_extensions: supported_extensions.clone(),
                                            })
                                        } else {
                                            None
                                        };

                                        let ev1 = Arc::clone(&ctx.events);
                                        let ev2 = Arc::clone(&ctx.events);
                                        match IndexWatcher::start(
                                            root_path,
                                            index_arc,
                                            Arc::new(registry),
                                            Some(embedder),
                                            watcher_config,
                                            chunk_size,
                                            chunk_overlap,
                                            supported_extensions.clone(),
                                            move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                                            move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
                                        ) {
                                            Ok(w) => *ctx.watcher.lock().unwrap() = Some(w),
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
                                        ctx.events.emit("embed-error", serde_json::json!({
                                            "operation": "Build", "message": e.to_string()
                                        }));
                                    }
                                    Err(e) => {
                                        ctx.events.emit("embed-error", serde_json::json!({
                                            "operation": "Build", "message": e.to_string()
                                        }));
                                    }
                                }
                            }
                            Err(e) => {
                                ctx.events.emit("embed-error", serde_json::json!({
                                    "operation": "Build", "message": e.to_string()
                                }));
                            }
                        }
                        break;
                    }

                    Some(p) = progress_rx.recv() => {
                        ctx.events.emit("embed-progress", serde_json::to_value(&p).unwrap_or_default());
                    }
                }
            }
            *ctx.embed_task.lock().unwrap() = None;
            Ok(())
        });

        let cancel = CancellationToken::new();
        *self.embed_task.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
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
            let guard = self.embed_task.lock().unwrap();
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
                    let (probe_tx, _) = tokio::sync::mpsc::channel(1);
                    let installer = dispatch::get_installer(engine, model_clone.clone(), manager, device);
                    if let Err(e) = installer.install(&data_dir, probe_tx).await {
                        ctx.events.emit("embed-error", serde_json::json!({
                            "operation": "Download",
                            "message": format!("Failed to probe model dimensions: {e:#}")
                        }));
                        *ctx.embed_task.lock().unwrap() = None;
                        return Ok(());
                    }
                    match installer.build(&data_dir) {
                        Ok(embedder) => {
                            *ctx.embedder.lock().unwrap() = Some(embedder);
                            ctx.update_semantic_settings(|s| SemanticSettings {
                                enabled: true,
                                engine,
                                model: model_clone,
                                ..s
                            }).await;
                            ctx.events.emit("embed-done", serde_json::json!({ "operation": "Download" }));
                        }
                        Err(e) => {
                            ctx.events.emit("embed-error", serde_json::json!({
                                "operation": "Download", "message": e.to_string()
                            }));
                        }
                    }
                }
                Err(e) => {
                    ctx.events.emit("embed-error", serde_json::json!({
                        "operation": "Download", "message": e.to_string()
                    }));
                }
            }

            *ctx.embed_task.lock().unwrap() = None;
            Ok(())
        });

        let cancel = CancellationToken::new();
        *self.embed_task.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
        Ok(())
    }

    // ── Embed lifecycle ───────────────────────────────────────────────────────

    pub fn cancel_embed(&self) {
        self.worker_manager.kill_active();
        if let Some(task) = self.embed_task.lock().unwrap().take() {
            task.cancel.cancel();
        }
    }

    pub async fn delete_index(&self) -> anyhow::Result<()> {
        self.stop_watcher();
        *self.index.lock().unwrap() = Arc::new(Mutex::new(None));
        *self.embedder.lock().unwrap() = None;
        crate::commands::embed::delete_index(&self.data_dir).await?;
        self.update_semantic_settings(|s| SemanticSettings { index_path: None, ..s }).await;
        Ok(())
    }

    pub async fn get_index_status(&self) -> anyhow::Result<IndexStatus> {
        crate::commands::embed::get_index_status(&self.data_dir).await
    }

    // ── Worker management ─────────────────────────────────────────────────────

    pub async fn get_worker_status(&self) -> anyhow::Result<WorkerStatus> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.worker_manager.send(ManagerCommand::GetStatus(tx)).await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        rx.await.map_err(|e| anyhow::anyhow!("{e}"))
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
            _ => {
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

        *self.embedder.lock().unwrap() = Some(Arc::clone(&embedder));
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock().unwrap() = Arc::clone(&index_arc);

        if let Some(root) = settings.last_directory {
            let mut registry = ExtractorRegistry::new();
            registry.register(Box::new(PdfExtractor::new()));

            let watcher_config = if engine == EmbeddingEngine::SBERT {
                Some(WatcherConfig {
                    manager: self.worker_manager.clone(),
                    model_id: model.model_id().to_string(),
                    data_dir: self.data_dir.clone(),
                    device: settings.semantic.device_for(EmbeddingEngine::SBERT).to_string(),
                    supported_extensions: settings.supported_extensions.clone(),
                })
            } else {
                None
            };

            let ev1 = Arc::clone(&self.events);
            let ev2 = Arc::clone(&self.events);
            match IndexWatcher::start(
                root,
                index_arc,
                Arc::new(registry),
                Some(Arc::clone(&embedder)),
                watcher_config,
                settings.semantic.chunk_size,
                settings.semantic.chunk_overlap,
                settings.supported_extensions.clone(),
                move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
            ) {
                Ok(w) => *self.watcher.lock().unwrap() = Some(w),
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
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use serde_json::Value;

    struct MockEmitter {
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }
    impl EventEmitter for MockEmitter {
        fn emit(&self, name: &str, payload: Value) {
            self.events.lock().unwrap().push((name.to_string(), payload));
        }
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
            },
            emitter.clone(),
        );

        let (tx, rx) = mpsc::channel(1);
        let forwarder = tokio::spawn(ctx.run_event_forwarder(rx));

        tx.send(ManagerEvent::WorkerStarting).await.unwrap();
        drop(tx);
        forwarder.await.unwrap();

        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 1);
        assert_eq!(events_guard[0].0, "manager-event");
        assert_eq!(events_guard[0].1, serde_json::json!("WorkerStarting"));
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
            },
            emitter,
        );

        let watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            ctx.index.lock().unwrap().clone(),
            Arc::new(ExtractorRegistry::new()),
            None,
            None,
            100,
            10,
            vec![],
            || {},
            || {},
        ).unwrap();

        *ctx.watcher.lock().unwrap() = Some(watcher);
        ctx.stop_watcher();
        assert!(ctx.watcher.lock().unwrap().is_none());
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
}
