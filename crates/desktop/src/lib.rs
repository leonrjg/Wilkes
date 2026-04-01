use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wilkes_core::embed::Embedder;
use wilkes_core::embed::candle::CandleInstaller;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::{EmbedProgress, EmbedderInstaller};
use wilkes_core::embed::watcher::IndexWatcher;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{
    EmbedderModel, FileEntry, IndexStatus, MatchRef, ModelDescriptor, SearchMode, SearchQuery,
    SearchStats, SemanticSettings, Settings,
};

fn desktop_settings_path() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
    Ok(config.join("wilkes").join("settings.json"))
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
struct SemanticIndexState(Arc<Mutex<Option<SemanticIndex>>>);

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

// ── Tauri commands ───────────────────────────────────────────────────────────

/// Start a search. Returns a `search_id` that identifies this run.
/// Results are emitted as `search-result-{id}` events (payload: FileMatches).
/// A final `search-complete-{id}` event carries SearchStats.
/// The caller may supply a `search_id` that was pre-registered with `listen`
/// before this invocation, preventing a race between event emission and listener
/// registration.
#[tauri::command]
async fn search(query: SearchQuery, search_id: Option<String>, app: AppHandle) -> Result<String, String> {
    // For semantic mode, extract the embedder and index from state.
    let embedder = if query.mode == SearchMode::Semantic {
        let state = app.state::<ActiveEmbedderState>();
        let guard = state.0.lock().unwrap();
        match guard.clone() {
            Some(e) => Some(e),
            None => return Err("No embedder loaded. Download and install a model first.".into()),
        }
    } else {
        None
    };

    let index = if query.mode == SearchMode::Semantic {
        Some(app.state::<SemanticIndexState>().0.clone())
    } else {
        None
    };

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
async fn download_model(model: EmbedderModel, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let installer = Arc::new(CandleInstaller::new(model));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

    let app_clone = app.clone();
    let installer_clone = Arc::clone(&installer);
    let data_dir_clone = data_dir.clone();

    let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let forward_app = app_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(progress) = rx.recv().await {
                let _ = forward_app.emit("embed-progress", &progress);
            }
        });

        let result =
            wilkes_api::commands::embed::download_model(installer_clone.as_ref(), data_dir_clone.clone(), tx).await;

        let _ = forward_handle.await;

        match result {
            Ok(()) => {
                // Load the embedder into state.
                match installer_clone.build(&data_dir_clone) {
                    Ok(embedder) => {
                        *app_clone.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(embedder);
                        update_semantic_settings(|s| SemanticSettings { enabled: true, ..s }).await;
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

        Ok(())
    });

    let cancel = CancellationToken::new();
    *app.state::<EmbedState>().0.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
    Ok(())
}

/// Build the semantic index for `root` using `model`.
/// Emits `embed-progress`, `embed-done`, or `embed-error`.
#[tauri::command]
async fn build_index(root: String, model: EmbedderModel, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;

    // Stop the active watcher before rebuilding (a watcher event against a
    // half-written index during rebuild would corrupt it).
    stop_watcher(&app);

    let cancel = CancellationToken::new();
    let cancel_for_select = cancel.clone();

    let installer = Arc::new(CandleInstaller::new(model));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);

    let app_clone = app.clone();
    let installer_clone = Arc::clone(&installer);
    let data_dir_clone = data_dir.clone();
    let root_path: std::path::PathBuf = root.into();

    let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let forward_app = app_clone.clone();
        let forward_handle = tokio::spawn(async move {
            while let Some(progress) = rx.recv().await {
                let _ = forward_app.emit("embed-progress", &progress);
            }
        });

        // Race the build against the cancel token. When cancel fires the
        // select drops the build_index future at its next await point
        // (the spawn_blocking await), giving an immediate response.
        let result = tokio::select! {
            biased;
            _ = cancel_for_select.cancelled() => Err(anyhow::anyhow!("cancelled")),
            result = wilkes_api::commands::embed::build_index(
                root_path.clone(),
                installer_clone.as_ref(),
                data_dir_clone.clone(),
                tx,
            ) => result,
        };

        // The progress forwarder can be dropped immediately; any in-flight
        // events from the still-running background thread are irrelevant now.
        forward_handle.abort();

        match result {
            Ok(embedder) => {
                // Store the embedder in state.
                *app_clone.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(Arc::clone(&embedder));

                // Open the newly built index.
                let open_result =
                    tokio::task::spawn_blocking({
                        let data_dir = data_dir_clone.clone();
                        let emb = Arc::clone(&embedder);
                        move || SemanticIndex::open(&data_dir, emb.as_ref())
                    })
                    .await;

                let open_msg = match open_result {
                    Ok(Ok(idx)) => {
                        *app_clone.state::<SemanticIndexState>().0.lock().unwrap() = Some(idx);

                        // Start the watcher.
                        let index_arc = app_clone.state::<SemanticIndexState>().0.clone();
                        let mut registry = ExtractorRegistry::new();
                        registry.register(Box::new(wilkes_core::extract::pdf::PdfExtractor::new()));

                        match IndexWatcher::start(
                            root_path,
                            index_arc,
                            Arc::new(registry),
                            Arc::clone(&embedder),
                        ) {
                            Ok(watcher) => {
                                *app_clone.state::<WatcherState>().0.lock().unwrap() = Some(watcher);
                            }
                            Err(e) => {
                                eprintln!("Failed to start watcher: {e:#}");
                            }
                        }

                        let db_path = data_dir_clone.join("semantic_index.db");
                        update_semantic_settings(|s| SemanticSettings { index_path: Some(db_path), ..s }).await;
                        let _ = app_clone.emit("embed-done", EmbedDone { operation: EmbedOperation::Build });
                        None
                    }
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                if let Some(msg) = open_msg {
                    let _ = app_clone.emit(
                        "embed-error",
                        EmbedError {
                            operation: EmbedOperation::Build,
                            message: msg,
                        },
                    );
                }
            }
            Err(e) => {
                // Remove the partial index so a future build starts clean.
                let _ = std::fs::remove_file(data_dir_clone.join("semantic_index.db"));
                let msg = if e.to_string() == "cancelled" {
                    String::new()
                } else {
                    e.to_string()
                };
                let _ = app_clone.emit(
                    "embed-error",
                    EmbedError {
                        operation: EmbedOperation::Build,
                        message: msg,
                    },
                );
            }
        }

        Ok(())
    });

    *app.state::<EmbedState>().0.lock().unwrap() = Some(EmbedTaskHandle { cancel, join });
    Ok(())
}

