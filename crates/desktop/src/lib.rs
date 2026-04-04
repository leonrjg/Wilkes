use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use wilkes_core::embed::Embedder;
use wilkes_core::embed::dispatch;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::watcher::IndexWatcher;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{
    EmbedderModel, EmbeddingEngine, FileEntry, IndexStatus, MatchRef, ModelDescriptor, SearchMode, SearchQuery,
    SearchStats, SemanticSettings, Settings,
};

fn desktop_settings_path() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
    Ok(config.join("wilkes").join("settings.json"))
}

fn resolve_python() -> anyhow::Result<std::path::PathBuf> {
    let mut attempted_paths = Vec::new();

    // 1. Bundled Python
    let exe = std::env::current_exe()?;
    let bundled_path = if cfg!(target_os = "macos") {
        exe.parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("Resources").join("python").join("bin").join("python3"))
    } else if cfg!(target_os = "windows") {
        exe.parent().map(|p| p.join("python").join("python.exe"))
    } else {
        exe.parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("lib").join("python").join("bin").join("python3"))
    };

    if let Some(ref p) = bundled_path {
        attempted_paths.push(p.clone());
        if p.exists() {
            return Ok(p.clone());
        }
    }

    // 2. System Python
    let system_name = if cfg!(target_os = "windows") { "python.exe" } else { "python3" };
    // Check if system python exists in PATH
    if let Ok(output) = std::process::Command::new(if cfg!(target_os = "windows") { "where" } else { "which" })
        .arg(system_name)
        .output()
    {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path_str.is_empty() {
                let system_path = std::path::PathBuf::from(path_str);
                attempted_paths.push(system_path.clone()); // Add to attempted paths
                return Ok(system_path);
            }
        }
    }

    // If no Python found, generate a detailed error message
    let mut error_message = "Python interpreter not found. Tried the following locations:\n".to_string();
    for path in attempted_paths {
        error_message.push_str(&format!("- {}\n", path.display()));
    }
    error_message.push_str("\nPlease install the bundled version or set up a Python environment.");

    anyhow::bail!("{}", error_message);
}

fn resolve_worker_script(app: &AppHandle) -> anyhow::Result<std::path::PathBuf> {
    let resource_dir = app.path().resource_dir()?;
    // Production bundles flatten resources; dev mode preserves the relative path via _up_/worker/.
    let candidates = [
        resource_dir.join("wilkes_worker.py"),
        resource_dir.join("_up_").join("worker").join("wilkes_worker.py"),
    ];
    candidates
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("Python worker script not found at {}", resource_dir.display()))
}

// ── App state ────────────────────────────────────────────────────────────────

struct ActiveSearches(Mutex<HashMap<String, JoinHandle<()>>>);

/// Tracks the active download or index build so it can be cancelled.
struct EmbedState(Mutex<Option<EmbedTaskHandle>>);

/// The loaded embedder, shared with SemanticSearchProvider via Arc.
/// Only one embedder is live at a time; each model occupies significant memory.
struct ActiveEmbedderState(Mutex<Option<Arc<dyn Embedder>>>);

/// The open index, shared with the watcher and query path.
/// `None` when no index has been built yet.
struct SemanticIndexState(Mutex<Arc<Mutex<Option<SemanticIndex>>>>);

/// The active filesystem watcher. Stopped and replaced when the root changes.
struct WatcherState(Mutex<Option<IndexWatcher>>);

pub struct EmbedTaskHandle {
    cancel: CancellationToken,
    #[allow(dead_code)]
    join: JoinHandle<anyhow::Result<()>>,
}

