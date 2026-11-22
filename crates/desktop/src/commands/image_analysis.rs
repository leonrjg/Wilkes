use crate::*;

/// Every recognizer this build can read with, and the engines it compiled in.
#[tauri::command]
pub(crate) async fn image_recognizer_catalogue(
    window: tauri::WebviewWindow,
) -> wilkes_core::extract::image::dispatch::RecognizerCatalogue {
    window_context(&window).image_recognizer_catalogue()
}

/// What the named image recognizer is, where it came from, and under what
/// licence. Answers whether or not it is installed: it describes the recipe,
/// and the point of it is to be readable before the download rather than after.
#[tauri::command]
pub(crate) async fn image_recognizer_inventory(
    engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    model_id: String,
    window: tauri::WebviewWindow,
) -> Result<wilkes_core::types::RecognizerInventory, String> {
    window_context(&window)
        .image_recognizer_inventory(engine, &model_id)
        .map_err(|e| format!("{e:#}"))
}

/// Download (if needed) and verify the named image recognizer, then attach the
/// analyzer if the settings already name it. Progress arrives on the
/// image-analysis-progress stream, terminated by image-analysis-done or
/// image-analysis-error.
#[tauri::command]
pub(crate) async fn install_image_recognizer(
    engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    model_id: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    window_context(&window)
        .install_image_recognizer(engine, model_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Download the layout detector. Reports on the same event stream as the
/// recognizer install: image-analysis-progress, image-analysis-done,
/// image-analysis-error.
#[cfg(feature = "recognize-onnx")]
#[tauri::command]
pub(crate) async fn install_layout_detector(window: tauri::WebviewWindow) -> Result<(), String> {
    window_context(&window)
        .install_layout_detector()
        .await
        .map_err(|e| format!("{e:#}"))
}

/// The same command in a build with no detector compiled in. Present rather
/// than absent so the UI gets an explanation instead of an unknown-command
/// error, which reads as a bug in the application.
#[cfg(not(feature = "recognize-onnx"))]
#[tauri::command]
pub(crate) async fn install_layout_detector(_app: AppHandle) -> Result<(), String> {
    Err("this build has no layout detector compiled in".to_string())
}
