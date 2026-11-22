use crate::*;

pub(crate) async fn list_bookmarks_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmarks = state
        .context()
        .list_bookmarks()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmarks))
}

pub(crate) async fn add_bookmark_handler(
    State(state): State<Arc<AppState>>,
    Json(bookmark): Json<NewBookmark>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmark = state
        .context()
        .add_bookmark(bookmark)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmark))
}

pub(crate) async fn cluster_bookmarks_handler(
    State(state): State<Arc<AppState>>,
    Json(query): Json<BookmarkClustersQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let clusters = state
        .context()
        .clone()
        .cluster_bookmarks(query)
        .await
        .map_err(server_err)?;
    Ok(Json(clusters))
}

#[derive(Deserialize)]
pub(crate) struct ChunkTopicsBody {
    pub(crate) request_id: String,
    pub(crate) query: ChunkTopicsQuery,
}

pub(crate) async fn chunk_topics_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunkTopicsBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let mut query = body.query;
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;
    if let Some(path) = query.path.as_ref() {
        query.path = Some(confine_to_uploads(&path.to_string_lossy(), &uploads_dir)?);
    }
    let topics = ctx
        .clone()
        .chunk_topics(body.request_id, query)
        .await
        .map_err(server_err)?;
    Ok(Json(topics))
}

pub(crate) async fn cancel_chunk_topics_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> StatusCode {
    state.context().cancel_chunk_topics(&request_id);
    StatusCode::NO_CONTENT
}

pub(crate) async fn remove_bookmark_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .remove_bookmark(&id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct UpdateNoteBody {
    pub(crate) note: Option<String>,
}

pub(crate) async fn update_bookmark_note_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateNoteBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmark = state
        .context()
        .update_bookmark_note(&id, body.note)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmark))
}

pub(crate) async fn list_saved_works_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let works = state
        .context()
        .list_saved_works()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(works))
}

pub(crate) async fn save_work_handler(
    State(state): State<Arc<AppState>>,
    Json(work): Json<NewSavedWork>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let saved = state
        .context()
        .save_work(work)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(saved))
}

pub(crate) async fn remove_saved_work_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .remove_saved_work(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn update_saved_work_note_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateNoteBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let saved = state
        .context()
        .update_saved_work_note(&id, body.note)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(saved))
}

#[derive(Deserialize)]
pub(crate) struct SavedWorkAcquiredBody {
    pub(crate) provider: String,
    pub(crate) work: LiteratureSearchResult,
    pub(crate) path: String,
}

/// The path is one the web build fetched into its uploads directory, which is
/// the root it serves; anything else is not a file this server added.
pub(crate) async fn mark_saved_work_acquired_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SavedWorkAcquiredBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let saved = ctx
        .mark_saved_work_acquired(&body.provider, &body.work, path)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(saved))
}

pub(crate) async fn list_tags_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Tag>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_tags()
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn create_tag_handler(
    State(state): State<Arc<AppState>>,
    Json(tag): Json<NewTag>,
) -> Result<Json<Tag>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .create_tag(tag)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn update_tag_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(tag): Json<UpdateTag>,
) -> Result<Json<Tag>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .update_tag(&id, tag)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn delete_tag_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_tag(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn update_document_tags_handler(
    State(state): State<Arc<AppState>>,
    Json(mut update): Json<DocumentTagUpdate>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    update.paths = update
        .paths
        .iter()
        .map(|path| confine_to_uploads(&path.to_string_lossy(), &uploads_dir))
        .collect::<Result<Vec<_>, _>>()?;
    ctx.update_document_tags(update)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn list_collections_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SmartCollection>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_collections()
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn create_collection_handler(
    State(state): State<Arc<AppState>>,
    Json(collection): Json<NewSmartCollection>,
) -> Result<Json<SmartCollection>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .create_collection(collection)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn update_collection_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(collection): Json<UpdateSmartCollection>,
) -> Result<Json<SmartCollection>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .update_collection(&id, collection)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn delete_collection_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_collection(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct ValidateCollectionBody {
    pub(crate) expression: String,
}

pub(crate) async fn validate_collection_handler(
    Json(body): Json<ValidateCollectionBody>,
) -> Json<CollectionValidation> {
    Json(wilkes_api::research::ResearchStore::validate_collection(
        &body.expression,
    ))
}

#[derive(Deserialize)]
pub(crate) struct SearchLogQuery {
    pub(crate) limit: Option<usize>,
}

pub(crate) async fn list_search_log_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchLogQuery>,
) -> Result<Json<Vec<SearchLogEntry>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_search_log(query.limit.unwrap_or(100))
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

pub(crate) async fn delete_search_log_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_search_log(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn clear_search_log_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .clear_search_log()
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
