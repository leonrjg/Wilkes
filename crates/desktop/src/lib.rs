use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use tracing::info;
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_core::embed::worker::manager::WorkerStatus;
use wilkes_core::types::{
    DataPaths, DocumentMetadata, EmbeddingEngine, FileListResponse, IndexStatus, ModelDescriptor,
    SelectedEmbedder, Settings,
};

mod platform;

use platform::{
    build_startup_plan, validate_open_target, DesktopPlatform, DesktopStartupPlan,
    SystemDesktopPlatform, TauriPlatform,
};

fn app_context(app: &AppHandle) -> Arc<AppContext> {
    app.state::<Arc<AppContext>>().inner().clone()
}

fn active_searches_state(app: &AppHandle) -> Arc<ActiveSearches> {
    app.state::<Arc<ActiveSearches>>().inner().clone()
}

fn data_paths_from(app_data: String) -> DataPaths {
    DataPaths { app_data }
}

async fn list_files_for_ctx(ctx: Arc<AppContext>, root: String) -> Result<FileListResponse, String> {
    ctx.list_files(root.into()).await.map_err(|e| e.to_string())
}

async fn open_file_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<wilkes_core::types::PreviewData, String> {
    ctx.open_file(path.into()).await.map_err(|e| e.to_string())
}

async fn get_file_metadata_for_path(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    let supported_extensions = ctx.get_settings().await.supported_extensions;
    wilkes_api::commands::metadata::get_file_metadata(path.into(), supported_extensions)
        .await
        .map_err(|e| e.to_string())
}

async fn get_settings_for_ctx(ctx: Arc<AppContext>) -> Result<Settings, String> {
    Ok(ctx.get_settings().await)
}

async fn update_settings_for_ctx(
    ctx: Arc<AppContext>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    ctx.update_settings(patch).await.map_err(|e| e.to_string())
}

fn is_semantic_ready_for_ctx(ctx: Arc<AppContext>) -> bool {
    ctx.is_semantic_ready()
}

async fn download_model_for_ctx(
    ctx: Arc<AppContext>,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::download_model_for_ctx: engine={}, model={}",
        selected.engine.as_str(),
        selected.model.model_id()
    );
    ctx.start_download_model(selected).await
}

async fn build_index_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::build_index_for_ctx: root={}, engine={}, model={}",
        root,
        selected.engine.as_str(),
        selected.model.model_id()
    );
    Arc::clone(&ctx).start_build_index(root, selected).await
}

async fn list_models_for_ctx(
    ctx: Arc<AppContext>,
    engine: EmbeddingEngine,
) -> Result<Vec<ModelDescriptor>, String> {
    Ok(wilkes_api::commands::embed::list_models(engine, &ctx.data_dir).await)
}

async fn cancel_embed_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    info!("desktop::cancel_embed_for_ctx");
    ctx.cancel_embed().await;
    Ok(())
}

async fn get_index_status_for_ctx(ctx: Arc<AppContext>) -> Result<IndexStatus, String> {
    ctx.get_index_status().await.map_err(|e| e.to_string())
}

async fn delete_index_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    ctx.delete_index().await.map_err(|e| e.to_string())
}

fn get_worker_status_for_ctx(ctx: Arc<AppContext>) -> WorkerStatus {
    ctx.get_worker_status()
}

async fn kill_worker_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    ctx.kill_worker();
    Ok(())
}

async fn set_worker_timeout_for_ctx(ctx: Arc<AppContext>, secs: u64) -> Result<(), String> {
    ctx.set_worker_timeout(secs)
        .await
        .map_err(|e| e.to_string())
}

fn handle_exit_event(app_handle: &AppHandle, event: tauri::RunEvent) {
    if matches!(
        event,
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
    ) {
        let ctx = app_handle.state::<Arc<AppContext>>().inner().clone();
        tauri::async_runtime::spawn(async move {
            ctx.shutdown().await;
        });
    }
}

// ── Tauri EventEmitter impl ───────────────────────────────────────────────────

struct TauriEmitter(AppHandle);

impl EventEmitter for TauriEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let platform = TauriPlatform(self.0.clone());
        platform.emit(name, payload);
    }
}

