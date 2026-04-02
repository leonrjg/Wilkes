use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use wilkes_core::embed::Embedder;
use wilkes_core::embed::dispatch;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::watcher::IndexWatcher;
use wilkes_core::embed::worker_ipc::{WorkerEvent, WorkerRequest};
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
async fn download_model(model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let installer = dispatch::get_installer(engine, model.clone());
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
                        update_semantic_settings(|s| SemanticSettings { enabled: true, engine, model, ..s }).await;
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
///
/// The actual embedding work runs in a separate `wilkes-worker` process so that
/// a crash inside the embedder (OOM, Metal/ONNX driver fault, etc.) cannot kill
/// the Tauri UI. On success the worker exits, then the desktop reopens the index
/// and starts the watcher in-process as before.
///
/// The worker binary must sit next to the desktop binary at runtime. During
/// development `cargo build -p wilkes-worker` places it in the same target
/// directory. For bundled distributions list it as a Tauri sidecar in
/// `tauri.conf.json` under `bundle.externalBin`.
#[tauri::command]
async fn build_index(root: String, model: EmbedderModel, engine: EmbeddingEngine, app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let settings_path = desktop_settings_path().map_err(|e| e.to_string())?;
    let current_settings = wilkes_api::commands::settings::get_settings(&settings_path).await
        .unwrap_or_default();
    let chunk_size = current_settings.semantic.chunk_size;
    let chunk_overlap = current_settings.semantic.chunk_overlap;

    // Stop the active watcher before rebuilding.
    stop_watcher(&app);

    // Resolve the worker binary: same directory as the running executable.
    let worker_bin = std::env::current_exe()
        .map_err(|e| format!("Cannot resolve current exe: {e}"))?
        .with_file_name("wilkes-worker");

    let request = WorkerRequest {
        root: root.into(),
        engine,
        model: model.clone(),
        data_dir: data_dir.clone(),
        chunk_size,
        chunk_overlap,
    };
    let request_json = serde_json::to_string(&request)
        .map_err(|e| format!("Failed to serialise worker request: {e}"))?;

    let mut child = tokio::process::Command::new(&worker_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Failed to spawn wilkes-worker ({worker_bin:?}): {e}"))?;

    // Send the config and close stdin so the worker can proceed.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(request_json.as_bytes()).await
            .map_err(|e| format!("Failed to write worker config: {e}"))?;
        stdin.write_all(b"\n").await
            .map_err(|e| format!("Failed to write worker config newline: {e}"))?;
    }

    let stdout = child.stdout.take().expect("stdout was piped");

    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();

    let app_clone = app.clone();
    let data_dir_clone = data_dir.clone();
    let root_path = request.root.clone();

    let join: JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        let mut final_event: Option<WorkerEvent> = None;

        loop {
            tokio::select! {
                biased;
                _ = cancel_for_task.cancelled() => {
                    child.kill().await.ok();
                    child.wait().await.ok();
                    let _ = std::fs::remove_file(data_dir_clone.join("semantic_index.db"));
                    let _ = app_clone.emit("embed-error", EmbedError {
                        operation: EmbedOperation::Build,
                        message: String::new(), // empty = user-cancelled
                    });
                    return Ok(());
                }
                line = reader.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            match serde_json::from_str::<WorkerEvent>(&line) {
                                Ok(WorkerEvent::Progress(p)) => {
                                    let _ = app_clone.emit("embed-progress", &p);
                                }
                                Ok(ev) => {
                                    // Done or Error — no more lines expected.
                                    final_event = Some(ev);
                                    break;
                                }
                                Err(e) => {
                                    eprintln!("[worker] Unrecognised stdout line ({e}): {line}");
                                }
                            }
                        }
                        _ => break, // EOF or read error
                    }
                }
            }
        }

        let exit_status = child.wait().await.ok();

        match final_event {
            Some(WorkerEvent::Done) => {
                // Worker completed: reload embedder + open index in-process.
                let installer = dispatch::get_installer(engine, model.clone());
                let embedder = match tokio::task::spawn_blocking({
                    let d = data_dir_clone.clone();
                    move || installer.build(&d)
                })
                .await
                {
                    Ok(Ok(e)) => e,
                    Ok(Err(e)) => {
                        let _ = app_clone.emit("embed-error", EmbedError {
                            operation: EmbedOperation::Build,
                            message: e.to_string(),
                        });
                        return Ok(());
                    }
                    Err(e) => {
                        let _ = app_clone.emit("embed-error", EmbedError {
                            operation: EmbedOperation::Build,
                            message: e.to_string(),
                        });
                        return Ok(());
                    }
                };

                *app_clone.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(Arc::clone(&embedder));

                let open_result = tokio::task::spawn_blocking({
                    let d = data_dir_clone.clone();
                    let emb = Arc::clone(&embedder);
                    move || SemanticIndex::open(&d, emb.as_ref())
                })
                .await;

                let open_msg = match open_result {
                    Ok(Ok(idx)) => {
                        *app_clone.state::<SemanticIndexState>().0.lock().unwrap() = Some(idx);

                        let index_arc = app_clone.state::<SemanticIndexState>().0.clone();
                        let mut registry = ExtractorRegistry::new();
                        registry.register(Box::new(wilkes_core::extract::pdf::PdfExtractor::new()));

                        match IndexWatcher::start(
                            root_path,
                            index_arc,
                            Arc::new(registry),
                            Arc::clone(&embedder),
                            chunk_size,
                            chunk_overlap,
                        ) {
                            Ok(watcher) => {
                                *app_clone.state::<WatcherState>().0.lock().unwrap() = Some(watcher);
                            }
                            Err(e) => error!("Failed to start watcher: {e:#}"),
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
                    let _ = app_clone.emit("embed-error", EmbedError {
                        operation: EmbedOperation::Build,
                        message: msg,
                    });
                }
            }
            Some(WorkerEvent::Error(msg)) => {
                let _ = std::fs::remove_file(data_dir_clone.join("semantic_index.db"));
                let _ = app_clone.emit("embed-error", EmbedError {
                    operation: EmbedOperation::Build,
                    message: msg,
                });
            }
            _ => {
                // Worker exited without sending Done or Error (ungraceful termination).
                let _ = std::fs::remove_file(data_dir_clone.join("semantic_index.db"));
                #[cfg(unix)]
                let message = {
                    use std::os::unix::process::ExitStatusExt;
                    if exit_status.and_then(|s| s.signal()) == Some(9) {
                        error!("[worker] killed by OS out-of-memory killer (SIGKILL)");
                        "Worker ran out of memory. Try a smaller model or index a smaller directory.".to_string()
                    } else {
                        let status_info = exit_status
                            .map(|s| format!(" (exit status: {s})"))
                            .unwrap_or_default();
                        error!("[worker] terminated without Done/Error{status_info}");
                        format!("Worker process terminated unexpectedly{status_info}")
                    }
                };
                #[cfg(not(unix))]
                let message = {
                    let status_info = exit_status
                        .map(|s| format!(" (exit status: {s})"))
                        .unwrap_or_default();
                    error!("[worker] terminated without Done/Error{status_info}");
                    format!("Worker process terminated unexpectedly{status_info}")
                };
                let _ = app_clone.emit("embed-error", EmbedError {
                    operation: EmbedOperation::Build,
                    message,
                });
            }
        }

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
    *app.state::<SemanticIndexState>().0.lock().unwrap() = None;
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

    if !settings.semantic.enabled || settings.semantic.index_path.is_none() {
        return;
    }

    let model = settings.semantic.model;
    let engine = settings.semantic.engine;
    let installer = dispatch::get_installer(engine, model);
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
    *app.state::<ActiveEmbedderState>().0.lock().unwrap() = Some(Arc::clone(&embedder));

    let data_dir_clone = data_dir.clone();
    let emb = Arc::clone(&embedder);
    let index = match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir_clone, emb.as_ref())).await {
        Ok(Ok(idx)) => idx,
        Ok(Err(e)) => { error!("restore_semantic_state: open index failed: {e:#}"); return; }
        Err(e) => { error!("restore_semantic_state: open index panicked: {e}"); return; }
    };
    *app.state::<SemanticIndexState>().0.lock().unwrap() = Some(index);

    info!("restore_semantic_state: embedder and index restored");
}

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run() {
    wilkes_core::logging::init_logging();

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
            get_logs,
            clear_logs,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
