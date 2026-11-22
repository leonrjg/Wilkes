use crate::*;

pub(crate) fn confine_to_uploads(
    raw: &str,
    uploads_dir: &std::path::Path,
) -> Result<PathBuf, (StatusCode, Json<ErrorBody>)> {
    let candidate = PathBuf::from(raw);
    let canonical_uploads = uploads_dir
        .canonicalize()
        .map_err(|e| server_err(format!("uploads dir unavailable: {e}")))?;
    let canonical = candidate.canonicalize().map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                code: None,
                error: "Path not found".into(),
            }),
        )
    })?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied: path outside uploads directory"));
    }
    Ok(canonical)
}

pub(crate) const MAX_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;

#[derive(Deserialize)]
pub(crate) struct FilesQuery {
    pub(crate) root: String,
    pub(crate) collection_id: Option<String>,
    pub(crate) tag_ids: Option<String>,
    pub(crate) collection_expression: Option<String>,
}

pub(crate) async fn list_files_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FilesQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let root = confine_to_uploads(&params.root, &uploads_dir)?;
    let tag_ids = params
        .tag_ids
        .as_deref()
        .map(|value| {
            value
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let files = ctx
        .list_files_filtered(
            root,
            params.collection_id.as_deref(),
            &tag_ids,
            params.collection_expression.as_deref(),
        )
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(files))
}

#[derive(Deserialize)]
pub(crate) struct OpenFileBody {
    pub(crate) path: String,
}

#[derive(Deserialize)]
pub(crate) struct RenameFileBody {
    pub(crate) path: String,
    pub(crate) new_name: String,
}

#[derive(Deserialize)]
pub(crate) struct RefreshFileMetadataBody {
    pub(crate) path: Option<String>,
}

pub(crate) async fn open_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let data = ctx
        .open_file(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(data))
}

pub(crate) async fn rename_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RenameFileBody>,
) -> Result<Json<String>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let new_path = ctx
        .rename_file(path, body.new_name)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(new_path.display().to_string()))
}

pub(crate) async fn get_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<DocumentMetadata>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let metadata = ctx
        .get_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(metadata))
}

pub(crate) async fn zotero_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    let status = state
        .context()
        .zotero_status()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

pub(crate) async fn resolve_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<DocumentMetadata>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let metadata = ctx
        .resolve_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(metadata))
}

pub(crate) async fn refresh_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    body: Option<Json<RefreshFileMetadataBody>>,
) -> Result<Json<()>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = body
        .and_then(|Json(body)| body.path)
        .map(|path| confine_to_uploads(&path, &uploads_dir))
        .transpose()?;
    ctx.refresh_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(()))
}

pub(crate) async fn zotero_add_item_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<AddOutcome>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let outcome = ctx
        .zotero_add_item(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(outcome))
}

pub(crate) async fn zotero_generate_citation_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<CitationResult>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let citation = ctx
        .zotero_generate_citation(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(citation))
}

#[derive(Serialize)]
pub(crate) struct UploadResponse {
    pub(crate) root: String,
    pub(crate) file_count: usize,
}

pub(crate) async fn upload_handler(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    let current_size = TokioServerFs.dir_size(&uploads_dir).await.unwrap_or(0);
    if current_size >= MAX_UPLOAD_BYTES {
        return Err(err(format!(
            "Upload directory exceeds maximum size of {} MB",
            MAX_UPLOAD_BYTES / 1024 / 1024
        )));
    }

    let mut file_count = 0usize;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| err(e.to_string()))?
    {
        let filename = field.file_name().unwrap_or("upload").to_string();
        let rel = sanitize_relative_upload_path(&filename);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let plan = upload_write_plan(&uploads_dir, &rel);
        if let Some(parent) = plan.create_parent {
            TokioServerFs
                .create_dir_all(&parent)
                .await
                .map_err(|e| server_err(e.to_string()))?;
        }
        let data = field.bytes().await.map_err(|e| err(e.to_string()))?;
        TokioServerFs
            .write(&plan.dest, &data)
            .await
            .map_err(|e| server_err(e.to_string()))?;
        file_count += 1;
    }

    if file_count > 0 {
        ctx.register_library_root(&uploads_dir)
            .await
            .map_err(consumer_anyhow_err)?;
    }
    Ok(Json(UploadResponse {
        root: uploads_dir.to_string_lossy().into_owned(),
        file_count,
    }))
}

#[derive(Deserialize)]
pub(crate) struct DeleteUploadQuery {
    pub(crate) path: String,
}

pub(crate) async fn delete_upload_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DeleteUploadQuery>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    let requested = PathBuf::from(&params.path);
    if requested.as_os_str().is_empty() {
        return Err(err(
            "Cannot delete uploads root via this endpoint; use DELETE /api/upload/all",
        ));
    }
    let target = if requested.is_absolute() {
        requested
    } else {
        uploads_dir.join(sanitize_relative_upload_path(&params.path))
    };
    let canonical_uploads = TokioServerFs
        .canonicalize(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let canonical_target = TokioServerFs.canonicalize(&target).await.map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                code: None,
                error: "Not found".into(),
            }),
        )
    })?;
    if !canonical_target.starts_with(&canonical_uploads) {
        return Err(err("Path outside uploads directory"));
    }
    if canonical_target == canonical_uploads {
        return Err(err(
            "Cannot delete uploads root via this endpoint; use DELETE /api/upload/all",
        ));
    }
    if canonical_target.is_dir() {
        TokioServerFs
            .remove_dir_all(&canonical_target)
            .await
            .map_err(|e| server_err(e.to_string()))?;
    } else {
        TokioServerFs
            .remove_file(&canonical_target)
            .await
            .map_err(|e| server_err(e.to_string()))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_all_upload_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    TokioServerFs
        .remove_dir_all(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    TokioServerFs
        .create_dir_all(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct AssetQuery {
    pub(crate) path: String,
}

pub(crate) async fn asset_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AssetQuery>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let (_, uploads_dir) = state.workspace_snapshot();
    let plan = asset_access_plan(Path::new(&params.path), &uploads_dir, &TokioServerFs).await?;
    let bytes = TokioServerFs
        .read(&plan.canonical)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, plan.content_type)
        .body(Body::from(bytes))
        .unwrap())
}