// ── Event payloads ────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct EmbedDone {
    operation: EmbedOperation,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct EmbedError {
    operation: EmbedOperation,
    message: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
enum EmbedOperation {
    Download,
    Build,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct DataPaths {
    hf_cache: String,
    app_data: String,
}

// ── Tauri commands ───────────────────────────────────────────────────────────

/// Return the resolved data paths for HuggingFace and the application.
#[tauri::command]
async fn get_data_paths(app: AppHandle) -> Result<DataPaths, String> {
    let app_data = app.path().app_data_dir()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())?;
    let hf_cache = wilkes_core::embed::hf_cache::get_hf_cache_root().display().to_string();
    Ok(DataPaths { hf_cache, app_data })
}

/// Open a path in the system file manager.
#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(path);
    if !p.exists() {
        return Err("Path does not exist".into());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(p)
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// Start a search. Returns a `search_id` that identifies this run.
/// Results are emitted as `search-result-{id}` events (payload: FileMatches).
/// A final `search-complete-{id}` event carries SearchStats.
/// The caller may supply a `search_id` that was pre-registered with `listen`
/// before this invocation, preventing a race between event emission and listener
/// registration.
#[tauri::command]
async fn search(query: SearchQuery, search_id: Option<String>, app: AppHandle) -> Result<String, String> {
    // Block if a reindex or download is already running AND it is a semantic search.
    if query.mode == SearchMode::Semantic {
        let embed_state = app.state::<EmbedState>();
        let mut state = embed_state.0.lock().unwrap();
        if let Some(task) = state.as_ref() {
            if !task.join.is_finished() {
                return Err("Semantic index is currently being built or updated. Please wait.".into());
            } else {
                // Task is done but handle is still there, clear it.
                *state = None;
            }
        }
    }

    // For semantic mode, extract the embedder and index from state.
    let embedder = if query.mode == SearchMode::Semantic {
        let state = app.state::<ActiveEmbedderState>();
        let guard = state.0.lock().unwrap();
        match guard.clone() {
            Some(e) => Some(e),
            None => {
                error!("semantic search requested but no embedder is loaded in state");
                return Err("No embedder loaded. Download and install a model first.".into());
            }
        }
    } else {
        None
    };

    let mut index = if query.mode == SearchMode::Semantic {
        Some(app.state::<SemanticIndexState>().0.lock().unwrap().clone())
    } else {
        None
    };

    // Auto-reindex if the search root changed.
    if let (SearchMode::Semantic, Some(idx_arc), Some(embedder)) = (&query.mode, &index, &embedder) {
        let query_root_canonical = std::fs::canonicalize(&query.root).unwrap_or_else(|_| query.root.clone());
        
        let root_mismatch = {
            let guard = idx_arc.lock().unwrap();
            match guard.as_ref() {
                Some(idx) => {
                    let status = idx.status();
                    match status.root_path {
                        Some(p) => {
                            let indexed_root_canonical = std::fs::canonicalize(&p).unwrap_or(p);
                            indexed_root_canonical != query_root_canonical
                        }
                        None => true,
                    }
                }
                None => true,
            }
        };

        if root_mismatch {
            info!("search: root mismatch, triggering reindex of {}", query_root_canonical.display());
            let model = wilkes_core::types::EmbedderModel(embedder.model_id().to_string());
            let engine = {
                let guard = idx_arc.lock().unwrap();
                guard.as_ref().map(|idx| idx.status().engine).unwrap_or(EmbeddingEngine::SBERT)
            };
            
            build_index(query_root_canonical.to_string_lossy().to_string(), model, engine, app.clone()).await?;
            
            // Re-acquire the new index from state after build completes
            index = Some(app.state::<SemanticIndexState>().0.lock().unwrap().clone());
        }
    }

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let handle = wilkes_api::commands::search::start_search(query, embedder, index);

    let app_for_task = app.clone();
    let id = search_id.clone();
    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let mut handle = handle;
        let started = Instant::now();
        let mut total_matches = 0usize;
        let mut files_scanned = 0usize;

        while let Some(file_matches) = handle.next().await {
            total_matches += file_matches.matches.len();
            files_scanned += 1;
            let _ = app_for_task.emit(&format!("search-result-{}", id), &file_matches);
        }

        let errors = handle.finish().await;

        let stats = SearchStats {
            files_scanned,
            total_matches,
            elapsed_ms: started.elapsed().as_millis() as u64,
            errors,
        };
        let _ = app_for_task.emit(&format!("search-complete-{}", id), &stats);

        app_for_task
            .state::<ActiveSearches>()
            .0
            .lock()
            .unwrap()
            .remove(&id);
    });

    app.state::<ActiveSearches>()
        .0
        .lock()
        .unwrap()
        .insert(search_id.clone(), forwarder);

    Ok(search_id)
}

