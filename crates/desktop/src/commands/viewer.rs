use crate::*;

#[tauri::command]
pub(crate) async fn preview_standalone(
    match_ref: wilkes_core::types::MatchRef,
) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::preview::preview(match_ref, None)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn get_standalone_file_metadata(
    app: AppHandle,
    path: String,
) -> Result<DocumentMetadata, String> {
    let settings =
        wilkes_api::commands::settings::get_settings(&app.state::<GlobalSettingsPath>().0)
            .await
            .map_err(|error| error.to_string())?;
    wilkes_api::commands::metadata::get_file_metadata(path.into(), settings.supported_extensions)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn preview(
    match_ref: wilkes_core::types::MatchRef,
    window: tauri::Window,
) -> Result<wilkes_core::types::PreviewData, String> {
    window_context(&window)
        .preview(match_ref)
        .await
        .map_err(|e| e.to_string())
}

/// One comic page as an image and its native geometry.
#[tauri::command]
pub(crate) async fn comic_page(
    path: String,
    page: u32,
    window: tauri::Window,
) -> Result<tauri::ipc::Response, String> {
    let bytes = window_context(&window)
        .comic_page(std::path::PathBuf::from(path), page)
        .await
        .map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// A file's first page, as PNG bytes.
///
/// `tauri::ipc::Response` for the reason `comic_page` uses it. The stored cover
/// is not served over the asset protocol because that protocol's scope does not
/// reach dot-directories, which is where Linux keeps application data.
#[tauri::command]
pub(crate) async fn file_cover(
    path: String,
    max_edge: u32,
    window: tauri::Window,
) -> Result<tauri::ipc::Response, String> {
    window_context(&window)
        .file_cover(std::path::PathBuf::from(path), max_edge)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(|e| e.to_string())
}

/// The pages of a book, as PDF bytes.
///
/// Returned as bytes rather than served over the asset protocol because there
/// is no file to serve: the pages exist only as a rendering this process made,
/// from a layout it chose. `tauri::ipc::Response` sends them raw, so a 6 MB
/// book does not become 8 MB of base64 on the way.
#[tauri::command]
pub(crate) async fn surrogate_bytes(
    path: String,
    window: tauri::Window,
) -> Result<tauri::ipc::Response, String> {
    window_context(&window)
        .surrogate_bytes(std::path::PathBuf::from(path))
        .await
        .map(tauri::ipc::Response::new)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn resolve_pdf_font(
    request: wilkes_core::pdf_edit::fonts::FontRequest,
    window: tauri::Window,
) -> Result<String, String> {
    window_context(&window)
        .resolve_pdf_font(request)
        .await
        .map_err(|e| e.to_string())
}

/// A font file, raw: the interface registers it as a `FontFace`.
#[tauri::command]
pub(crate) async fn pdf_font_bytes(
    id: String,
    window: tauri::Window,
) -> Result<tauri::ipc::Response, String> {
    window_context(&window)
        .pdf_font_bytes(id)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn save_pdf_edits(
    source: String,
    destination: String,
    export: wilkes_core::pdf_edit::EditExport,
    window: tauri::Window,
) -> Result<(), String> {
    window_context(&window)
        .save_pdf_edits(
            std::path::PathBuf::from(source),
            std::path::PathBuf::from(destination),
            export,
        )
        .await
        // The whole chain: "writing page 3" without what went wrong on page 3
        // is not something anyone can act on.
        .map_err(|e| format!("{e:#}"))
}
