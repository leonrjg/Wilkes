use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use wilkes_core::types::{FileEntry, MatchRef, SearchQuery, SearchStats, Settings};

// ── App state ────────────────────────────────────────────────────────────────

struct ActiveSearches(Mutex<HashMap<String, JoinHandle<()>>>);

// ── Tauri commands ───────────────────────────────────────────────────────────

/// Start a search. Returns a `search_id` that identifies this run.
/// Results are emitted as `search-result-{id}` events (payload: FileMatches).
/// A final `search-complete-{id}` event carries SearchStats.
#[tauri::command]
async fn search(query: SearchQuery, app: AppHandle) -> Result<String, String> {
    let search_id = uuid::Uuid::new_v4().to_string();

    let handle = wilkes_api::commands::search::start_search(query);

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

        // Clean up handle from active searches
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

/// Cancel a running search by aborting the forwarder task, which drops rx
/// and causes the provider's blocking_send to fail, stopping the walk.
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
    wilkes_api::commands::settings::get_settings()
        .await
        .map_err(|e| e.to_string())
}

/// Merge a partial settings patch and persist. Returns the full new settings.
#[tauri::command]
async fn update_settings(patch: serde_json::Value) -> Result<Settings, String> {
    wilkes_api::commands::settings::update_settings(patch)
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

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ActiveSearches(Mutex::new(HashMap::new())))
        .invoke_handler(tauri::generate_handler![
            search,
            cancel_search,
            preview,
            list_files,
            open_file,
            get_settings,
            update_settings,
            pick_directory,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