/// Cancel a running search by aborting the forwarder task.
#[tauri::command]
async fn cancel_search(search_id: String, app: AppHandle) -> Result<(), String> {
    if let Some(handle) = app
        .state::<ActiveSearches>()
        .0
        .lock()
        .unwrap()
        .remove(&search_id)
    {
        handle.abort();
    }
    Ok(())
}

/// Return preview data for a specific match.
#[tauri::command]
async fn preview(match_ref: MatchRef) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::preview::preview(match_ref)
        .await
        .map_err(|e| e.to_string())
}

/// Returns the resolved Python interpreter path, or an error describing what was tried.
#[tauri::command]
async fn get_python_info() -> Result<String, String> {
    resolve_python()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

/// Returns the embedding engines compiled into this app build.
#[tauri::command]
fn get_supported_engines() -> Vec<wilkes_core::types::EmbeddingEngine> {
    wilkes_core::types::EmbeddingEngine::supported_engines()
}

/// Load persisted settings (returns defaults if no settings file exists yet).
#[tauri::command]
async fn get_settings() -> Result<Settings, String> {
    let path = desktop_settings_path().map_err(|e| e.to_string())?;
    wilkes_api::commands::settings::get_settings(&path)
        .await
        .map_err(|e| e.to_string())
}

/// Merge a partial settings patch and persist. Returns the full new settings.
#[tauri::command]
async fn update_settings(patch: serde_json::Value) -> Result<Settings, String> {
    let path = desktop_settings_path().map_err(|e| e.to_string())?;
    wilkes_api::commands::settings::update_settings(&path, patch)
        .await
        .map_err(|e| e.to_string())
}

/// List all supported files under a directory (no pattern matching).
#[tauri::command]
async fn list_files(root: String) -> Result<Vec<FileEntry>, String> {
    wilkes_api::commands::files::list_files(root.into())
        .await
        .map_err(|e| e.to_string())
}

/// Open a file for preview at page/line 1 with no highlight.
#[tauri::command]
async fn open_file(path: String) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::files::open_file(path.into())
        .await
        .map_err(|e| e.to_string())
}

/// Open the native folder picker and return the chosen path (or null).
#[tauri::command]
async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string()));
    });
    Ok(rx.await.unwrap_or(None))
}

// ── Embed commands ────────────────────────────────────────────────────────────

/// Download the selected embedding model. Emits `embed-progress`, `embed-done`, or `embed-error`.
#[tauri::command]
async fn download_model(model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let settings_path = desktop_settings_path().map_err(|e| e.to_string())?;
    let current_settings = wilkes_api::commands::settings::get_settings(&settings_path).await
        .unwrap_or_default();
    let device = current_settings.semantic.device.clone();
    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

    let app_clone = app.clone();
    let data_dir_clone = data_dir.clone();
    let model_clone = model.clone();

    let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let forward_app = app_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(progress) = rx.recv().await {
                let _ = forward_app.emit("embed-progress", &progress);
            }
        });

        let result =
            wilkes_api::commands::embed::download_model(engine, model_clone.clone(), manager.clone(), device.clone(), data_dir_clone.clone(), tx).await;

        let _ = forward_handle.await;

        match result {
            Ok(()) => {
                // Load the embedder into state.
                let (tx, _) = tokio::sync::mpsc::channel(1);
                let installer = dispatch::get_installer(engine, model_clone.clone(), manager, device);
                if let Err(e) = installer.install(&data_dir_clone, tx).await {
                    let _ = app_clone.emit(
                        "embed-error",
                        EmbedError {
                            operation: EmbedOperation::Download,
                            message: format!("Failed to probe model dimensions: {e:#}"),
                        },
                    );
                    *app_clone.state::<EmbedState>().0.lock().unwrap() = None;
                    return Ok(());
                }

                match installer.build(&data_dir_clone) {
                    Ok(embedder) => {
                        *app_clone.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(embedder);
                        update_semantic_settings(|s| SemanticSettings { enabled: true, engine, model: model_clone, ..s }).await;
                        let _ = app_clone.emit("embed-done", EmbedDone { operation: EmbedOperation::Download });
                    }
                    Err(e) => {
                        let _ = app_clone.emit(
                            "embed-error",
                            EmbedError {
                                operation: EmbedOperation::Download,
                                message: e.to_string(),
                            },
                        );
                    }
                }
            }
            Err(e) => {
                let _ = app_clone.emit(
                    "embed-error",
                    EmbedError {
                        operation: EmbedOperation::Download,
                        message: e.to_string(),
                    },
                );
            }
        }

        *app_clone.state::<EmbedState>().0.lock().unwrap() = None;
        Ok::<(), anyhow::Error>(())
    });

    let cancel = CancellationToken::new();
    *app.state::<EmbedState>().0.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
    Ok(())
}