/// Return all fastembed-supported models annotated with local cache availability.
#[tauri::command]
async fn list_models(app: AppHandle) -> Result<Vec<ModelDescriptor>, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(wilkes_api::commands::embed::list_models(&data_dir).await)
}

/// Fetch the total download size for `model_id` from the HuggingFace API.
#[tauri::command]
async fn get_model_size(model_id: String) -> Result<u64, String> {
    wilkes_api::commands::embed::get_model_size(model_id)
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
    *app.state::<SemanticIndexState>().0.lock().unwrap() = None;
    *app.state::<ActiveEmbedderState>().0.lock().unwrap() = None;

    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    wilkes_api::commands::embed::delete_index(&data_dir)
        .await
        .map_err(|e| e.to_string())?;
    update_semantic_settings(|s| SemanticSettings { index_path: None, ..s }).await;
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
        Err(e) => { eprintln!("Failed to resolve settings path: {e:#}"); return; }
    };
    let current = match wilkes_api::commands::settings::get_settings(&path).await {
        Ok(s) => s,
        Err(e) => { eprintln!("Failed to read settings for semantic update: {e:#}"); return; }
    };
    let semantic = f(current.semantic);
    if let Err(e) = wilkes_api::commands::settings::update_settings(
        &path,
        serde_json::json!({ "semantic": semantic }),
    )
    .await
    {
        eprintln!("Failed to write semantic settings: {e:#}");
    }
}

// ── Startup state restore ─────────────────────────────────────────────────────

/// If settings indicate a model was downloaded and an index was built, reload
/// both into in-memory state so semantic search works immediately after restart.
async fn restore_semantic_state(app: &AppHandle) {
    let data_dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => { eprintln!("restore_semantic_state: cannot get data dir: {e:#}"); return; }
    };
    let settings_path = match desktop_settings_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    let settings = match wilkes_api::commands::settings::get_settings(&settings_path).await {
        Ok(s) => s,
        Err(e) => { eprintln!("restore_semantic_state: cannot read settings: {e:#}"); return; }
    };

    if !settings.semantic.enabled || settings.semantic.index_path.is_none() {
        return;
    }

    let model = settings.semantic.model;
    let installer = CandleInstaller::new(model);
    if !installer.is_available(&data_dir) {
        eprintln!("restore_semantic_state: model files not found, skipping restore");
        return;
    }

    let data_dir_clone = data_dir.clone();
    let embedder = match tokio::task::spawn_blocking(move || installer.build(&data_dir_clone)).await {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => { eprintln!("restore_semantic_state: build embedder failed: {e:#}"); return; }
        Err(e) => { eprintln!("restore_semantic_state: build embedder panicked: {e}"); return; }
    };
    *app.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(Arc::clone(&embedder));

    let data_dir_clone = data_dir.clone();
    let emb = Arc::clone(&embedder);
    let index = match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir_clone, emb.as_ref())).await {
        Ok(Ok(idx)) => idx,
        Ok(Err(e)) => { eprintln!("restore_semantic_state: open index failed: {e:#}"); return; }
        Err(e) => { eprintln!("restore_semantic_state: open index panicked: {e}"); return; }
    };
    *app.state::<SemanticIndexState>().0.lock().unwrap() = Some(index);

    eprintln!("restore_semantic_state: embedder and index restored");
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                restore_semantic_state(&handle).await;
            });
            Ok(())
        })
        .manage(ActiveSearches(Mutex::new(HashMap::new())))
        .manage(EmbedState(Mutex::new(None)))
        .manage(ActiveEmbedderState(Mutex::new(None)))
        .manage(SemanticIndexState(Arc::new(Mutex::new(None))))
        .manage(WatcherState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            search,
            cancel_search,
            preview,
            list_files,
            open_file,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
