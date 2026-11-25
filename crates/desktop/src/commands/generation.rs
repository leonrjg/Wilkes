use crate::*;

#[tauri::command]
pub(crate) async fn is_generation_ready(window: tauri::Window) -> bool {
    window_context(&window).is_generation_ready().await
}

#[tauri::command]
pub(crate) async fn list_generation_models(
    window: tauri::Window,
) -> Result<Vec<wilkes_core::types::GeneratorDescriptor>, String> {
    window_context(&window)
        .list_generation_models()
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub(crate) async fn get_generation_model_size(
    model_id: String,
    window: tauri::Window,
) -> Result<u64, String> {
    window_context(&window)
        .fetch_generation_model_size(&model_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Download (if needed) and attach the configured generation model. Progress
/// arrives on the generation-progress event stream, terminated by
/// generation-done or generation-error.
#[tauri::command]
pub(crate) async fn load_generation_model(window: tauri::Window) -> Result<bool, String> {
    window_context(&window)
        .load_generator()
        .await
        .map(|outcome| outcome.attached())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub(crate) async fn explain_related_document(
    request_id: String,
    anchor_path: String,
    path: String,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .explain_related_document(request_id, anchor_path.into(), path.into())
        .await
}

#[tauri::command]
pub(crate) async fn summarize_document(
    request_id: String,
    path: String,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .summarize_document(request_id, path.into())
        .await
}

#[tauri::command]
pub(crate) async fn summarize_search_results(
    request_id: String,
    input: wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryInput,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .summarize_search_results(request_id, input)
        .await
}

#[tauri::command]
pub(crate) async fn request_completion(
    completion_id: String,
    request: wilkes_core::completion::CompletionRequest,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .request_completion(completion_id, request)
        .await
}

#[tauri::command]
pub(crate) fn cancel_completion(completion_id: String, window: tauri::Window) {
    window_context(&window).cancel_completion(&completion_id);
}

#[tauri::command]
pub(crate) async fn completion_feedback(
    completion_id: String,
    feedback: wilkes_core::completion::CompletionFeedback,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .completion_feedback(&completion_id, feedback)
        .await
}

#[tauri::command]
pub(crate) fn get_session_steering(
    window: tauri::Window,
) -> wilkes_core::completion::SessionSteering {
    window_context(&window).get_session_steering()
}

#[tauri::command]
pub(crate) fn reset_session_steering(window: tauri::Window) {
    window_context(&window).reset_session_steering();
}

#[tauri::command]
pub(crate) async fn save_document(
    path: String,
    text: String,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .save_document(path.into(), text)
        .await
}