/// Build the semantic index for `root` using `model`.
/// Emits `embed-progress`, `embed-done`, or `embed-error`.
#[tauri::command]
async fn build_index(root: String, model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let settings_path = desktop_settings_path().map_err(|e| e.to_string())?;
    let current_settings = wilkes_api::commands::settings::get_settings(&settings_path).await
        .unwrap_or_default();
    let chunk_size = current_settings.semantic.chunk_size;
    let chunk_overlap = current_settings.semantic.chunk_overlap;
    let device = current_settings.semantic.device.clone();

    // Stop the active watcher before rebuilding.
    stop_watcher(&app);

    let _ = app.emit("manager-event", "Reindexing");

    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();

    let (tx, rx) = tokio::sync::mpsc::channel(128);
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();

    let app_clone = app.clone();
    let data_dir_clone = data_dir.clone();
    let root_clone = root.clone();
    let model_clone = model.clone();
    let manager_clone = manager.clone();

    let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        // Ensure we always emit ReindexingDone, even on panic or early return.
        struct DoneGuard(AppHandle);
        impl Drop for DoneGuard {
            fn drop(&mut self) {
                let _ = self.0.emit("manager-event", "ReindexingDone");
            }
        }
        let _guard = DoneGuard(app_clone.clone());

        let build_fut = wilkes_api::commands::embed::build_index(
            root_clone.clone().into(),
            engine,
            model_clone.clone(),
            manager_clone.clone(),
            device.clone(),
            data_dir_clone.clone(),
            tx,
            chunk_size,
            chunk_overlap,
        );

        tokio::pin!(build_fut);

        let mut rx = rx;

        loop {
            tokio::select! {
                biased;
                _ = cancel_for_task.cancelled() => {
                    let _ = std::fs::remove_file(data_dir_clone.join("semantic_index.db"));
                    let _ = app_clone.emit("embed-error", EmbedError {
                        operation: EmbedOperation::Build,
                        message: String::new(), // empty = user-cancelled
                    });
                    return Ok(());
                }
                res = &mut build_fut => {
                    match res {
                        Ok(embedder) => {
                            let dimension = embedder.dimension();
                            *app_clone.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(embedder.clone());
                            
                            let open_result = tokio::task::spawn_blocking({
                                let d = data_dir_clone.clone();
                                let m = model_clone.model_id().to_string();
                                move || SemanticIndex::open(&d, &m, dimension)
                            })
                            .await;

                            match open_result {
                                Ok(Ok(idx)) => {
                                    let actual_dimension = idx.status().dimension;
                                    *app_clone.state::<SemanticIndexState>().0.lock().unwrap() = Arc::new(Mutex::new(Some(idx)));

                                    let index_arc = app_clone.state::<SemanticIndexState>().0.lock().unwrap().clone();
                                    let mut registry = ExtractorRegistry::new();
                                    registry.register(Box::new(wilkes_core::extract::pdf::PdfExtractor::new()));

                                    let watcher_config = if engine == EmbeddingEngine::SBERT {
                                        Some(wilkes_core::embed::watcher::WatcherConfig {
                                            manager: manager_clone,
                                            model_id: model_clone.model_id().to_string(),
                                            data_dir: data_dir_clone.clone(),
                                            device: device.clone(),
                                        })
                                    } else {
                                        None
                                    };

                                    let handle_for_watcher = app_clone.clone();
                                    let handle_for_watcher_done = handle_for_watcher.clone();
                                    match IndexWatcher::start(
                                        root_clone.into(),
                                        index_arc,
                                        Arc::new(registry),
                                        Some(embedder),
                                        watcher_config,
                                        chunk_size,
                                        chunk_overlap,
                                        move || {
                                            let _ = handle_for_watcher.emit("manager-event", "Reindexing");
                                        },
                                        move || {
                                            let _ = handle_for_watcher_done.emit("manager-event", "ReindexingDone");
                                        }
                                    ) {
                                        Ok(watcher) => {
                                            *app_clone.state::<WatcherState>().0.lock().unwrap() = Some(watcher);
                                        }
                                        Err(e) => error!("Failed to start watcher: {e:#}"),
                                    }

                                    let db_path = data_dir_clone.join("semantic_index.db");
                                    update_semantic_settings(|s| SemanticSettings {
                                        index_path: Some(db_path),
                                        dimension: actual_dimension,
                                        enabled: true,
                                        ..s
                                    }).await;
                                    let _ = app_clone.emit("embed-done", EmbedDone { operation: EmbedOperation::Build });
                                }
                                Ok(Err(e)) => {
                                    let _ = app_clone.emit("embed-error", EmbedError {
                                        operation: EmbedOperation::Build,
                                        message: e.to_string(),
                                    });
                                }
                                Err(e) => {
                                    let _ = app_clone.emit("embed-error", EmbedError {
                                        operation: EmbedOperation::Build,
                                        message: e.to_string(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = app_clone.emit("embed-error", EmbedError {
                                operation: EmbedOperation::Build,
                                message: e.to_string(),
                            });
                        }
                    }
                    break;
                }
                event_opt = rx.recv() => {
                    if let Some(p) = event_opt {
                        let _ = app_clone.emit("embed-progress", &p);
                    }
                }
            }
        }

        *app_clone.state::<EmbedState>().0.lock().unwrap() = None;
        Ok(())
    });

    *app.state::<EmbedState>().0.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
    Ok(())
}

/// Return all supported models annotated with local cache availability.
#[tauri::command]
async fn list_models(engine: EmbeddingEngine, app: AppHandle) -> Result<Vec<ModelDescriptor>, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(wilkes_api::commands::embed::list_models(engine, &data_dir).await)
}

/// Fetch the total download size for `model_id` from the HuggingFace API.
#[tauri::command]
async fn get_model_size(engine: EmbeddingEngine, model_id: String) -> Result<u64, String> {
    wilkes_api::commands::embed::get_model_size(engine, model_id)
        .await
        .map_err(|e| e.to_string())
}

/// Cancel the active download or index build.
#[tauri::command]
async fn cancel_embed(app: AppHandle) -> Result<(), String> {
    if let Some(task) = app.state::<EmbedState>().0.lock().unwrap().take() {
        task.cancel.cancel();
        // Don't await the join handle here — let it finish in background.
    }
    Ok(())
}

/// Return the current index status.
#[tauri::command]
async fn get_index_status(app: AppHandle) -> Result<IndexStatus, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    wilkes_api::commands::embed::get_index_status(&data_dir)
        .await
        .map_err(|e| e.to_string())
}

/// Delete the semantic index, clear state, and stop the watcher.
#[tauri::command]
async fn delete_index(app: AppHandle) -> Result<(), String> {
    stop_watcher(&app);
    *app.state::<SemanticIndexState>().0.lock().unwrap() = Arc::new(Mutex::new(None));
    *app.state::<ActiveEmbedderState>().0.lock().unwrap() = None;

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    wilkes_api::commands::embed::delete_index(&data_dir)
        .await
        .map_err(|e| e.to_string())?;
    update_semantic_settings(|s| SemanticSettings { index_path: None, ..s }).await;
    Ok(())
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn stop_watcher(app: &AppHandle) {
    if let Some(mut w) = app.state::<WatcherState>().0.lock().unwrap().take() {
        w.stop();
    }
}

// ── Settings persistence helpers ─────────────────────────────────────────────

/// Read current settings, apply `f` to the semantic sub-struct, and write back.
async fn update_semantic_settings<F: FnOnce(SemanticSettings) -> SemanticSettings>(f: F) {
    let path = match desktop_settings_path() {
        Ok(p) => p,
        Err(e) => { error!("Failed to resolve settings path: {e:#}"); return; }
    };
    let current = match wilkes_api::commands::settings::get_settings(&path).await {
        Ok(s) => s,
        Err(e) => { error!("Failed to read settings for semantic update: {e:#}"); return; }
    };
    let semantic = f(current.semantic);
    if let Err(e) = wilkes_api::commands::settings::update_settings(
        &path,
        serde_json::json!({ "semantic": semantic }),
    )
    .await
    {
        error!("Failed to write semantic settings: {e:#}");
    }
}

// ── Worker Management ─────────────────────────────────────────────────────────

#[tauri::command]
async fn get_worker_status(app: AppHandle) -> Result<wilkes_core::embed::worker_manager::WorkerStatus, String> {
    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    manager.sender().send(wilkes_core::embed::worker_manager::ManagerCommand::GetStatus(tx)).await.map_err(|e| e.to_string())?;
    rx.await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn kill_worker(app: AppHandle) -> Result<(), String> {
    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();
    manager.sender().send(wilkes_core::embed::worker_manager::ManagerCommand::KillWorker).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_worker_timeout(app: AppHandle, secs: u64) -> Result<(), String> {
    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();
    manager.sender().send(wilkes_core::embed::worker_manager::ManagerCommand::SetTimeout(secs)).await.map_err(|e| e.to_string())
}

// ── Startup state restore ─────────────────────────────────────────────────────

/// If settings indicate a model was downloaded and an index was built, reload
/// both into in-memory state so semantic search works immediately after restart.
async fn restore_semantic_state(app: &AppHandle) {
    let data_dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => { error!("restore_semantic_state: cannot get data dir: {e:#}"); return; }
    };
    let settings_path = match desktop_settings_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let settings = match wilkes_api::commands::settings::get_settings(&settings_path).await {
        Ok(s) => s,
        Err(e) => { error!("restore_semantic_state: cannot read settings: {e:#}"); return; }
    };

    // Use the DB as the ground truth: check if an index exists and its model_id
    // matches settings. The `enabled` flag in settings is derived state; do not
    // gate on it so that a valid index is always restored after a restart.
    let db_status = match tokio::task::spawn_blocking({
        let d = data_dir.clone();
        move || SemanticIndex::read_status_from_path(&d)
    })
    .await
    {
        Ok(Ok(s)) => s,
        _ => return, // No index on disk.
    };

    let model = settings.semantic.model.clone();
    let engine = settings.semantic.engine;

    if db_status.model_id != model.model_id() {
        info!(
            "restore_semantic_state: index model '{}' does not match settings model '{}', skipping restore",
            db_status.model_id,
            model.model_id()
        );
        return;
    }

    let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();
    let installer = dispatch::get_installer(engine, model.clone(), manager, settings.semantic.device.clone());
    
    // Ensure dimension is probed/known before build()
    let (tx, _) = tokio::sync::mpsc::channel(1);
    if let Err(e) = installer.install(&data_dir, tx).await {
        error!("restore_semantic_state: installer.install failed: {e:#}");
        return;
    }

    if !installer.is_available(&data_dir) {
        info!("restore_semantic_state: model files not found, skipping restore");
        return;
    }

    let data_dir_clone = data_dir.clone();
    let embedder = match tokio::task::spawn_blocking(move || installer.build(&data_dir_clone)).await {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => { error!("restore_semantic_state: build embedder failed: {e:#}"); return; }
        Err(e) => { error!("restore_semantic_state: build embedder panicked: {e}"); return; }
    };
    let loaded_embedder = Some(Arc::clone(&embedder));

    let data_dir_clone = data_dir.clone();
    let m = model.model_id().to_string();
    let expected_dim = embedder.dimension();
    let index = match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir_clone, &m, expected_dim)).await {
        Ok(Ok(idx)) => idx,
        Ok(Err(e)) => { error!("restore_semantic_state: open index failed: {e:#}"); return; }
        Err(e) => { error!("restore_semantic_state: open index panicked: {e}"); return; }
    };

    if let Some(ref emb) = loaded_embedder {
        *app.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(Arc::clone(emb));
    }
    
    let index_arc = Arc::new(Mutex::new(Some(index)));
    *app.state::<SemanticIndexState>().0.lock().unwrap() = Arc::clone(&index_arc);

    // Start watcher
    if let Some(root) = settings.last_directory {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(wilkes_core::extract::pdf::PdfExtractor::new()));

        let watcher_config = if engine == EmbeddingEngine::SBERT {
            let manager = app.state::<wilkes_core::embed::worker_manager::WorkerManager>().inner().clone();
            Some(wilkes_core::embed::watcher::WatcherConfig {
                manager,
                model_id: model.model_id().to_string(),
                data_dir: data_dir.clone(),
                device: settings.semantic.device.clone(),
            })
        } else {
            None
        };

        let handle_for_watcher = app.clone();
        let handle_for_watcher_done = handle_for_watcher.clone();
        match IndexWatcher::start(
            root,
            index_arc,
            Arc::new(registry),
            loaded_embedder,
            watcher_config,
            settings.semantic.chunk_size,
            settings.semantic.chunk_overlap,
            move || {
                let _ = handle_for_watcher.emit("manager-event", "Reindexing");
            },
            move || {
                let _ = handle_for_watcher_done.emit("manager-event", "ReindexingDone");
            }
        ) {
            Ok(watcher) => {
                *app.state::<WatcherState>().0.lock().unwrap() = Some(watcher);
            }
            Err(e) => error!("restore_semantic_state: failed to start watcher: {e:#}"),
        }
    }

    // Persist the ground-truth state back to settings so the UI reflects it.
    let db_path = data_dir.join("semantic_index.db");
    let dim = db_status.dimension;
    update_semantic_settings(|s| SemanticSettings {
        enabled: true,
        index_path: Some(db_path),
        dimension: dim,
        ..s
    })
    .await;

    info!("restore_semantic_state: embedder and index restored");
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run() {
    wilkes_core::logging::init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            
            let python_path = resolve_python().unwrap_or_default();
            let script_path = resolve_worker_script(&handle).unwrap_or_default();
            let worker_bin = std::env::current_exe().unwrap_or_default().with_file_name("wilkes-worker");
            let paths = wilkes_core::embed::worker_manager::WorkerPaths { python_path, script_path, worker_bin };
            let (manager, mut event_rx, loop_fut) = wilkes_core::embed::worker_manager::WorkerManager::new(paths);
            app.manage(manager);

            let handle_for_events = handle.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    match event {
                        wilkes_core::embed::worker_manager::ManagerEvent::WorkerStarting => {
                            let _ = handle_for_events.emit("manager-event", "WorkerStarting");
                        }
                        wilkes_core::embed::worker_manager::ManagerEvent::ReindexingDone => {
                            let _ = handle_for_events.emit("manager-event", "ReindexingDone");
                        }
                    }
                }
            });

            tauri::async_runtime::spawn(async move {
                tauri::async_runtime::spawn(loop_fut);
                restore_semantic_state(&handle).await;
            });
            Ok(())
        })
        .manage(ActiveSearches(Mutex::new(HashMap::new())))
        .manage(EmbedState(Mutex::new(None)))
        .manage(ActiveEmbedderState(Mutex::new(None)))
        .manage(SemanticIndexState(Mutex::new(Arc::new(Mutex::new(None)))))
        .manage(WatcherState(Mutex::new(None)))
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
