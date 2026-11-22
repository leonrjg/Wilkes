#[tauri::command]
pub(crate) async fn get_http_calls() -> Vec<wilkes_core::http_history::HttpCall> {
    wilkes_api::commands::logs::get_http_calls()
}

#[tauri::command]
pub(crate) async fn clear_http_calls() {
    wilkes_api::commands::logs::clear_http_calls();
}

#[tauri::command]
pub(crate) async fn get_logs() -> Result<Vec<String>, String> {
    Ok(wilkes_api::commands::logs::get_logs())
}

#[tauri::command]
pub(crate) async fn clear_logs() -> Result<(), String> {
    wilkes_api::commands::logs::clear_logs();
    Ok(())
}