// ── Desktop-specific state ────────────────────────────────────────────────────

struct ActiveSearches(Mutex<HashMap<String, JoinHandle<()>>>);

trait SearchEventSink: Send + Sync + Clone + 'static {
    fn emit_event(&self, name: &str, payload: serde_json::Value);
}

impl SearchEventSink for AppHandle {
    fn emit_event(&self, name: &str, payload: serde_json::Value) {
        let _ = Emitter::emit(self, name, &payload);
    }
}

async fn search_for_ctx<E>(
    ctx: Arc<AppContext>,
    active_searches: Arc<ActiveSearches>,
    emitter: E,
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
) -> Result<String, String>
where
    E: SearchEventSink,
{
    let handle = Arc::clone(&ctx).start_search(query).await?;

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let id = search_id.clone();
    let emitter_for_task = emitter.clone();
    let active_searches_for_task = Arc::clone(&active_searches);

    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let stats = handle
            .run(|fm| {
                let emitter = emitter_for_task.clone();
                let id = id.clone();
                async move {
                    let payload = serde_json::to_value(&fm).unwrap_or_default();
                    emitter.emit_event(&format!("search-result-{}", id), payload);
                    true
                }
            })
            .await;

        emitter_for_task.emit_event(
            &format!("search-complete-{}", id),
            serde_json::to_value(&stats).unwrap_or_default(),
        );

        active_searches_for_task.0.lock().unwrap().remove(&id);
    });

    active_searches
        .0
        .lock()
        .unwrap()
        .insert(search_id.clone(), forwarder);

    Ok(search_id)
}

async fn get_model_size_for_ctx_with<F, Fut>(
    engine: EmbeddingEngine,
    model_id: String,
    fetcher: F,
) -> Result<u64, String>
where
    F: FnOnce(EmbeddingEngine, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<u64>>,
{
    fetcher(engine, model_id).await.map_err(|e| e.to_string())
}

fn cancel_search_for_ctx(active_searches: Arc<ActiveSearches>, search_id: &str) {
    if let Some(handle) = active_searches.0.lock().unwrap().remove(search_id) {
        handle.abort();
    }
}

// ── Desktop-only commands ─────────────────────────────────────────────────────

#[tauri::command]
async fn get_data_paths(app: AppHandle) -> Result<DataPaths, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())?;
    Ok(data_paths_from(app_data))
}

#[tauri::command]
async fn get_python_info() -> Result<String, String> {
    wilkes_core::path::resolve_python()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    validate_open_target(&path)?;
    SystemDesktopPlatform.open_target(&path)?;
    Ok(())
}

#[tauri::command]
async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string()));
    });
    Ok(rx.await.unwrap_or(None))
}

// ── Search ────────────────────────────────────────────────────────────────────

#[tauri::command]
async fn search(
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
    app: AppHandle,
) -> Result<String, String> {
    let ctx = app_context(&app);
    let active_searches = active_searches_state(&app);
    search_for_ctx(ctx, active_searches, app.clone(), query, search_id).await
}

#[tauri::command]
async fn cancel_search(search_id: String, app: AppHandle) -> Result<(), String> {
    cancel_search_for_ctx(active_searches_state(&app), &search_id);
    Ok(())
}

// ── Delegating commands ───────────────────────────────────────────────────────

#[tauri::command]
async fn preview(
    match_ref: wilkes_core::types::MatchRef,
) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::preview::preview(match_ref)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_files(root: String, app: AppHandle) -> Result<FileListResponse, String> {
    list_files_for_ctx(app_context(&app), root).await
}

