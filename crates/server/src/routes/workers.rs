use crate::*;

pub(crate) async fn get_worker_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let status = state.context().get_worker_status();
    Ok(Json(status))
}

pub(crate) async fn get_worker_statuses_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    Ok(Json(state.context().get_worker_statuses()))
}

pub(crate) async fn get_python_envs_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .python_environments()
        .await
        .map(Json)
        .map_err(|e| server_err(format!("{e:#}")))
}

#[derive(Deserialize)]
pub(crate) struct RemovePythonEnvsBody {
    pub(crate) ids: Vec<String>,
}

pub(crate) async fn remove_python_envs_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RemovePythonEnvsBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .remove_python_environments(body.ids)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

#[derive(Deserialize)]
pub(crate) struct RemoveCustomModelBody {
    pub(crate) engine: wilkes_core::types::EmbeddingEngine,
    pub(crate) model_id: String,
}

pub(crate) async fn remove_custom_model_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RemoveCustomModelBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .remove_custom_model(body.engine, body.model_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(server_err)
}

pub(crate) async fn kill_worker_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().kill_worker();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub(crate) struct TimeoutBody {
    pub(crate) secs: u64,
}

pub(crate) async fn set_worker_timeout_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TimeoutBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .set_worker_timeout(body.secs)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}
