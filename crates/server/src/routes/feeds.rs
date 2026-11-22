use crate::*;

pub(crate) async fn feeds_list_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<wilkes_api::research::FeedRecord>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feeds_list()
        .map(Json)
        .map_err(|error| server_err(format!("{error:#}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedDiscoverBody {
    pub(crate) address: String,
}

/// The feeds an address offers, each read to be sure it is one.
pub(crate) async fn feed_discover_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FeedDiscoverBody>,
) -> Result<Json<Vec<wilkes_api::context::feeds::OfferedFeed>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_discover(&body.address)
        .await
        .map(Json)
        .map_err(|error| err(format!("{error:#}")))
}

/// Add a feed and refresh it. Its root must be one of the served library's.
pub(crate) async fn feed_add_handler(
    State(state): State<Arc<AppState>>,
    Json(feed): Json<wilkes_api::context::feeds::NewFeed>,
) -> Result<Json<wilkes_api::research::FeedRecord>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_add(feed)
        .await
        .map(Json)
        .map_err(|error| err(format!("{error:#}")))
}

pub(crate) async fn feed_update_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(update): Json<wilkes_api::context::feeds::FeedUpdate>,
) -> Result<Json<wilkes_api::research::FeedRecord>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_update(&id, update)
        .await
        .map(Json)
        .map_err(|error| err(format!("{error:#}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedRemoveQuery {
    #[serde(default)]
    pub(crate) delete_files: bool,
}

pub(crate) async fn feed_remove_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(query): Query<FeedRemoveQuery>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_remove(&id, query.delete_files)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| err(format!("{error:#}")))
}

pub(crate) async fn feeds_refresh_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feeds_refresh()
        .map(|()| StatusCode::ACCEPTED)
        .map_err(|error| err(format!("{error:#}")))
}

pub(crate) async fn feeds_refresh_cancel_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().feeds_refresh_cancel();
    StatusCode::NO_CONTENT
}

pub(crate) async fn feed_items_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<wilkes_api::context::feeds::FeedItemsQuery>,
) -> Result<Json<wilkes_api::context::feeds::FeedItemsPage>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_items(query)
        .map(Json)
        .map_err(|error| server_err(format!("{error:#}")))
}

pub(crate) async fn feed_item_set_status_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(action): Json<wilkes_api::context::feeds::FeedItemAction>,
) -> Result<Json<wilkes_api::research::FeedItemRecord>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .feed_item_set_status(&id, action)
        .await
        .map(Json)
        .map_err(|error| err(format!("{error:#}")))
}