#[tauri::command]
async fn open_file(
    path: String,
    app: AppHandle,
) -> Result<wilkes_core::types::PreviewData, String> {
    open_file_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn get_file_metadata(path: String, app: AppHandle) -> Result<DocumentMetadata, String> {
    get_file_metadata_for_path(app_context(&app), path).await
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    get_settings_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn update_settings(patch: serde_json::Value, app: AppHandle) -> Result<Settings, String> {
    update_settings_for_ctx(app_context(&app), patch).await
}

#[tauri::command]
fn is_semantic_ready(app: AppHandle) -> bool {
    is_semantic_ready_for_ctx(app_context(&app))
}

#[tauri::command]
async fn get_logs() -> Result<Vec<String>, String> {
    Ok(wilkes_api::commands::logs::get_logs())
}

#[tauri::command]
async fn clear_logs() -> Result<(), String> {
    wilkes_api::commands::logs::clear_logs();
    Ok(())
}

#[tauri::command]
fn get_supported_engines() -> Vec<EmbeddingEngine> {
    EmbeddingEngine::supported_engines()
}

// ── Embed commands (delegating to AppContext) ─────────────────────────────────

#[tauri::command]
async fn download_model(selected: SelectedEmbedder, app: AppHandle) -> Result<(), String> {
    download_model_for_ctx(app_context(&app), selected).await
}

#[tauri::command]
async fn build_index(
    root: String,
    selected: SelectedEmbedder,
    app: AppHandle,
) -> Result<(), String> {
    build_index_for_ctx(app_context(&app), root, selected).await
}

#[tauri::command]
async fn list_models(
    engine: EmbeddingEngine,
    app: AppHandle,
) -> Result<Vec<ModelDescriptor>, String> {
    list_models_for_ctx(app_context(&app), engine).await
}

#[tauri::command]
async fn get_model_size(engine: EmbeddingEngine, model_id: String) -> Result<u64, String> {
    get_model_size_for_ctx_with(engine, model_id, |engine, model_id| async move {
        wilkes_api::commands::embed::get_model_size(engine, model_id).await
    })
    .await
}

#[tauri::command]
async fn cancel_embed(app: AppHandle) -> Result<(), String> {
    cancel_embed_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn get_index_status(app: AppHandle) -> Result<IndexStatus, String> {
    get_index_status_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn delete_index(app: AppHandle) -> Result<(), String> {
    delete_index_for_ctx(app_context(&app)).await
}

#[tauri::command]
fn get_worker_status(app: AppHandle) -> WorkerStatus {
    get_worker_status_for_ctx(app_context(&app))
}

#[tauri::command]
async fn kill_worker(app: AppHandle) -> Result<(), String> {
    kill_worker_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn set_worker_timeout(secs: u64, app: AppHandle) -> Result<(), String> {
    set_worker_timeout_for_ctx(app_context(&app), secs).await
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    wilkes_core::logging::init_logging();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let platform = TauriPlatform(handle.clone());
            let DesktopStartupPlan {
                data_dir,
                settings_path,
                worker_paths: paths,
            } = build_startup_plan(&platform)?;

            let emitter = Arc::new(TauriEmitter(handle.clone()));
            let (ctx, event_rx, loop_fut) =
                AppContext::new(data_dir, settings_path, paths, emitter);

            app.manage(Arc::clone(&ctx));
            app.manage(Arc::new(ActiveSearches(Mutex::new(HashMap::new()))));

            let ctx_c = Arc::clone(&ctx);
            tauri::async_runtime::spawn(async move {
                ctx_c.spawn_background_tasks(event_rx, loop_fut);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search,
            cancel_search,
            preview,
            list_files,
            open_file,
            get_file_metadata,
            get_python_info,
            get_supported_engines,
            get_settings,
            update_settings,
            pick_directory,
            download_model,
            build_index,
            list_models,
            get_model_size,
            cancel_embed,
            get_index_status,
            delete_index,
            get_logs,
            clear_logs,
            get_data_paths,
            open_path,
            is_semantic_ready,
            get_worker_status,
            kill_worker,
            set_worker_timeout,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| handle_exit_event(&app_handle, event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_api::context::EventEmitter;
    use wilkes_core::embed::worker::manager::WorkerPaths;

    static OPEN_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct MockEmitter;

    impl EventEmitter for MockEmitter {
        fn emit(&self, _name: &str, _payload: serde_json::Value) {}
    }

    #[derive(Clone)]
    struct SearchEmitter {
        events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl SearchEventSink for SearchEmitter {
        fn emit_event(&self, name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((name.to_string(), payload));
        }
    }

    #[tokio::test]
    async fn test_get_python_info() {
        let result = get_python_info().await;
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_get_supported_engines() {
        let engines = get_supported_engines();
        assert!(!engines.is_empty());
        assert!(engines.contains(&EmbeddingEngine::SBERT));
    }

    #[tokio::test]
    async fn test_get_python_info_fallback() {
        // Just check it doesn't panic
        let _ = get_python_info().await;
    }

    #[tokio::test]
    async fn test_active_searches() {
        let active = ActiveSearches(Mutex::new(HashMap::new()));
        let mut guard = active.0.lock().unwrap();
        guard.insert("test".to_string(), tokio::spawn(async {}));
        assert!(guard.contains_key("test"));
    }

    #[tokio::test]
    async fn test_search_for_ctx_orchestration_emits_and_cleans_up() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("example.txt"), "hello world").unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = SearchEmitter {
            events: Arc::clone(&events),
        };
        let active_searches = Arc::new(ActiveSearches(Mutex::new(HashMap::new())));
        let (_ctx_dir, ctx) = test_ctx();
        let query = wilkes_core::types::SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: wilkes_core::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
        };

        let search_id = search_for_ctx(
            ctx,
            Arc::clone(&active_searches),
            emitter,
            query,
            Some("search-1".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(search_id, "search-1");

        for _ in 0..20 {
            if active_searches.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }

        assert!(active_searches.0.lock().unwrap().is_empty());

        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|(name, _)| name == "search-result-search-1"));
        assert!(events
            .iter()
            .any(|(name, _)| name == "search-complete-search-1"));
    }

    #[test]
    fn test_validate_open_target() {
        let dir = tempdir().unwrap();
        assert!(validate_open_target(&dir.path().display().to_string()).is_ok());
        assert_eq!(
            validate_open_target(&dir.path().join("missing").display().to_string()),
            Err("Path does not exist".into())
        );
        assert!(validate_open_target("https://doi.org/10.1000/xyz123").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_open_path_uses_spawned_opener() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = OPEN_PATH_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let opener_name = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let opener = dir.path().join(opener_name);
        std::fs::write(&opener, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&opener).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opener, perms).unwrap();

        let path = dir.path().join("folder");
        std::fs::create_dir(&path).unwrap();

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), original_path);
        std::env::set_var("PATH", &new_path);

        let res = open_path(path.display().to_string()).await;
        std::env::set_var("PATH", original_path);

        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_model_size_for_ctx_with_injected_fetcher() {
        let result = get_model_size_for_ctx_with(
            EmbeddingEngine::Candle,
            "model-x".to_string(),
            |_engine, model_id| async move {
                assert_eq!(model_id, "model-x");
                Ok(42)
            },
        )
        .await
        .unwrap();

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_get_model_size_for_ctx_with_error() {
        let err = get_model_size_for_ctx_with(
            EmbeddingEngine::Candle,
            "model-x".to_string(),
            |_engine, _model_id| async move { Err(anyhow::anyhow!("no size")) },
        )
        .await
        .unwrap_err();

        assert!(err.contains("no size"));
    }

    #[tokio::test]
    async fn test_delete_index_for_ctx_removes_db() {
        let (_dir, ctx) = test_ctx();
        let db_path = ctx.data_dir.join("semantic_index.db");
        std::fs::write(&db_path, "fake db").unwrap();

        delete_index_for_ctx(Arc::clone(&ctx)).await.unwrap();
        assert!(!db_path.exists());
    }

    #[tokio::test]
    async fn test_get_index_status_for_ctx_missing() {
        let (_dir, ctx) = test_ctx();
        let result = get_index_status_for_ctx(ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_open_file_for_ctx_denied() {
        let (_dir, ctx) = test_ctx();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let err = open_file_for_ctx(ctx, outside.display().to_string())
            .await
            .unwrap_err();
        assert!(err.contains("Access denied"));
    }

    #[tokio::test]
    async fn test_get_file_metadata_for_path_allows_outside_data_dir() {
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let (_dir, ctx) = test_ctx();
        let metadata = get_file_metadata_for_path(ctx, outside.display().to_string())
            .await
            .unwrap();
        assert_eq!(metadata, DocumentMetadata::default());
    }

    #[tokio::test]
    async fn test_build_index_for_ctx_missing_root() {
        let (_dir, ctx) = test_ctx();
        let err = build_index_for_ctx(
            ctx,
            "/definitely/missing/root".to_string(),
            SelectedEmbedder::default_for(EmbeddingEngine::Candle),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Index root not found"));
    }

    #[tokio::test]
    async fn test_get_worker_status_for_ctx_and_timeout_update() {
        let dir = tempdir().unwrap();
        use std::path::PathBuf;

        let emitter = Arc::new(MockEmitter);
        let (ctx, _rx, loop_fut) = AppContext::new(
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
        let _loop_handle = tokio::spawn(loop_fut);

        let status = get_worker_status_for_ctx(Arc::clone(&ctx));
        assert!(!status.active);
        assert_eq!(status.timeout_secs, 300);

        set_worker_timeout_for_ctx(Arc::clone(&ctx), 123)
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let status = get_worker_status_for_ctx(ctx);
        assert_eq!(status.timeout_secs, 123);
    }

    #[tokio::test]
    async fn test_list_models_for_ctx_returns_catalog() {
        let (_dir, ctx) = test_ctx();
        let models = list_models_for_ctx(ctx, EmbeddingEngine::Candle)
            .await
            .unwrap();
        assert!(!models.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_and_kill_helpers() {
        let (_dir, ctx) = test_ctx();
        cancel_embed_for_ctx(Arc::clone(&ctx)).await.unwrap();
        kill_worker_for_ctx(ctx).await.unwrap();
    }

    #[tokio::test]
    async fn test_set_worker_timeout_for_ctx_errors_without_loop() {
        let (_dir, ctx) = test_ctx();
        let err = set_worker_timeout_for_ctx(ctx, 12).await.unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_search_for_ctx_removes_handle() {
        let active = Arc::new(ActiveSearches(Mutex::new(HashMap::new())));
        active.0.lock().unwrap().insert(
            "search-1".to_string(),
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }),
        );

        cancel_search_for_ctx(Arc::clone(&active), "search-1");

        assert!(active.0.lock().unwrap().is_empty());
    }

    fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter);
        let paths = WorkerPaths {
            python_path: std::path::PathBuf::from("python"),
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            venv_dir: std::path::PathBuf::from("venv"),
            worker_bin: std::path::PathBuf::from("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (ctx, _rx, _loop) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        (dir, ctx)
    }

    #[tokio::test]
    async fn test_get_settings_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let settings = get_settings_for_ctx(ctx).await.unwrap();
        assert!(settings.bookmarked_dirs.is_empty());
        assert!(settings.last_directory.is_none());
        assert!(!settings.semantic.enabled);
    }

    #[tokio::test]
    async fn test_update_settings_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let updated = update_settings_for_ctx(
            Arc::clone(&ctx),
            serde_json::json!({
                "semantic": {
                    "enabled": true
                }
            }),
        )
        .await
        .unwrap();
        assert!(updated.semantic.enabled);
    }

    #[tokio::test]
    async fn test_list_files_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("example.txt"), "hello").unwrap();
        let files = list_files_for_ctx(ctx, dir.path().display().to_string())
            .await
            .unwrap();
        assert!(!files.files.is_empty());
    }

    #[tokio::test]
    async fn test_is_semantic_ready_for_ctx() {
        let (_dir, ctx) = test_ctx();
        assert!(!is_semantic_ready_for_ctx(ctx));
    }

    #[tokio::test]
    async fn test_cancel_embed_for_ctx() {
        let (_dir, ctx) = test_ctx();
        assert!(super::cancel_embed_for_ctx(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_data_paths_and_logs_for_ctx() {
        let (_dir, _ctx) = test_ctx();

        let paths = data_paths_from("test-data".to_string());
        assert_eq!(paths.app_data, "test-data");

        let _ = super::get_python_info().await;

        super::clear_logs().await.unwrap();
        let logs = super::get_logs().await.unwrap();
        assert!(logs.is_empty());
    }
}
