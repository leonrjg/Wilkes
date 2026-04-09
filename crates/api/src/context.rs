use parking_lot::Mutex as PLMutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use wilkes_core::embed::index::watcher::IndexWatcher;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::models::installer::EmbedderInstaller;
use wilkes_core::embed::worker::manager::{
    ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus,
};
use wilkes_core::embed::{dispatch, Embedder};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::path::is_under;
use wilkes_core::types::{
    EmbedderModel, FileEntry, IndexStatus, IndexingConfig, PreviewData, SearchMode, SearchQuery,
    SelectedEmbedder, SemanticSettings, Settings,
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
    pub operation: EmbedOperation,
    pub cancel: CancellationToken,
    pub cancel_flag: Arc<AtomicBool>,
    pub join: JoinHandle<anyhow::Result<()>>,
}

#[derive(Copy, Clone)]
pub enum EmbedOperation {
    Download,
    Build,
}

impl EmbedOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Build => "Build",
        }
    }
}

#[derive(Clone, Debug)]
struct BuildIndexPlan {
    root_path: PathBuf,
    device: String,
    chunk_size: usize,
    chunk_overlap: usize,
    supported_extensions: Vec<String>,
}

#[derive(Clone, Debug)]
struct DownloadModelPlan {
    device: String,
}

#[derive(Clone, Debug)]
struct RestoreStatePlan {
    settings: Settings,
    db_status: IndexStatus,
    selected: SelectedEmbedder,
    device: String,
}

