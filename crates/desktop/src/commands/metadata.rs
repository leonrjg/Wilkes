use crate::*;

pub(crate) async fn zotero_status_for_ctx(
    ctx: Arc<AppContext>,
) -> Result<IntegrationStatus, String> {
    ctx.zotero_status().await.map_err(|e| e.to_string())
}

pub(crate) async fn resolve_file_metadata_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    ctx.resolve_file_metadata(path.into())
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn refresh_file_metadata_for_ctx(
    ctx: Arc<AppContext>,
    path: Option<String>,
) -> Result<(), String> {
    ctx.refresh_file_metadata(path.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn zotero_add_item_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<AddOutcome, String> {
    ctx.zotero_add_item(path.into())
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn zotero_generate_citation_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<CitationResult, String> {
    ctx.zotero_generate_citation(path.into())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) async fn zotero_status(
    window: tauri::Window,
) -> Result<IntegrationStatus, String> {
    zotero_status_for_ctx(window_context(&window)).await
}

#[tauri::command]
pub(crate) async fn resolve_file_metadata(
    path: String,
    window: tauri::Window,
) -> Result<DocumentMetadata, String> {
    resolve_file_metadata_for_ctx(window_context(&window), path).await
}

#[tauri::command]
pub(crate) async fn refresh_file_metadata(
    path: Option<String>,
    window: tauri::Window,
) -> Result<(), String> {
    refresh_file_metadata_for_ctx(window_context(&window), path).await
}

#[tauri::command]
pub(crate) async fn zotero_add_item(
    path: String,
    window: tauri::Window,
) -> Result<AddOutcome, String> {
    zotero_add_item_for_ctx(window_context(&window), path).await
}

#[tauri::command]
pub(crate) async fn zotero_generate_citation(
    path: String,
    window: tauri::Window,
) -> Result<CitationResult, String> {
    zotero_generate_citation_for_ctx(window_context(&window), path).await
}
