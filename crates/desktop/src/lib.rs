use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_core::embed::worker::manager::WorkerPaths;
use wilkes_core::embed::worker::manager::WorkerStatus;
use wilkes_core::types::{
    DataPaths, EmbedderModel, EmbeddingEngine, FileEntry, IndexStatus, ModelDescriptor,
    Settings,
};

// ── Platform helpers ──────────────────────────────────────────────────────────

fn desktop_settings_path() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
    Ok(config.join("wilkes").join("settings.json"))
}

// ── Tauri EventEmitter impl ───────────────────────────────────────────────────

struct TauriEmitter(AppHandle);

impl EventEmitter for TauriEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let _ = Emitter::emit(&self.0, name, &payload);
    }
}

// ── Desktop-specific state ────────────────────────────────────────────────────

struct ActiveSearches(Mutex<HashMap<String, JoinHandle<()>>>);

// ── Desktop-only commands ─────────────────────────────────────────────────────

#[tauri::command]
async fn get_data_paths(app: AppHandle) -> Result<DataPaths, String> {
    let app_data = app.path().app_data_dir()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())?;
    let hf_cache = wilkes_core::embed::models::hf_cache::get_hf_cache_root().display().to_string();
    Ok(DataPaths { hf_cache, app_data })
}

#[tauri::command]
async fn get_python_info() -> Result<String, String> {
    wilkes_core::path::resolve_python().map(|p| p.display().to_string()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err("Path does not exist".into());
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(&p).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer").arg(&p).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(&p).spawn().map_err(|e| e.to_string())?;
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
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    let handle = Arc::clone(&ctx).start_search(query).await?;

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let id = search_id.clone();
    let app_clone = app.clone();

    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let stats = handle.run(|fm| {
            let app_clone = app_clone.clone();
            let id = id.clone();
            async move {
                let _ = app_clone.emit(&format!("search-result-{}", id), &fm);
                true
            }
        }).await;

        let _ = app_clone.emit(&format!("search-complete-{}", id), &stats);

        app_clone.state::<ActiveSearches>().0.lock().unwrap().remove(&id);
    });

    app.state::<ActiveSearches>().0.lock().unwrap().insert(search_id.clone(), forwarder);
    Ok(search_id)
}

#[tauri::command]
async fn cancel_search(search_id: String, app: AppHandle) -> Result<(), String> {
    if let Some(h) = app.state::<ActiveSearches>().0.lock().unwrap().remove(&search_id) {
        h.abort();
    }
    Ok(())
}

// ── Delegating commands ───────────────────────────────────────────────────────

#[tauri::command]
async fn preview(match_ref: wilkes_core::types::MatchRef) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::preview::preview(match_ref).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_files(root: String, app: AppHandle) -> Result<Vec<FileEntry>, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.list_files(root.into()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_file(path: String, app: AppHandle) -> Result<wilkes_core::types::PreviewData, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.open_file(path.into()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    Ok(ctx.get_settings().await)
}

#[tauri::command]
async fn update_settings(patch: serde_json::Value, app: AppHandle) -> Result<Settings, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.update_settings(patch).await.map_err(|e| e.to_string())
}

#[tauri::command]
fn is_semantic_ready(app: AppHandle) -> bool {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.is_semantic_ready()
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
async fn download_model(model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.start_download_model(model, engine).await
}

#[tauri::command]
async fn build_index(root: String, model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    Arc::clone(&ctx).start_build_index(root, model, engine).await
}

#[tauri::command]
async fn list_models(engine: EmbeddingEngine, app: AppHandle) -> Result<Vec<ModelDescriptor>, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    Ok(wilkes_api::commands::embed::list_models(engine, &ctx.data_dir).await)
}

#[tauri::command]
async fn get_model_size(engine: EmbeddingEngine, model_id: String) -> Result<u64, String> {
    wilkes_api::commands::embed::get_model_size(engine, model_id)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn cancel_embed(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.cancel_embed();
    Ok(())
}

#[tauri::command]
async fn get_index_status(app: AppHandle) -> Result<IndexStatus, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.get_index_status().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_index(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.delete_index().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_worker_status(app: AppHandle) -> Result<WorkerStatus, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.get_worker_status().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn kill_worker(app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.kill_worker();
    Ok(())
}

#[tauri::command]
async fn set_worker_timeout(secs: u64, app: AppHandle) -> Result<(), String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    ctx.set_worker_timeout(secs).await.map_err(|e| e.to_string())
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    wilkes_core::logging::init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();

            let data_dir = handle.path().app_data_dir()?;
            let settings_path = desktop_settings_path()?;
            let paths = WorkerPaths::resolve(&data_dir);

            let emitter = Arc::new(TauriEmitter(handle.clone()));
            let (ctx, event_rx, loop_fut) = AppContext::new(data_dir, settings_path, paths, emitter);

            app.manage(Arc::clone(&ctx));
            app.manage(ActiveSearches(Mutex::new(HashMap::new())));

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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_desktop_settings_path() {
        let result = desktop_settings_path();
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.ends_with("wilkes/settings.json") || path.ends_with("wilkes\\settings.json"));
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
    async fn test_logs_commands() {
        clear_logs().await.unwrap();
        let logs = get_logs().await.unwrap();
        assert!(logs.is_empty());
    }
}
