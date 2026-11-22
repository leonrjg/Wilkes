use crate::*;

pub(crate) async fn get_http_calls_handler() -> impl IntoResponse {
    Json(wilkes_api::commands::logs::get_http_calls())
}

pub(crate) async fn clear_http_calls_handler() -> StatusCode {
    wilkes_api::commands::logs::clear_http_calls();
    StatusCode::NO_CONTENT
}

/// Browser calls are captured at the API boundary, in the same history as
/// outgoing calls. History polling/clearing is excluded to avoid log noise.
pub(crate) async fn log_http_call(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if request.uri().path() == "/api/http-requests" {
        return next.run(request).await;
    }
    let authority = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .unwrap_or("localhost");
    let url = format!("http://{authority}{}", request.uri());
    let guard =
        wilkes_core::http_history::CallGuard::start("HTTP API", request.method().as_str(), &url);
    let response = next.run(request).await;
    guard.finish(Some(response.status().as_u16()), None);
    response
}

pub(crate) async fn get_logs_handler() -> impl IntoResponse {
    Json(wilkes_api::commands::logs::get_logs())
}

pub(crate) async fn clear_logs_handler() -> StatusCode {
    wilkes_api::commands::logs::clear_logs();
    StatusCode::NO_CONTENT
}

pub(crate) async fn get_data_paths_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let ctx = state.context();
    Json(wilkes_core::types::DataPaths {
        app_data: ctx.shared_data_dir.display().to_string(),
        workspace: ctx.data_dir.display().to_string(),
    })
}

pub(crate) async fn health_handler() -> StatusCode {
    StatusCode::OK
}
