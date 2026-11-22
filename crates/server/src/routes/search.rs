use crate::*;

pub(crate) async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(query): Json<SearchQuery>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, Json<ErrorBody>)> {
    crate::http::search::search_handler(state, query).await
}

pub(crate) async fn related_documents_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<RelatedDocumentsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;
    let related = ctx
        .clone()
        .related_documents(query)
        .await
        .map_err(server_err)?;
    Ok(Json(related))
}

pub(crate) async fn citation_links_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<CitationLinksQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;
    let links = ctx
        .clone()
        .citation_links(query)
        .await
        .map_err(server_err)?;
    Ok(Json(links))
}

/// A reading order is of one library folder, confined like any other search
/// root.
pub(crate) async fn reading_order_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<ReadingOrderQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;
    let order = ctx.reading_order(query).await.map_err(server_err)?;
    Ok(Json(order))
}

/// A citation graph is read from the workspace's library roots; a named root
/// is confined like any other search root.
pub(crate) async fn confine_citation_graph_query(
    state: &AppState,
    query: &mut CitationGraphQuery,
) -> Result<Arc<AppContext>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    if let Some(root) = query.root.take() {
        query.root = Some(
            crate::http::state::confined_root_for_search(
                &root.to_string_lossy(),
                &uploads_dir,
                &TokioServerFs,
            )
            .await?,
        );
    }
    Ok(ctx)
}

pub(crate) async fn citation_graph_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<CitationGraphQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let ctx = confine_citation_graph_query(&state, &mut query).await?;
    let graph = ctx.citation_graph(query).await.map_err(server_err)?;
    Ok(Json(graph))
}

pub(crate) async fn fetch_missing_citations_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<CitationGraphQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let ctx = confine_citation_graph_query(&state, &mut query).await?;
    let queued = ctx
        .fetch_missing_citations(query)
        .await
        .map_err(server_err)?;
    Ok(Json(serde_json::json!({ "queued": queued })))
}
