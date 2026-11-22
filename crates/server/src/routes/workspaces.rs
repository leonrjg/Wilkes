use crate::*;

pub(crate) async fn get_settings_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = state.context().get_settings().await;
    Ok(Json(settings))
}

pub(crate) async fn update_settings_handler(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<serde_json::Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = state
        .context()
        .update_settings(patch)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
pub(crate) struct WorkspaceNameBody {
    pub(crate) name: String,
}

pub(crate) async fn list_workspaces_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<WorkspaceState>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    manager
        .state()
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}

pub(crate) async fn create_workspace_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WorkspaceNameBody>,
) -> Result<Json<WorkspaceSummary>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    manager
        .create(body.name)
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}

pub(crate) async fn rename_workspace_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(workspace_id): axum::extract::Path<String>,
    Json(body): Json<WorkspaceNameBody>,
) -> Result<Json<WorkspaceSummary>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    manager
        .rename(&workspace_id, body.name)
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}

pub(crate) async fn switch_workspace_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(workspace_id): axum::extract::Path<String>,
) -> Result<Json<WorkspaceState>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    // The web UI in front of `wilkes-server` is the primary view. In the
    // desktop app this is the main window, as activating through the API
    // always moved it.
    manager
        .switch_view(wilkes_api::workspace::PRIMARY_VIEW, &workspace_id)
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}

pub(crate) async fn delete_workspace_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(workspace_id): axum::extract::Path<String>,
) -> Result<Json<WorkspaceState>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    manager
        .delete(&workspace_id)
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}