enum RestoreStatePreparation {
    Ready(RestoreStatePlan),
    ResetStaleSelection {
        db_status: IndexStatus,
        selected: SelectedEmbedder,
    },
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
    shutting_down: AtomicBool,
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
    ) -> (
        Arc<Self>,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (worker_manager, event_rx, loop_fut) = WorkerManager::new(paths);
        let ctx = Arc::new(Self {
            data_dir,
            settings_path,
            embedder: PLMutex::new(None),
            index: PLMutex::new(Arc::new(Mutex::new(None))),
            watcher: PLMutex::new(None),
            embed_task: PLMutex::new(None),
            shutting_down: AtomicBool::new(false),
            worker_manager,
            events,
            settings_lock: tokio::sync::Mutex::new(()),
        });
        (ctx, event_rx, loop_fut)
    }

    /// Spawns the required background tasks for the application context.
    pub fn spawn_background_tasks(
        self: Arc<Self>,
        event_rx: mpsc::Receiver<ManagerEvent>,
        loop_fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
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
        self.events.emit(
            "embed-error",
            serde_json::json!({
                "operation": operation,
                "message": message,
            }),
        );
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

    fn embed_task_is_running(&self) -> bool {
        let guard = self.embed_task.lock();
        guard.as_ref().is_some_and(|task| !task.join.is_finished())
    }

    fn clear_embed_task(&self) {
        *self.embed_task.lock() = None;
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    pub async fn update_semantic_settings<F>(&self, f: F)
    where
        F: FnOnce(SemanticSettings) -> SemanticSettings,
    {
        let _lock = self.settings_lock.lock().await;
        let current = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("update_semantic_settings: read: {e:#}");
                return;
            }
        };
        let semantic = f(current.semantic);
        if let Err(e) = update_settings(
            &self.settings_path,
            serde_json::json!({ "semantic": semantic }),
        )
        .await
        {
            error!("update_semantic_settings: write: {e:#}");
        }
    }

    pub async fn update_settings(
        &self,
        patch: serde_json::Value,
    ) -> anyhow::Result<wilkes_core::types::Settings> {
        let _lock = self.settings_lock.lock().await;
        update_settings(&self.settings_path, patch).await
    }

    pub fn is_semantic_ready(&self) -> bool {
        self.embedder.lock().is_some()
            && self
                .index
                .lock()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Resolve semantic state (if needed) and start the search. Handles both
    /// Grep and Semantic modes; callers do not branch on mode.
    pub async fn start_search(
        self: Arc<Self>,
        mut query: SearchQuery,
    ) -> Result<SearchHandle, String> {
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

            let embedder = self
                .embedder
                .lock()
                .clone()
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
                    let engine = {
                        let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
                        guard
                            .as_ref()
                            .map(|idx| idx.status().engine)
                            .unwrap_or_default()
                    };
                    let selected = SelectedEmbedder {
                        engine,
                        model: EmbedderModel(embedder.model_id().to_string()),
                        dimension: embedder.dimension(),
                    };
                    let ctx = Arc::clone(&self);
                    let root_str = query_root_canonical.to_string_lossy().to_string();
                    tokio::spawn(async move {
                        if let Err(e) = ctx.start_build_index(root_str, selected).await {
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
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        let plan = self.prepare_build_index(&root, &selected).await?;

        self.stop_watcher();
        self.events
            .emit("manager-event", serde_json::json!("Reindexing"));

        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join = Arc::clone(&self).spawn_build_index_task(
            plan,
            selected,
            cancel.clone(),
            Arc::clone(&cancel_flag),
        );

        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag,
            join,
        });
        Ok(())
    }

    // ── Download model ────────────────────────────────────────────────────────

    /// Download a model in the background and load it into state on success.
    pub async fn start_download_model(
        self: Arc<Self>,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        let plan = self.prepare_download_model(&selected).await?;
        let join = Arc::clone(&self).spawn_download_model_task(plan, selected);

        let cancel = CancellationToken::new();
        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Download,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });
        Ok(())
    }

    async fn prepare_build_index(
        &self,
        root: &str,
        selected: &SelectedEmbedder,
    ) -> Result<BuildIndexPlan, String> {
        if self.embed_task_is_running() {
            return Err("A build is already in progress.".into());
        }

        let root_path = PathBuf::from(root);
        if !root_path.exists() {
            return Err(format!("Index root not found: {}", root_path.display()));
        }
        if !root_path.is_dir() {
            return Err(format!(
                "Index root is not a directory: {}",
                root_path.display()
            ));
        }

        let settings = self.settings().await;
        Ok(BuildIndexPlan {
            root_path,
            device: settings.semantic.device_for(selected.engine).to_string(),
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
        })
    }

    async fn prepare_download_model(
        &self,
        selected: &SelectedEmbedder,
    ) -> Result<DownloadModelPlan, String> {
        if self.embed_task_is_running() {
            return Err("A build is already in progress.".into());
        }

        let settings = self.settings().await;
        Ok(DownloadModelPlan {
            device: settings.semantic.device_for(selected.engine).to_string(),
        })
    }

    fn build_index_options(
        manager: WorkerManager,
        data_dir: PathBuf,
        plan: &BuildIndexPlan,
        progress_tx: tokio::sync::mpsc::Sender<EmbedProgress>,
        cancel_flag: Arc<AtomicBool>,
    ) -> crate::commands::embed::BuildIndexOptions {
        crate::commands::embed::BuildIndexOptions {
            manager: Some(manager),
            device: Some(plan.device.clone()),
            data_dir,
            tx: progress_tx,
            cancel_flag,
            chunk_size: plan.chunk_size,
            chunk_overlap: plan.chunk_overlap,
            supported_extensions: plan.supported_extensions.clone(),
        }
    }

    fn cleanup_partial_index_files(data_dir: &Path) {
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
    }

    fn emit_progress_event(&self, progress: &EmbedProgress) {
        self.events.emit(
            "embed-progress",
            serde_json::to_value(progress).unwrap_or_default(),
        );
    }

    async fn open_built_index(
        &self,
        data_dir: PathBuf,
        model_id: String,
        dim: usize,
    ) -> Result<SemanticIndex, String> {
        match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir, &model_id, dim))
            .await
        {
            Ok(Ok(index)) => Ok(index),
            Ok(Err(err)) => Err(err.to_string()),
            Err(err) => Err(err.to_string()),
        }
    }

    fn start_build_watcher(
        &self,
        root_path: PathBuf,
        index_arc: Arc<Mutex<Option<SemanticIndex>>>,
        embedder: Arc<dyn Embedder>,
        indexing: IndexingConfig,
    ) {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        let ev1 = Arc::clone(&self.events);
        let ev2 = Arc::clone(&self.events);
        match IndexWatcher::start(
            root_path,
            index_arc,
            Arc::new(registry),
            embedder,
            indexing,
            move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
            move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
        ) {
            Ok(watcher) => *self.watcher.lock() = Some(watcher),
            Err(err) => error!("watcher start failed: {err:#}"),
        }
    }

    async fn finish_build_index(
        self: &Arc<Self>,
        plan: &BuildIndexPlan,
        selected: &SelectedEmbedder,
        data_dir: &Path,
        embedder: Arc<dyn Embedder>,
    ) -> Result<(), String> {
        let dim = embedder.dimension();
        let model_id = selected.model.model_id().to_string();
        let index = self
            .open_built_index(data_dir.to_path_buf(), model_id, dim)
            .await?;
        let actual_dim = index.status().dimension;
        let index_arc = Arc::new(Mutex::new(Some(index)));

        *self.embedder.lock() = Some(Arc::clone(&embedder));
        *self.index.lock() = Arc::clone(&index_arc);

        self.start_build_watcher(
            plan.root_path.clone(),
            index_arc,
            embedder,
            IndexingConfig {
                chunk_size: plan.chunk_size,
                chunk_overlap: plan.chunk_overlap,
                supported_extensions: plan.supported_extensions.clone(),
            },
        );

        self.update_semantic_settings(|s| SemanticSettings {
            index_path: Some(data_dir.join("semantic_index.db")),
            selected: SelectedEmbedder {
                dimension: actual_dim,
                ..selected.clone()
            },
            enabled: true,
            ..s
        })
        .await;

        self.events
            .emit("embed-done", serde_json::json!({ "operation": "Build" }));
        Ok(())
    }

    async fn forward_embed_progress(
        events: Arc<dyn EventEmitter>,
        mut progress_rx: mpsc::Receiver<EmbedProgress>,
    ) {
        while let Some(progress) = progress_rx.recv().await {
            events.emit(
                "embed-progress",
                serde_json::to_value(&progress).unwrap_or_default(),
            );
        }
    }

    fn spawn_build_index_task(
        self: Arc<Self>,
        plan: BuildIndexPlan,
        selected: SelectedEmbedder,
        cancel: CancellationToken,
        cancel_flag: Arc<AtomicBool>,
    ) -> JoinHandle<anyhow::Result<()>> {
        let manager = self.worker_manager.clone();
        let data_dir = self.data_dir.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<EmbedProgress>(128);
        let cancel_for_task = cancel.clone();

        tokio::spawn(async move {
            // Always emit ReindexingDone when this task exits.
            struct DoneGuard(Arc<dyn EventEmitter>);
            impl Drop for DoneGuard {
                fn drop(&mut self) {
                    self.0
                        .emit("manager-event", serde_json::json!("ReindexingDone"));
                }
            }
            let _guard = DoneGuard(Arc::clone(&ctx.events));

            let options = Self::build_index_options(
                manager.clone(),
                data_dir.clone(),
                &plan,
                progress_tx,
                Arc::clone(&cancel_flag),
            );
            let build_fut = crate::commands::embed::build_index(
                plan.root_path.clone(),
                selected.clone(),
                options,
            );
            tokio::pin!(build_fut);
            let mut progress_rx = progress_rx;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel_for_task.cancelled() => {
                        cancel_flag.store(true, Ordering::Relaxed);
                        Self::cleanup_partial_index_files(&data_dir);
                        ctx.emit_embed_error("Build", "");
                        return Ok(());
                    }

                    res = &mut build_fut => {
                        match res {
                            Ok(embedder) => {
                                if let Err(err) = ctx
                                    .finish_build_index(&plan, &selected, &data_dir, embedder)
                                    .await
                                {
                                    ctx.emit_embed_error("Build", err);
                                }
                            }
                            Err(e) => {
                                ctx.emit_embed_error("Build", e.to_string());
                            }
                        }
                        break;
                    }

                    Some(p) = progress_rx.recv() => {
                        ctx.emit_progress_event(&p);
                    }
                }
            }
            ctx.clear_embed_task();
            Ok(())
        })
    }

    fn spawn_download_model_task(
        self: Arc<Self>,
        plan: DownloadModelPlan,
        selected: SelectedEmbedder,
    ) -> JoinHandle<anyhow::Result<()>> {
        let data_dir = self.data_dir.clone();
        let manager = self.worker_manager.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = mpsc::channel::<EmbedProgress>(64);

        tokio::spawn(async move {
            let forward = tokio::spawn(Self::forward_embed_progress(
                Arc::clone(&ctx.events),
                progress_rx,
            ));

            let result = crate::commands::embed::download_model(
                selected.clone(),
                manager.clone(),
                plan.device.clone(),
                data_dir.clone(),
                progress_tx,
            )
            .await;

            let _ = forward.await;

            match result {
                Ok(()) => {
                    if let Err(e) = ctx
                        .probe_and_load_downloaded_model(
                            selected.clone(),
                            manager,
                            plan.device.clone(),
                        )
                        .await
                    {
                        ctx.emit_embed_error("Download", e);
                    } else {
                        ctx.events
                            .emit("embed-done", serde_json::json!({ "operation": "Download" }));
                    }
                }
                Err(e) => {
                    ctx.emit_embed_error("Download", e.to_string());
                }
            }

            ctx.clear_embed_task();
            Ok(())
        })
    }

    async fn probe_and_load_downloaded_model(
        self: &Arc<Self>,
        selected: SelectedEmbedder,
        manager: WorkerManager,
        device: String,
    ) -> Result<(), String> {
        let installer =
            dispatch::get_installer(selected.engine, selected.model.clone(), manager, device);
        self.probe_and_load_downloaded_model_with(installer).await
    }

    async fn probe_and_load_downloaded_model_with(
        self: &Arc<Self>,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Result<(), String> {
        // Probe model dimensions by running install again (no-op if cached).
        let (probe_tx, _) = mpsc::channel(1);
        if let Err(e) = installer.install(&self.data_dir, probe_tx).await {
            return Err(format!("Failed to probe model dimensions: {e:#}"));
        }

        match installer.build(&self.data_dir) {
            Ok(embedder) => {
                *self.embedder.lock() = Some(embedder);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    // ── Embed lifecycle ───────────────────────────────────────────────────────

    pub fn cancel_embed(&self) {
        self.worker_manager.kill_active();
        if let Some(task) = self.embed_task.lock().take() {
            task.cancel_flag.store(true, Ordering::Relaxed);
            task.cancel.cancel();
            self.emit_embed_error(task.operation.as_str(), "");
            task.join.abort();
        }
    }

    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop_watcher();
        self.cancel_embed();
        self.kill_worker();
    }

    pub async fn delete_index(&self) -> anyhow::Result<()> {
        self.stop_watcher();
        *self.index.lock() = Arc::new(Mutex::new(None));
        *self.embedder.lock() = None;
        crate::commands::embed::delete_index(&self.data_dir).await?;
        self.update_semantic_settings(|s| SemanticSettings {
            index_path: None,
            ..s
        })
        .await;
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
        self.worker_manager
            .send(ManagerCommand::SetTimeout(secs))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    // ── Startup restore ───────────────────────────────────────────────────────

    fn restore_state_needs_reset(settings: &Settings, db_status: Option<&IndexStatus>) -> bool {
        match db_status {
            None => settings.semantic.enabled || settings.semantic.index_path.is_some(),
            Some(db_status) => {
                let selected = &settings.semantic.selected;
                db_status.engine != selected.engine
                    || db_status.model_id != selected.model.model_id()
            }
        }
    }

    fn restore_state_indexing_config(settings: &Settings) -> IndexingConfig {
        IndexingConfig {
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
        }
    }

    fn restore_state_enabled_settings(
        current: SemanticSettings,
        db_path: PathBuf,
        selected: SelectedEmbedder,
        dim: usize,
    ) -> SemanticSettings {
        SemanticSettings {
            enabled: true,
            index_path: Some(db_path),
            selected: SelectedEmbedder {
                dimension: dim,
                ..selected
            },
            ..current
        }
    }

    fn open_semantic_index_with<F>(
        data_dir: &PathBuf,
        model_id: &str,
        expected_dim: usize,
        open: F,
    ) -> anyhow::Result<SemanticIndex>
    where
        F: FnOnce(&PathBuf, &str, usize) -> anyhow::Result<SemanticIndex>,
    {
        open(data_dir, model_id, expected_dim)
    }

    fn start_index_watcher_with<F>(
        root: PathBuf,
        index_arc: Arc<Mutex<Option<SemanticIndex>>>,
        registry: Arc<ExtractorRegistry>,
        embedder: Arc<dyn Embedder>,
        indexing: IndexingConfig,
        start: F,
    ) -> anyhow::Result<IndexWatcher>
    where
        F: FnOnce(
            PathBuf,
            Arc<Mutex<Option<SemanticIndex>>>,
            Arc<ExtractorRegistry>,
            Arc<dyn Embedder>,
            IndexingConfig,
            Box<dyn Fn() + Send + Sync>,
            Box<dyn Fn() + Send + Sync>,
        ) -> anyhow::Result<IndexWatcher>,
    {
        let ev1: Box<dyn Fn() + Send + Sync> = Box::new(|| {});
        let ev2: Box<dyn Fn() + Send + Sync> = Box::new(|| {});
        start(root, index_arc, registry, embedder, indexing, ev1, ev2)
    }

    fn prepare_restore_state_plan(
        settings: Settings,
        db_status: IndexStatus,
    ) -> RestoreStatePreparation {
        let selected = settings.semantic.selected.clone();
        if Self::restore_state_needs_reset(&settings, Some(&db_status)) {
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            }
        } else {
            RestoreStatePreparation::Ready(RestoreStatePlan {
                device: settings.semantic.device_for(selected.engine).to_string(),
                settings,
                db_status,
                selected,
            })
        }
    }

    async fn clear_restore_state_settings(&self) {
        self.update_semantic_settings(|s| SemanticSettings {
            enabled: false,
            index_path: None,
            ..s
        })
        .await;
    }

    async fn load_restore_db_status(&self, settings: &Settings) -> Option<IndexStatus> {
        match tokio::task::spawn_blocking({
            let d = self.data_dir.clone();
            move || SemanticIndex::read_status_from_path(&d)
        })
        .await
        {
            Ok(Ok(status)) => Some(status),
            Ok(Err(err)) => {
                info!("restore_state: no index DB ({err:#}), nothing to restore");
                if Self::restore_state_needs_reset(settings, None) {
                    self.clear_restore_state_settings().await;
                }
                None
            }
            Err(err) => {
                error!("restore_state: spawn_blocking panicked: {err}");
                None
            }
        }
    }

    async fn restore_embedder(
        &self,
        selected: &SelectedEmbedder,
        device: String,
    ) -> Option<Arc<dyn Embedder>> {
        let installer = dispatch::get_installer(
            selected.engine,
            selected.model.clone(),
            self.worker_manager.clone(),
            device,
        );
        self.restore_embedder_with(installer).await
    }

    async fn restore_embedder_with(
        &self,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Option<Arc<dyn Embedder>> {
        let (probe_tx, _) = tokio::sync::mpsc::channel(1);
        if let Err(err) = installer.install(&self.data_dir, probe_tx).await {
            error!("restore_state: install probe failed: {err:#}");
            return None;
        }
        if !installer.is_available(&self.data_dir) {
            info!("restore_state: model files absent, skipping");
            return None;
        }

        let data_dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || installer.build(&data_dir)).await {
            Ok(Ok(embedder)) => Some(embedder),
            Ok(Err(err)) => {
                error!("restore_state: build embedder: {err:#}");
                None
            }
            Err(err) => {
                error!("restore_state: build embedder panicked: {err}");
                None
            }
        }
    }

    async fn restore_index(
        &self,
        selected: &SelectedEmbedder,
        expected_dim: usize,
    ) -> Option<SemanticIndex> {
        let data_dir = self.data_dir.clone();
        let model_id = selected.model.model_id().to_string();
        match tokio::task::spawn_blocking(move || {
            Self::open_semantic_index_with(&data_dir, &model_id, expected_dim, |dir, model, dim| {
                SemanticIndex::open(dir, model, dim)
            })
        })
        .await
        {
            Ok(Ok(index)) => Some(index),
            Ok(Err(err)) => {
                error!("restore_state: open index: {err:#}");
                None
            }
            Err(err) => {
                error!("restore_state: open index panicked: {err}");
                None
            }
        }
    }

    fn restore_store_loaded_state(
        &self,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) -> Arc<Mutex<Option<SemanticIndex>>> {
        *self.embedder.lock() = Some(Arc::clone(&embedder));
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);
        index_arc
    }

    fn maybe_restore_watcher(
        &self,
        settings: &Settings,
        index_arc: Arc<Mutex<Option<SemanticIndex>>>,
        embedder: Arc<dyn Embedder>,
    ) {
        if let Some(root) = settings.last_directory.clone() {
            let mut registry = ExtractorRegistry::new();
            registry.register(Box::new(PdfExtractor::new()));
            let indexing = Self::restore_state_indexing_config(settings);
            match Self::start_index_watcher_with(
                root,
                index_arc,
                Arc::new(registry),
                embedder,
                indexing,
                |root, index_arc, registry, embedder, indexing, on_reindex, on_done| {
                    let ev1 = Arc::clone(&self.events);
                    let ev2 = Arc::clone(&self.events);
                    let on_reindex = move || {
                        on_reindex();
                        ev1.emit("manager-event", serde_json::json!("Reindexing"))
                    };
                    let on_done = move || {
                        on_done();
                        ev2.emit("manager-event", serde_json::json!("ReindexingDone"))
                    };
                    IndexWatcher::start(
                        root, index_arc, registry, embedder, indexing, on_reindex, on_done,
                    )
                    .map_err(Into::into)
                },
            ) {
                Ok(watcher) => *self.watcher.lock() = Some(watcher),
                Err(err) => error!("restore_state: watcher: {err:#}"),
            }
        }
    }

    async fn finish_restore_state(
        &self,
        plan: &RestoreStatePlan,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) {
        let index_arc = self.restore_store_loaded_state(Arc::clone(&embedder), index);
        self.maybe_restore_watcher(&plan.settings, index_arc, embedder);

        let db_path = self.data_dir.join("semantic_index.db");
        let dim = plan.db_status.dimension;
        self.update_semantic_settings(|s| {
            Self::restore_state_enabled_settings(s, db_path.clone(), plan.selected.clone(), dim)
        })
        .await;

        info!("restore_state: embedder and index restored");
    }

    /// Reload the embedder and index from disk if they were previously built,
    /// and restart the filesystem watcher. Run this once after `new`.
    pub async fn restore_state(self: Arc<Self>) {
        let settings = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("restore_state: read settings: {e:#}");
                return;
            }
        };

        let Some(db_status) = self.load_restore_db_status(&settings).await else {
            return;
        };

        let plan = match Self::prepare_restore_state_plan(settings, db_status) {
            RestoreStatePreparation::Ready(plan) => plan,
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                info!(
                    "restore_state: index selection '{:?}/{}' != settings selection '{:?}/{}', clearing stale index reference",
                    db_status.engine, db_status.model_id, selected.engine, selected.model.model_id()
                );
                self.clear_restore_state_settings().await;
                return;
            }
        };

        let Some(embedder) = self
            .restore_embedder(&plan.selected, plan.device.clone())
            .await
        else {
            return;
        };

        let Some(index) = self
            .restore_index(&plan.selected, embedder.dimension())
            .await
        else {
            return;
        };

        self.finish_restore_state(&plan, embedder, index).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tracing::subscriber;
    use tracing_subscriber::prelude::*;
    use wilkes_core::embed::MockEmbedder;
    use wilkes_core::types::EmbeddingEngine;
    use wilkes_core::types::{
        EmbedderModel, IndexStatus, SearchMode, SelectedEmbedder, SemanticSettings, Settings, Theme,
    };

    struct MockEmitter {
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }
    impl EventEmitter for MockEmitter {
        fn emit(&self, name: &str, payload: Value) {
            self.events
                .lock()
                .unwrap()
                .push((name.to_string(), payload));
        }
    }

    struct FakeInstaller {
        install_calls: Arc<AtomicUsize>,
        build_calls: Arc<AtomicUsize>,
        available: bool,
        install_should_fail: bool,
        build_should_fail: bool,
    }

    #[async_trait::async_trait]
    impl EmbedderInstaller for FakeInstaller {
        fn is_available(&self, _data_dir: &Path) -> bool {
            self.available
        }

        async fn install(
            &self,
            _data_dir: &Path,
            _tx: mpsc::Sender<EmbedProgress>,
        ) -> anyhow::Result<()> {
            self.install_calls.fetch_add(1, Ordering::Relaxed);
            if self.install_should_fail {
                Err(anyhow::anyhow!("install failed"))
            } else {
                Ok(())
            }
        }

        fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
            Ok(())
        }

        fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
            self.build_calls.fetch_add(1, Ordering::Relaxed);
            if self.build_should_fail {
                Err(anyhow::anyhow!("build failed"))
            } else {
                Ok(Arc::new(MockEmbedder::default()))
            }
        }
    }

    fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (ctx, _rx, _loop) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        (dir, ctx)
    }

    #[test]
    fn test_emit_embed_error_logs_and_emits() {
        wilkes_core::logging::clear_logs();

        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
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
        assert!(logs
            .iter()
            .any(|line| line.contains("Build failed: Worker error")));

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"] == "Worker error"
        }));
    }

    #[test]
    fn test_embed_operation_as_str() {
        assert_eq!(EmbedOperation::Download.as_str(), "Download");
        assert_eq!(EmbedOperation::Build.as_str(), "Build");
    }

    #[test]
    fn test_restore_state_needs_reset_on_missing_db() {
        let settings = Settings {
            bookmarked_dirs: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder::default_for(EmbeddingEngine::Candle),
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            supported_extensions: vec![],
            max_results: 0,
        };

        assert!(AppContext::restore_state_needs_reset(&settings, None));
    }

    #[test]
    fn test_restore_state_needs_reset_on_mismatch() {
        let settings = Settings {
            bookmarked_dirs: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            supported_extensions: vec![],
            max_results: 0,
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Candle,
            model_id: "model-b".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        assert!(AppContext::restore_state_needs_reset(
            &settings,
            Some(&db_status)
        ));
    }

    #[test]
    fn test_restore_state_needs_reset_false_when_matching() {
        let settings = Settings {
            bookmarked_dirs: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            supported_extensions: vec![],
            max_results: 0,
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Fastembed,
            model_id: "model-a".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        assert!(!AppContext::restore_state_needs_reset(
            &settings,
            Some(&db_status)
        ));
    }

    #[test]
    fn test_restore_state_indexing_config() {
        let settings = Settings {
            supported_extensions: vec!["txt".to_string(), "md".to_string()],
            semantic: SemanticSettings {
                chunk_size: 128,
                chunk_overlap: 32,
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };

        let indexing = AppContext::restore_state_indexing_config(&settings);
        assert_eq!(indexing.chunk_size, 128);
        assert_eq!(indexing.chunk_overlap, 32);
        assert_eq!(indexing.supported_extensions, vec!["txt", "md"]);
    }

    #[test]
    fn test_restore_state_enabled_settings() {
        let current = SemanticSettings {
            enabled: false,
            index_path: None,
            ..SemanticSettings::default()
        };
        let selected = SelectedEmbedder {
            engine: EmbeddingEngine::Fastembed,
            model: EmbedderModel("model-a".to_string()),
            dimension: 384,
        };
        let updated = AppContext::restore_state_enabled_settings(
            current,
            PathBuf::from("semantic_index.db"),
            selected,
            768,
        );

        assert!(updated.enabled);
        assert_eq!(updated.index_path, Some(PathBuf::from("semantic_index.db")));
        assert_eq!(updated.selected.dimension, 768);
    }

    #[test]
    fn test_open_semantic_index_with_error() {
        let dir = tempdir().unwrap();
        let result = AppContext::open_semantic_index_with(
            &dir.path().to_path_buf(),
            "model-a",
            384,
            |_dir, _model_id, _dim| Err(anyhow::anyhow!("open failed")),
        );

        match result {
            Ok(_) => panic!("expected open error"),
            Err(err) => assert!(err.to_string().contains("open failed")),
        }
    }

    #[test]
    fn test_start_index_watcher_with_error() {
        let dir = tempdir().unwrap();
        let index_arc = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder = Arc::new(MockEmbedder::default());

        let result = AppContext::start_index_watcher_with(
            dir.path().to_path_buf(),
            index_arc,
            registry,
            embedder,
            IndexingConfig {
                chunk_size: 64,
                chunk_overlap: 16,
                supported_extensions: vec!["txt".to_string()],
            },
            |_root, _index_arc, _registry, _embedder, _indexing, _on_reindex, _on_done| {
                Err(anyhow::anyhow!("watcher failed"))
            },
        );

        match result {
            Ok(_) => panic!("expected watcher error"),
            Err(err) => assert!(err.to_string().contains("watcher failed")),
        }
    }

    fn running_embed_task() -> EmbedTaskHandle {
        EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel: CancellationToken::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join: tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                Ok(())
            }),
        }
    }

    #[tokio::test]
    async fn test_embed_task_helpers_track_state() {
        let (_dir, ctx) = test_ctx();
        assert!(!ctx.embed_task_is_running());

        *ctx.embed_task.lock() = Some(running_embed_task());
        assert!(ctx.embed_task_is_running());

        ctx.clear_embed_task();
        assert!(!ctx.embed_task_is_running());
        assert!(ctx.embed_task.lock().is_none());
    }

    #[tokio::test]
    async fn test_prepare_build_index_happy_path() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let plan = ctx
            .prepare_build_index(&root.to_string_lossy(), &selected)
            .await
            .unwrap();

        assert_eq!(plan.root_path, root);
        assert_eq!(
            plan.chunk_size,
            ctx.get_settings().await.semantic.chunk_size
        );
        assert_eq!(
            plan.chunk_overlap,
            ctx.get_settings().await.semantic.chunk_overlap
        );
        assert_eq!(
            plan.supported_extensions,
            ctx.get_settings().await.supported_extensions
        );
        assert_eq!(
            plan.device,
            ctx.get_settings()
                .await
                .semantic
                .device_for(EmbeddingEngine::Candle)
                .to_string()
        );
    }

    #[tokio::test]
    async fn test_prepare_build_index_rejects_running_task() {
        let (_dir, ctx) = test_ctx();
        *ctx.embed_task.lock() = Some(running_embed_task());

        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let err = ctx
            .prepare_build_index("/tmp", &selected)
            .await
            .unwrap_err();

        assert!(err.contains("already in progress"));
    }

    #[tokio::test]
    async fn test_prepare_build_index_validates_root_path() {
        let (_dir, ctx) = test_ctx();
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);

        let missing = ctx
            .prepare_build_index("/definitely/missing/path", &selected)
            .await
            .unwrap_err();
        assert!(missing.contains("Index root not found"));

        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("not_a_dir");
        std::fs::write(&file_path, "hello").unwrap();
        let not_dir = ctx
            .prepare_build_index(&file_path.to_string_lossy(), &selected)
            .await
            .unwrap_err();
        assert!(not_dir.contains("not a directory"));
    }

    #[tokio::test]
    async fn test_prepare_download_model_happy_path_and_running_guard() {
        let (_dir, ctx) = test_ctx();
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);

        let plan = ctx.prepare_download_model(&selected).await.unwrap();
        assert_eq!(
            plan.device,
            ctx.get_settings()
                .await
                .semantic
                .device_for(EmbeddingEngine::Candle)
                .to_string()
        );

        *ctx.embed_task.lock() = Some(running_embed_task());
        let err = ctx.prepare_download_model(&selected).await.unwrap_err();
        assert!(err.contains("already in progress"));
    }

    #[test]
    fn test_build_index_options_and_cleanup_partial_files() {
        let dir = tempdir().unwrap();
        let plan = BuildIndexPlan {
            root_path: dir.path().join("root"),
            device: "cpu".to_string(),
            chunk_size: 123,
            chunk_overlap: 45,
            supported_extensions: vec!["rs".to_string(), "txt".to_string()],
        };
        let (tx, _rx) = mpsc::channel(1);
        let options = AppContext::build_index_options(
            WorkerManager::new(WorkerPaths {
                python_path: PathBuf::from("python"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("reqs.txt"),
                venv_dir: PathBuf::from("venv"),
                worker_bin: PathBuf::from("worker"),
                data_dir: dir.path().to_path_buf(),
            })
            .0,
            dir.path().to_path_buf(),
            &plan,
            tx,
            Arc::new(AtomicBool::new(false)),
        );

        assert_eq!(options.device.as_deref(), Some("cpu"));
        assert_eq!(options.chunk_size, 123);
        assert_eq!(options.chunk_overlap, 45);
        assert_eq!(options.supported_extensions, vec!["rs", "txt"]);

        for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
            std::fs::write(dir.path().join(format!("semantic_index.db{suffix}")), "x").unwrap();
        }
        AppContext::cleanup_partial_index_files(dir.path());
        for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
            assert!(!dir
                .path()
                .join(format!("semantic_index.db{suffix}"))
                .exists());
        }
    }

    #[tokio::test]
    async fn test_emit_progress_helpers() {
        let dir = tempdir().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: Arc::clone(&captured),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );
        let progress = EmbedProgress::Build(wilkes_core::embed::installer::IndexBuildProgress {
            files_processed: 1,
            total_files: 2,
            message: "building".to_string(),
            done: false,
        });
        ctx.emit_progress_event(&progress);

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "embed-progress");
    }

    #[tokio::test]
    async fn test_forward_embed_progress_emits_events() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let emitter: Arc<dyn EventEmitter> = Arc::new(MockEmitter {
            events: Arc::clone(&captured),
        });
        let (tx, rx) = mpsc::channel(2);

        let forward = tokio::spawn(AppContext::forward_embed_progress(Arc::clone(&emitter), rx));
        tx.send(EmbedProgress::Download(
            wilkes_core::embed::installer::DownloadProgress {
                bytes_received: 3,
                total_bytes: 9,
                done: false,
            },
        ))
        .await
        .unwrap();
        drop(tx);
        forward.await.unwrap();

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "embed-progress");
    }

    #[tokio::test]
    async fn test_open_built_index_error_and_store_loaded_state() {
        let (dir, ctx) = test_ctx();
        let err = match ctx
            .open_built_index(dir.path().to_path_buf(), "missing-model".to_string(), 384)
            .await
        {
            Ok(_) => panic!("expected open_built_index to fail"),
            Err(err) => err,
        };
        assert!(!err.is_empty());

        let data_dir = ctx.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir,
            "mock-model",
            384,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let index_arc = ctx.restore_store_loaded_state(Arc::clone(&embedder), index);

        assert!(ctx.embedder.lock().is_some());
        assert!(ctx.index.lock().lock().unwrap().is_some());
        assert!(index_arc.lock().unwrap().is_some());
    }

    #[test]
    fn test_prepare_restore_state_plan_variants() {
        let matching = Settings {
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Candle,
            model_id: "model-a".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        match AppContext::prepare_restore_state_plan(matching.clone(), db_status.clone()) {
            RestoreStatePreparation::Ready(plan) => {
                assert_eq!(plan.selected.model.model_id(), "model-a");
                assert_eq!(plan.db_status.model_id, "model-a");
            }
            RestoreStatePreparation::ResetStaleSelection { .. } => panic!("expected ready plan"),
        }

        let mismatched = Settings {
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-b".to_string()),
                    dimension: 384,
                },
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        match AppContext::prepare_restore_state_plan(mismatched, db_status) {
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                assert_eq!(db_status.model_id, "model-a");
                assert_eq!(selected.model.model_id(), "model-b");
            }
            RestoreStatePreparation::Ready(_) => panic!("expected stale-selection reset"),
        }
    }

    #[tokio::test]
    async fn test_restore_embedder_with_installer_branches() {
        let (_dir, ctx) = test_ctx();

        let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: true,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(install_fail).await.is_none());

        let unavailable: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: false,
            install_should_fail: false,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(unavailable).await.is_none());

        let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: true,
        });
        assert!(ctx.restore_embedder_with(build_fail).await.is_none());

        let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(ok).await.is_some());
    }

    #[tokio::test]
    async fn test_probe_and_load_downloaded_model_with_installer_branches() {
        let (_dir, ctx) = test_ctx();

        let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: true,
            build_should_fail: false,
        });
        let err = ctx
            .probe_and_load_downloaded_model_with(install_fail)
            .await
            .unwrap_err();
        assert!(err.contains("Failed to probe model dimensions"));

        let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: true,
        });
        let err = ctx
            .probe_and_load_downloaded_model_with(build_fail)
            .await
            .unwrap_err();
        assert!(err.contains("build failed"));

        let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: false,
        });
        ctx.probe_and_load_downloaded_model_with(ok).await.unwrap();
        assert!(ctx.embedder.lock().is_some());
    }

    #[tokio::test]
    async fn test_maybe_restore_watcher_and_finish_restore_state() {
        let (dir, ctx) = test_ctx();
        let data_dir = ctx.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir,
            "restore-model",
            384,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        let index_arc = Arc::new(Mutex::new(Some(index)));
        let bad_root = dir.path().join("not-a-dir.txt");
        std::fs::write(&bad_root, "nope").unwrap();
        let settings = Settings {
            last_directory: Some(bad_root),
            supported_extensions: vec!["txt".to_string()],
            semantic: SemanticSettings {
                chunk_size: 64,
                chunk_overlap: 8,
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        ctx.maybe_restore_watcher(&settings, index_arc, Arc::clone(&embedder));
        ctx.stop_watcher();
        assert!(ctx.watcher.lock().is_none());

        let (_dir2, ctx2) = test_ctx();
        let data_dir2 = ctx2.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir2,
            "restore-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        let plan = RestoreStatePlan {
            settings: Settings::default(),
            db_status: IndexStatus {
                indexed_files: 1,
                total_chunks: 1,
                built_at: None,
                build_duration_ms: None,
                engine: EmbeddingEngine::Candle,
                model_id: "restore-model".to_string(),
                dimension: 384,
                root_path: None,
                db_size_bytes: None,
            },
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("restore-model".to_string()),
                dimension: 1,
            },
            device: "cpu".to_string(),
        };
        ctx2.finish_restore_state(&plan, embedder, index).await;

        let settings = ctx2.get_settings().await;
        assert!(settings.semantic.enabled);
        assert_eq!(settings.semantic.selected.dimension, 384);
    }

    #[tokio::test]
    async fn test_app_context_new() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (ctx, _event_rx, _loop_fut) =
            AppContext::new(data_dir, settings_path.clone(), paths, emitter);

        ctx.update_semantic_settings(|s| SemanticSettings {
            enabled: true,
            chunk_size: 1234,
            ..s
        })
        .await;

        let updated = get_settings(&settings_path).await.unwrap();
        assert_eq!(updated.semantic.enabled, true);
        assert_eq!(updated.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_event_forwarder() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
            IndexingConfig {
                chunk_size: 100,
                chunk_overlap: 10,
                supported_extensions: vec![],
            },
            || {},
            || {},
        )
        .unwrap();

        *ctx.watcher.lock() = Some(watcher);
        ctx.stop_watcher();
        assert!(ctx.watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_is_semantic_ready() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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

        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        *ctx.embedder.lock() = Some(embedder);
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "semantic-ready",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
        assert!(ctx.is_semantic_ready());
    }

    #[tokio::test]
    async fn test_cancel_embed() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
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

        let cancel = CancellationToken::new();
        let join = tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        ctx.cancel_embed(); // Should not panic
        assert!(ctx.embed_task.lock().is_none());

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error" && payload["operation"] == "Build" && payload["message"] == ""
        }));
    }

    #[tokio::test]
    async fn test_shutdown() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
            IndexingConfig {
                chunk_size: 100,
                chunk_overlap: 10,
                supported_extensions: vec![],
            },
            || {},
            || {},
        )
        .unwrap();
        *ctx.watcher.lock() = Some(watcher);

        let cancel = CancellationToken::new();
        let join = tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        ctx.shutdown();

        assert!(ctx.watcher.lock().is_none());
        assert!(ctx.embed_task.lock().is_none());
    }

    #[tokio::test]
    async fn test_delete_index() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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

        let _updated_semantic = ctx
            .update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
            .await;

        // Settings should have been saved to disk
        let disk_content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        assert!(disk_content.contains("\"context_lines\": 5"));
        assert!(disk_content.contains("\"enabled\": true"));
    }

    #[tokio::test]
    async fn test_file_operations() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        tokio::fs::write(root.join("test.txt"), "hello")
            .await
            .unwrap();
        tokio::fs::write(root.join("test.pdf"), "fake pdf")
            .await
            .unwrap();
        tokio::fs::create_dir(root.join("subdir")).await.unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let res = ctx
            .start_build_index(
                "root".to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_start_build_index_root_not_found() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let res = ctx
            .start_build_index(
                "/non/existent/path/for/sure/12345".to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Index root not found"));
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_start_search_semantic_build_in_progress() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let res = ctx
            .start_download_model(SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            })
            .await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_open_file_denied() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
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
        let idx = SemanticIndex::create(
            &data_dir,
            &model_id,
            dimension,
            EmbeddingEngine::Candle,
            Some(&root1),
        )
        .unwrap();
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

        let mut saw_reindex = false;
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
            let events_guard = events.lock().unwrap();
            if events_guard
                .iter()
                .any(|e| e.0 == "manager-event" && e.1 == serde_json::json!("Reindexing"))
            {
                saw_reindex = true;
                break;
            }
        }
        assert!(saw_reindex);
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
                selected: SelectedEmbedder {
                    model: EmbedderModel("model-A".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        tokio::fs::write(&settings_path, serde_json::to_string(&settings).unwrap())
            .await
            .unwrap();

        // Write index status with model B
        SemanticIndex::create(&data_dir, "model-B", 1, EmbeddingEngine::Candle, None).unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
                selected: SelectedEmbedder {
                    model: EmbedderModel("model-A".to_string()),
                    ..Default::default()
                },
                enabled: true,
                index_path: Some(data_dir.join("semantic_index.db")),
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        wilkes_core::embed::index::SemanticIndex::create(
            &data_dir,
            "model-B",
            1,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        std::fs::write(
            data_dir.join("semantic_index.status.json"),
            r#"{"model_id": "m", "dimension": 1, "engine": "Candle"}"#,
        )
        .unwrap();

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
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
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
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
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
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        ctx.update_semantic_settings(|mut s| {
            s.chunk_size = 1234;
            s
        })
        .await;

        let settings = ctx.get_settings().await;
        assert_eq!(settings.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_cancel_embed_operation() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
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
        ctx.clone()
            .start_build_index(
                dir.path().to_string_lossy().to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await
            .unwrap();

        // Immediately cancel
        ctx.cancel_embed();

        assert!(ctx.embed_task.lock().is_none());
        assert!(!ctx.get_worker_status().active);
    }

    #[tokio::test]
    async fn test_start_download_model_error() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
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
        ctx.clone()
            .start_download_model(SelectedEmbedder {
                engine: EmbeddingEngine::Fastembed,
                model: EmbedderModel("invalid-model".to_string()),
                dimension: 384,
            })
            .await
            .unwrap();

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
        initial_settings.semantic.selected.model = EmbedderModel("m1".to_string());
        std::fs::write(
            &settings_path,
            serde_json::to_string(&initial_settings).unwrap(),
        )
        .unwrap();

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
        std::fs::write(
            index_dir.join("status.json"),
            serde_json::to_string(&status).unwrap(),
        )
        .unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        // AppContext::new might have already called restore_state internally,
        // but let's be explicit and check if it sticks.
        ctx.clone().restore_state().await;

        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.selected.model.0, "m1");
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
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
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
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        ctx.update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
            .await;

        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.enabled, true);
        assert!(settings_path.exists());
    }

    #[tokio::test]
    async fn test_worker_status_timeout_and_delete_index_wrapper() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });

        let (ctx, _event_rx, loop_fut) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            emitter,
        );
        let _loop_handle = tokio::spawn(loop_fut);

        let status = ctx.get_worker_status();
        assert!(!status.active);
        assert_eq!(status.timeout_secs, 300);

        ctx.set_worker_timeout(123).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let status = ctx.get_worker_status();
        assert_eq!(status.timeout_secs, 123);

        let index = SemanticIndex::create(
            &data_dir,
            "test-model",
            3,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        drop(index);

        let index_path = data_dir.join("semantic_index.db");
        assert!(index_path.exists());

        let status = ctx.get_index_status().await.unwrap();
        assert_eq!(status.model_id, "test-model");

        ctx.delete_index().await.unwrap();
        assert!(!index_path.exists());
    }
}
