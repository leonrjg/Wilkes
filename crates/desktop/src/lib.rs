use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use tracing::error;
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_core::embed::worker_manager::WorkerPaths;
use wilkes_core::embed::worker_manager::WorkerStatus;
use wilkes_core::types::{
    EmbedderModel, EmbeddingEngine, FileEntry, IndexStatus, ModelDescriptor, SearchStats,
    Settings,
};

// ── Platform helpers ──────────────────────────────────────────────────────────

fn desktop_settings_path() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
    Ok(config.join("wilkes").join("settings.json"))
}

fn resolve_python() -> anyhow::Result<std::path::PathBuf> {
    let mut attempted = Vec::new();

    let exe = std::env::current_exe()?;
    let bundled = if cfg!(target_os = "macos") {
        exe.parent().and_then(|p| p.parent())
            .map(|p| p.join("Resources").join("python").join("bin").join("python3"))
    } else if cfg!(target_os = "windows") {
        exe.parent().map(|p| p.join("python").join("python.exe"))
    } else {
        exe.parent().and_then(|p| p.parent())
            .map(|p| p.join("lib").join("python").join("bin").join("python3"))
    };

    if let Some(ref p) = bundled {
        attempted.push(p.clone());
        if p.exists() {
            return Ok(p.clone());
        }
    }

    let name = if cfg!(target_os = "windows") { "python.exe" } else { "python3" };
    let which = if cfg!(target_os = "windows") { "where" } else { "which" };
    if let Ok(out) = std::process::Command::new(which).arg(name).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                let p = std::path::PathBuf::from(s);
                attempted.push(p.clone());
                return Ok(p);
            }
        }
    }

    let mut msg = "Python interpreter not found. Tried:\n".to_string();
    for p in attempted {
        msg.push_str(&format!("- {}\n", p.display()));
    }
    anyhow::bail!("{}", msg);
}

fn resolve_python_package_dir(app: &AppHandle) -> anyhow::Result<std::path::PathBuf> {
    let resource_dir = app.path().resource_dir()?;
    let candidates = [resource_dir.clone(), resource_dir.join("_up_").join("worker")];
    candidates.into_iter()
        .find(|p| p.join("wilkes_python_worker").is_dir())
        .ok_or_else(|| anyhow::anyhow!(
            "Python worker package not found in {}", resource_dir.display()
        ))
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

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct DataPaths {
    hf_cache: String,
    app_data: String,
}

#[tauri::command]
async fn get_data_paths(app: AppHandle) -> Result<DataPaths, String> {
    let app_data = app.path().app_data_dir()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())?;
    let hf_cache = wilkes_core::embed::hf_cache::get_hf_cache_root().display().to_string();
    Ok(DataPaths { hf_cache, app_data })
}

#[tauri::command]
async fn get_python_info() -> Result<String, String> {
    resolve_python().map(|p| p.display().to_string()).map_err(|e| e.to_string())
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
    let mut handle = Arc::clone(&ctx).start_search(query).await?;

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let id = search_id.clone();
    let app_clone = app.clone();

    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let started = Instant::now();
        let mut total_matches = 0usize;
        let mut files_scanned = 0usize;

        while let Some(fm) = handle.next().await {
            total_matches += fm.matches.len();
            files_scanned += 1;
            let _ = app_clone.emit(&format!("search-result-{}", id), &fm);
        }

        let errors = handle.finish().await;
        let stats = SearchStats {
            files_scanned,
            total_matches,
            elapsed_ms: started.elapsed().as_millis() as u64,
            errors,
        };
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
    let settings = wilkes_api::commands::settings::get_settings(&ctx.settings_path)
        .await.unwrap_or_default();
    wilkes_api::commands::files::list_files(root.into(), settings.supported_extensions)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_file(path: String, app: AppHandle) -> Result<wilkes_core::types::PreviewData, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    let settings = wilkes_api::commands::settings::get_settings(&ctx.settings_path)
        .await.unwrap_or_default();
    wilkes_api::commands::files::open_file(path.into(), settings.supported_extensions)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    wilkes_api::commands::settings::get_settings(&ctx.settings_path)
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_settings(patch: serde_json::Value, app: AppHandle) -> Result<Settings, String> {
    let ctx = app.state::<Arc<AppContext>>().inner().clone();
    wilkes_api::commands::settings::update_settings(&ctx.settings_path, patch)
        .await.map_err(|e| e.to_string())
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

            let python_path = resolve_python().unwrap_or_default();
            let python_package_dir = resolve_python_package_dir(&handle).unwrap_or_default();
            let requirements_path = python_package_dir.join("requirements.txt");
            let venv_dir = data_dir.join("sbert_venv");
            let worker_bin = std::env::current_exe()
                .unwrap_or_default()
                .with_file_name("wilkes-rust-worker");

            if !worker_bin.exists() {
                error!("Worker binary NOT FOUND at {}", worker_bin.display());
            }

            let paths = WorkerPaths {
                python_path,
                python_package_dir,
                requirements_path,
                venv_dir,
                worker_bin,
            };

            let emitter = Arc::new(TauriEmitter(handle.clone()));
            let (ctx, event_rx, loop_fut) = AppContext::new(data_dir, settings_path, paths, emitter);

            app.manage(Arc::clone(&ctx));
            app.manage(ActiveSearches(Mutex::new(HashMap::new())));

            tauri::async_runtime::spawn(loop_fut);

            let ctx1 = Arc::clone(&ctx);
            tauri::async_runtime::spawn(async move {
                ctx1.run_event_forwarder(event_rx).await;
            });

            tauri::async_runtime::spawn(async move {
                ctx.restore_state().await;
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
            get_worker_status,
            kill_worker,
            set_worker_timeout,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
