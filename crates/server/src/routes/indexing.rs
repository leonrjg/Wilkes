use crate::*;

pub(crate) async fn is_semantic_ready_handler(State(state): State<Arc<AppState>>) -> Json<bool> {
    Json(state.context().is_semantic_ready())
}

#[derive(Deserialize)]
pub(crate) struct ModelSizeQuery {
    pub(crate) engine: EmbeddingEngine,
    pub(crate) model_id: String,
}

#[derive(Deserialize)]
pub(crate) struct ModelSearchQuery {
    pub(crate) engine: EmbeddingEngine,
    pub(crate) query: String,
}

#[derive(Deserialize)]
pub(crate) struct RootQuery {
    pub(crate) root: Option<String>,
}

pub(crate) async fn get_model_size_handler(
    Query(params): Query<ModelSizeQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let size = wilkes_api::commands::embed::get_model_size(params.engine, params.model_id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(size))
}

pub(crate) async fn search_custom_models_handler(
    Query(params): Query<ModelSearchQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let hits = wilkes_api::commands::embed::search_custom_models(params.engine, params.query)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(hits))
}

pub(crate) async fn get_index_status_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RootQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let status = ctx
        .get_index_status(
            params
                .root
                .as_deref()
                .map(|root| confine_to_uploads(root, &uploads_dir))
                .transpose()?,
        )
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

/// Download a model into this machine's cache for another Wilkes that embeds
/// here, streaming `progress` events and ending with `done` or `error`.
///
/// Unlike `/api/embed/download`, which this instance's own interface uses, it
/// does not make the model this workspace's embedder and does not wait for or
/// block a build here. A caller that disconnects stops listening; a download
/// already writing a file finishes it, so what is left in the cache is the
/// model, never part of it.
pub(crate) async fn install_model_handler(
    State(state): State<Arc<AppState>>,
    Json(address): Json<EmbedderAddress>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (events_tx, mut events_rx) = tokio::sync::mpsc::channel::<InstallEvent>(64);
    let (stream_tx, stream_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let context = state.context();
    let install = tokio::spawn(async move { context.install_model(address, events_tx).await });
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            let sse = match &event {
                InstallEvent::Progress(progress) => Event::default()
                    .event("progress")
                    .json_data(progress)
                    .unwrap_or_else(|error| {
                        tracing::error!("install progress did not serialize: {error}");
                        Event::default().event("progress").data("{}")
                    }),
                InstallEvent::Done => Event::default().event("done").data(""),
                InstallEvent::Error(error) => Event::default().event("error").data(error),
            };
            if stream_tx.send(Ok(sse)).await.is_err() {
                // The caller has gone. The install task is dropped with the
                // receiver it reports to; an install step that cannot be
                // interrupted completes on its own.
                install.abort();
                break;
            }
        }
    });
    Sse::new(ReceiverStream::new(stream_rx))
}

#[derive(Deserialize)]
pub(crate) struct DownloadBody {
    pub(crate) selected: SelectedEmbedder,
}

pub(crate) async fn download_model_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DownloadBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .start_download_model(body.selected)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub(crate) struct BuildBody {
    pub(crate) root: String,
    pub(crate) selected: SelectedEmbedder,
}

#[derive(Deserialize)]
pub(crate) struct ActivityQuery {
    pub(crate) root: String,
}

pub(crate) async fn index_activity_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ActivityQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let root = confine_to_uploads(&params.root, &uploads_dir)?;
    let activity = ctx.index_activity(root).await.map_err(server_err)?;
    Ok(Json(activity))
}

#[derive(Deserialize)]
pub(crate) struct CoverageBody {
    pub(crate) roots: Vec<String>,
}

/// How much of each root the semantic index covers.
pub(crate) async fn index_coverage_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CoverageBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let roots = body
        .roots
        .iter()
        .map(|root| confine_to_uploads(root, &uploads_dir))
        .collect::<Result<Vec<_>, _>>()?;
    let coverage = ctx.index_coverage(roots).await.map_err(server_err)?;
    Ok(Json(coverage))
}

/// Index the documents the last job for this root never reached.
pub(crate) async fn continue_index_job_handler(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<BuildBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    body.root = confine_to_uploads(&body.root, &uploads_dir)?
        .to_string_lossy()
        .into_owned();
    ctx.continue_index_job(body.root, body.selected)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

/// Re-attempt the documents the last job for this root failed on.
pub(crate) async fn retry_failed_documents_handler(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<BuildBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    body.root = confine_to_uploads(&body.root, &uploads_dir)?
        .to_string_lossy()
        .into_owned();
    ctx.retry_failed_documents(body.root, body.selected)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

/// Index the documents under this root the semantic index does not hold.
pub(crate) async fn index_missing_documents_handler(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<BuildBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    body.root = confine_to_uploads(&body.root, &uploads_dir)?
        .to_string_lossy()
        .into_owned();
    ctx.index_missing_documents(body.root, body.selected)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn build_index_handler(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<BuildBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    body.root = confine_to_uploads(&body.root, &uploads_dir)?
        .to_string_lossy()
        .into_owned();
    ctx.start_build_index(body.root, body.selected)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_index_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<RootQuery>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.delete_index(
        params
            .root
            .as_deref()
            .map(|root| confine_to_uploads(root, &uploads_dir))
            .transpose()?,
    )
    .await
    .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn cancel_embed_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().cancel_embed().await;
    StatusCode::NO_CONTENT
}
