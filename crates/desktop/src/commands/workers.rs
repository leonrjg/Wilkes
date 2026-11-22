use crate::*;

pub(crate) fn get_worker_status_for_ctx(ctx: Arc<AppContext>) -> WorkerStatus {
    ctx.get_worker_status()
}

/// Every worker, one row per role. Two processes can die independently, so a
/// single status would misreport a dead generation worker as healthy.
pub(crate) fn get_worker_statuses_for_ctx(ctx: Arc<AppContext>) -> Vec<WorkerStatus> {
    ctx.get_worker_statuses()
}

pub(crate) async fn kill_worker_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    ctx.kill_worker();
    Ok(())
}

pub(crate) async fn set_worker_timeout_for_ctx(
    ctx: Arc<AppContext>,
    secs: u64,
) -> Result<(), String> {
    ctx.set_worker_timeout(secs)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn get_python_envs(
    window: tauri::WebviewWindow,
) -> Result<wilkes_core::worker::PythonEnvInventory, String> {
    window_context(&window)
        .python_environments()
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub(crate) async fn remove_python_envs(
    ids: Vec<String>,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .remove_python_environments(ids)
        .await
}

#[tauri::command]
pub(crate) async fn remove_custom_model(
    engine: wilkes_core::types::EmbeddingEngine,
    model_id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .remove_custom_model(engine, model_id)
        .await
}

#[tauri::command]
pub(crate) fn get_worker_status(window: tauri::WebviewWindow) -> WorkerStatus {
    get_worker_status_for_ctx(window_context(&window))
}

#[tauri::command]
pub(crate) fn get_worker_statuses(window: tauri::WebviewWindow) -> Vec<WorkerStatus> {
    get_worker_statuses_for_ctx(window_context(&window))
}

#[tauri::command]
pub(crate) async fn kill_worker(window: tauri::WebviewWindow) -> Result<(), String> {
    kill_worker_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn set_worker_timeout(
    secs: u64,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    set_worker_timeout_for_ctx(window_context(&window), secs).await
}
