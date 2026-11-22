use crate::*;

pub(crate) async fn preview_handler(
    State(state): State<Arc<AppState>>,
    Json(match_ref): Json<MatchRef>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let data = ctx
        .preview(match_ref)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(data))
}

#[derive(Deserialize)]
pub(crate) struct ComicPageQuery {
    pub(crate) path: String,
    pub(crate) page: u32,
}

pub(crate) async fn comic_page_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ComicPageQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let bytes = ctx
        .comic_page(std::path::PathBuf::from(params.path), params.page)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    ))
}

#[derive(Deserialize)]
pub(crate) struct FileCoverQuery {
    pub(crate) path: String,
    /// The longest side of the picture, in pixels.
    pub(crate) max_edge: u32,
}

/// A file's first page, as PNG bytes.
///
/// Held to the same rule as `/asset`, because a picture of a page is the page.
pub(crate) async fn file_cover_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FileCoverQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let plan = asset_access_plan(Path::new(&params.path), &uploads_dir, &TokioServerFs).await?;
    let bytes = ctx
        .file_cover(plan.canonical, params.max_edge)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        bytes,
    ))
}

/// The pages of a book, as PDF bytes.
///
/// `application/pdf` because that is what the body is: the reader hands it
/// straight to pdf.js. The path is the book's, not the rendering's — there is
/// no file for the rendering, and asking for it by the book's own path is what
/// lets the answer be cached against that book's identity.
pub(crate) async fn surrogate_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AssetQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let bytes = ctx
        .surrogate_bytes(std::path::PathBuf::from(&params.path))
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/pdf")],
        bytes,
    ))
}

#[derive(Serialize)]
pub(crate) struct PdfFontId {
    pub(crate) id: String,
}

pub(crate) async fn resolve_pdf_font_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<wilkes_core::pdf_edit::fonts::FontRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let id = ctx
        .resolve_pdf_font(request)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(PdfFontId { id }))
}

#[derive(Deserialize)]
pub(crate) struct PdfFontQuery {
    pub(crate) id: String,
}

pub(crate) async fn pdf_font_bytes_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PdfFontQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let bytes = ctx
        .pdf_font_bytes(params.id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "font/sfnt")],
        bytes,
    ))
}

#[derive(Deserialize)]
pub(crate) struct PdfEditExportRequest {
    pub(crate) source: String,
    pub(crate) export: wilkes_core::pdf_edit::EditExport,
}

/// A PDF with a reader's edits applied, as the bytes of a new document.
///
/// Returned rather than written anywhere: the browser asking has no path on
/// this machine to name, and is handed the result as a download instead.
pub(crate) async fn export_pdf_edits_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PdfEditExportRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    // The same rule that decides which files this server will serve decides
    // which it will read to edit.
    let plan = asset_access_plan(Path::new(&request.source), &uploads_dir, &TokioServerFs).await?;
    let bytes = ctx
        .export_pdf_edits(plan.canonical.clone(), request.export)
        .await
        .map_err(|e| server_err(format!("{e:#}")))?;
    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/pdf")],
        bytes,
    ))
}
