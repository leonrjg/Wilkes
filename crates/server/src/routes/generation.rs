use crate::*;

/// The backend half of the gate. The UI gate prevents the request; this one
/// makes the API honest if something calls it anyway — the server is reachable
/// without the desktop UI, so neither is redundant with the other.
pub(crate) async fn is_generation_ready_handler(State(state): State<Arc<AppState>>) -> Json<bool> {
    Json(state.context().is_generation_ready().await)
}

pub(crate) async fn list_generation_models_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<wilkes_core::types::GeneratorDescriptor>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_generation_models()
        .await
        .map(Json)
        .map_err(|e| server_err(format!("{e:#}")))
}

#[derive(Deserialize)]
pub(crate) struct GenerationModelSizeQuery {
    pub(crate) model_id: String,
}

pub(crate) async fn generation_model_size_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<GenerationModelSizeQuery>,
) -> Result<Json<u64>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .fetch_generation_model_size(&query.model_id)
        .await
        .map(Json)
        .map_err(|e| server_err(format!("{e:#}")))
}

pub(crate) async fn load_generation_model_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .load_generator()
        .await
        .map(|outcome| Json(outcome.attached()))
        .map_err(|e| server_err(format!("{e:#}")))
}

#[derive(Deserialize)]
pub(crate) struct ExplainRelatedBody {
    pub(crate) request_id: String,
    pub(crate) anchor_path: String,
    pub(crate) path: String,
}

pub(crate) async fn explain_related_document_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExplainRelatedBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let anchor_path = confine_to_uploads(&body.anchor_path, &uploads_dir)?;
    let related_path = confine_to_uploads(&body.path, &uploads_dir)?;
    ctx.explain_related_document(body.request_id, anchor_path, related_path)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct SummarizeDocumentBody {
    pub(crate) request_id: String,
    pub(crate) path: String,
}

pub(crate) async fn summarize_document_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SummarizeDocumentBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    ctx.summarize_document(body.request_id, path)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct SummarizeSearchResultsBody {
    pub(crate) request_id: String,
    pub(crate) input: SearchResultsSummaryInput,
}

pub(crate) async fn summarize_search_results_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SummarizeSearchResultsBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .summarize_search_results(body.request_id, body.input)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

pub(crate) async fn request_completion_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(completion_id): axum::extract::Path<String>,
    Json(mut request): Json<CompletionRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    request.path = confine_to_uploads(&request.path.to_string_lossy(), &uploads_dir)?;
    request.scope.pinned = request
        .scope
        .pinned
        .iter()
        .map(|path| confine_to_uploads(&path.to_string_lossy(), &uploads_dir))
        .collect::<Result<Vec<_>, _>>()?;
    request.scope.excluded = request
        .scope
        .excluded
        .iter()
        .map(|path| confine_to_uploads(&path.to_string_lossy(), &uploads_dir))
        .collect::<Result<Vec<_>, _>>()?;
    ctx.request_completion(completion_id, request)
        .await
        .map(|()| StatusCode::ACCEPTED)
        .map_err(server_err)
}

pub(crate) async fn cancel_completion_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(completion_id): axum::extract::Path<String>,
) -> StatusCode {
    state.context().cancel_completion(&completion_id);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub(crate) struct CompletionFeedbackBody {
    pub(crate) feedback: CompletionFeedback,
}

pub(crate) async fn completion_feedback_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(completion_id): axum::extract::Path<String>,
    Json(body): Json<CompletionFeedbackBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .completion_feedback(&completion_id, body.feedback)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

pub(crate) async fn session_steering_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::completion::SessionSteering> {
    Json(state.context().get_session_steering())
}

pub(crate) async fn reset_session_steering_handler(
    State(state): State<Arc<AppState>>,
) -> StatusCode {
    state.context().reset_session_steering();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub(crate) struct SaveDocumentBody {
    pub(crate) path: String,
    pub(crate) text: String,
}

pub(crate) async fn save_document_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SaveDocumentBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    ctx.save_document(path, body.text)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}
