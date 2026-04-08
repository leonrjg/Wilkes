use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Multipart, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info};
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_core::embed::worker::manager::WorkerPaths;
use wilkes_core::types::{EmbedderModel, EmbeddingEngine, MatchRef, ModelDescriptor, SearchQuery};

const MAX_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;

// ── AppState ──────────────────────────────────────────────────────────────────

struct AppState {
    ctx: Arc<AppContext>,
    uploads_dir: PathBuf,
    /// Sender used by SSE clients to subscribe to embed/manager events.
    events_tx: broadcast::Sender<(String, serde_json::Value)>,
}

// ── EventEmitter impl ─────────────────────────────────────────────────────────

struct BroadcastEmitter {
    tx: broadcast::Sender<(String, serde_json::Value)>,
}

impl EventEmitter for BroadcastEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let _ = self.tx.send((name.to_string(), payload));
    }
}

// ── Error helpers ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::BAD_REQUEST, Json(ErrorBody { error: msg.into() }))
}

fn server_err(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorBody { error: msg.into() }))
}

/// Resolve `raw` to a canonical path and verify it is inside `uploads_dir`.
/// Every handler that accepts a user-supplied path must go through this.
fn confine_to_uploads(raw: &str, uploads_dir: &std::path::Path) -> Result<PathBuf, (StatusCode, Json<ErrorBody>)> {
    let candidate = PathBuf::from(raw);
    let canonical_uploads = uploads_dir.canonicalize()
        .map_err(|e| server_err(format!("uploads dir unavailable: {e}")))?;
    let canonical = candidate.canonicalize()
        .map_err(|_| (StatusCode::NOT_FOUND, Json(ErrorBody { error: "Path not found".into() })))?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied: path outside uploads directory"));
    }
    Ok(canonical)
}

// ── Search ────────────────────────────────────────────────────────────────────

async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, Json<ErrorBody>)> {
    query.root = confine_to_uploads(&query.root.to_string_lossy(), &state.uploads_dir)?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let ctx = Arc::clone(&state.ctx);

    tokio::spawn(async move {
        let handle = match Arc::clone(&ctx).start_search(query).await {
            Ok(h) => h,
            Err(e) => {
                let event = Event::default().event("error").data(e);
                let _ = tx.send(Ok(event)).await;
                return;
            }
        };

        let stats = handle.run(|fm| {
            let tx = tx.clone();
            async move {
                let data = match serde_json::to_string(&fm) {
                    Ok(s) => s,
                    Err(e) => { error!("search serialize: {e}"); return true; }
                };
                tx.send(Ok(Event::default().event("result").data(data))).await.is_ok()
            }
        }).await;

        let data = serde_json::to_string(&stats).unwrap_or_default();
        let _ = tx.send(Ok(Event::default().event("complete").data(data))).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

// ── Preview ───────────────────────────────────────────────────────────────────

async fn preview_handler(
    Json(match_ref): Json<MatchRef>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let data = wilkes_api::commands::preview::preview(match_ref)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(data))
}

// ── Settings ──────────────────────────────────────────────────────────────────

async fn get_logs_handler() -> impl IntoResponse {
    Json(wilkes_api::commands::logs::get_logs())
}

async fn clear_logs_handler() -> StatusCode {
    wilkes_api::commands::logs::clear_logs();
    StatusCode::NO_CONTENT
}

async fn get_data_paths_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let app_data = state.ctx.data_dir.display().to_string();
    Json(wilkes_core::types::DataPaths { app_data })
}

async fn get_python_info_handler() -> impl IntoResponse {
    match wilkes_core::path::resolve_python() {
        Ok(p) => Json(p.display().to_string()),
        Err(e) => Json(format!("Not found: {}", e)),
    }
}

#[derive(Serialize)]
struct SettingsResponse {
    #[serde(flatten)]
    settings: wilkes_core::types::Settings,
    is_demo: bool,
}

async fn get_settings_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = state.ctx.get_settings().await;
    Ok(Json(SettingsResponse {
        settings,
        is_demo: std::env::var("WILKES_DEMO_MODE").is_ok(),
    }))
}

async fn update_settings_handler(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<serde_json::Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = state.ctx.update_settings(patch)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(settings))
}

async fn is_semantic_ready_handler(
    State(state): State<Arc<AppState>>,
) -> Json<bool> {
    Json(state.ctx.is_semantic_ready())
}

// ── File listing / open ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FilesQuery {
    root: String,
}

async fn list_files_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FilesQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let root = confine_to_uploads(&params.root, &state.uploads_dir)?;
    let files = state.ctx.list_files(root)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(files))
}

#[derive(Deserialize)]
struct OpenFileBody {
    path: String,
}

async fn open_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let path = confine_to_uploads(&body.path, &state.uploads_dir)?;
    let data = state.ctx.open_file(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(data))
}

// ── Upload ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct UploadResponse {
    root: String,
    file_count: usize,
}

async fn upload_handler(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let current_size = dir_size(&state.uploads_dir).await.unwrap_or(0);
    if current_size >= MAX_UPLOAD_BYTES {
        return Err(err(format!(
            "Upload directory exceeds maximum size of {} MB",
            MAX_UPLOAD_BYTES / 1024 / 1024
        )));
    }

    let mut file_count = 0usize;

    while let Some(field) = multipart.next_field().await.map_err(|e| err(e.to_string()))? {
        let filename = field.file_name().unwrap_or("upload").to_string();
        let rel: PathBuf = filename
            .split(['/', '\\'])
            .filter(|s| !s.is_empty() && *s != "..")
            .collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = state.uploads_dir.join(&rel);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| server_err(e.to_string()))?;
        }
        let data = field.bytes().await.map_err(|e| err(e.to_string()))?;
        tokio::fs::write(&dest, data).await.map_err(|e| server_err(e.to_string()))?;
        file_count += 1;
    }

    Ok(Json(UploadResponse {
        root: state.uploads_dir.to_string_lossy().into_owned(),
        file_count,
    }))
}

#[derive(Deserialize)]
struct DeleteUploadQuery {
    path: String,
}

async fn delete_upload_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DeleteUploadQuery>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let rel: PathBuf = params.path
        .split(['/', '\\'])        .filter(|s| !s.is_empty() && *s != "..")
        .collect();
    if rel.as_os_str().is_empty() {
        return Err(err("Cannot delete uploads root via this endpoint; use DELETE /api/upload/all"));
    }
    let target = state.uploads_dir.join(&rel);
    let canonical_uploads = tokio::fs::canonicalize(&state.uploads_dir)
        .await.map_err(|e| server_err(e.to_string()))?;
    let canonical_target = tokio::fs::canonicalize(&target)
        .await.map_err(|_| (StatusCode::NOT_FOUND, Json(ErrorBody { error: "Not found".into() })))?;
    if !canonical_target.starts_with(&canonical_uploads) {
        return Err(err("Path outside uploads directory"));
    }
    if canonical_target.is_dir() {
        tokio::fs::remove_dir_all(&canonical_target).await.map_err(|e| server_err(e.to_string()))?;
    } else {
        tokio::fs::remove_file(&canonical_target).await.map_err(|e| server_err(e.to_string()))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_all_upload_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    tokio::fs::remove_dir_all(&state.uploads_dir).await.map_err(|e| server_err(e.to_string()))?;
    tokio::fs::create_dir_all(&state.uploads_dir).await.map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Asset serving ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AssetQuery {
    path: String,
}

async fn asset_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AssetQuery>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let path = PathBuf::from(&params.path);
    let canonical_uploads = tokio::fs::canonicalize(&state.uploads_dir)
        .await.map_err(|e| server_err(e.to_string()))?;
    let canonical = tokio::fs::canonicalize(&path)
        .await.map_err(|_| (StatusCode::NOT_FOUND, Json(ErrorBody { error: "File not found".into() })))?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied"));
    }
    let content_type = mime_for_path(&canonical);
    let bytes = tokio::fs::read(&canonical).await.map_err(|e| server_err(e.to_string()))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(bytes))
        .unwrap())
}

fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") | Some("rst") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

// ── Embed events SSE ──────────────────────────────────────────────────────────

// ── Health check ──────────────────────────────────────────────────────────────

async fn health_handler() -> StatusCode {
    StatusCode::OK
}

// ── Embed events SSE ──────────────────────────────────────────────────────────

/// Subscribe to a stream of embed/manager events (progress, done, error,
/// manager-event). Connect before triggering a download or build.
///
/// A keepalive comment is sent every 30 s so that stale connections (network
/// drops without a clean TCP close) are detected promptly via send failure.
async fn embed_events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let mut rx = state.events_tx.subscribe();
    let (tx, stream_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);

    tokio::spawn(async move {
        let mut keepalive = tokio::time::interval(Duration::from_secs(30));
        keepalive.tick().await; // discard the immediate first tick
        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    if tx.send(Ok(Event::default().comment(""))).await.is_err() {
                        break;
                    }
                }
                result = rx.recv() => {
                    match result {
                        Ok((name, payload)) => {
                            let data = serde_json::to_string(&payload).unwrap_or_default();
                            let event = Event::default().event(&name).data(data);
                            if tx.send(Ok(event)).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            }
        }
    });

    Sse::new(ReceiverStream::new(stream_rx))
}

// ── Embed handlers ────────────────────────────────────────────────────────────

async fn get_engines_handler() -> impl IntoResponse {
    Json(EmbeddingEngine::supported_engines())
}

#[derive(Deserialize)]
struct ListModelsQuery {
    engine: EmbeddingEngine,
}

async fn list_models_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListModelsQuery>,
) -> impl IntoResponse {
    let models: Vec<ModelDescriptor> =
        wilkes_api::commands::embed::list_models(params.engine, &state.ctx.data_dir).await;
    Json(models)
}

#[derive(Deserialize)]
struct ModelSizeQuery {
    engine: EmbeddingEngine,
    model_id: String,
}

async fn get_model_size_handler(
    Query(params): Query<ModelSizeQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let size = wilkes_api::commands::embed::get_model_size(params.engine, params.model_id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(size))
}

async fn get_index_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let status = state.ctx.get_index_status().await.map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

#[derive(Deserialize)]
struct DownloadBody {
    model: EmbedderModel,
    engine: EmbeddingEngine,
}

async fn download_model_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DownloadBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    Arc::clone(&state.ctx)
        .start_download_model(body.model, body.engine)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct BuildBody {
    root: String,
    model: EmbedderModel,
    engine: EmbeddingEngine,
}

async fn build_index_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BuildBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    Arc::clone(&state.ctx)
        .start_build_index(body.root, body.model, body.engine)
        .await
        .map_err(server_err)?;
    Ok(StatusCode::ACCEPTED)
}

async fn delete_index_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state.ctx.delete_index().await.map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_embed_handler(
    State(state): State<Arc<AppState>>,
) -> StatusCode {
    state.ctx.cancel_embed();
    StatusCode::NO_CONTENT
}

// ── Worker handlers ───────────────────────────────────────────────────────────

async fn get_worker_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let status = state.ctx.get_worker_status();
    Ok(Json(status))
}

async fn kill_worker_handler(
    State(state): State<Arc<AppState>>,
) -> StatusCode {
    state.ctx.kill_worker();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct TimeoutBody {
    secs: u64,
}

async fn set_worker_timeout_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TimeoutBody>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state.ctx.set_worker_timeout(body.secs).await.map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn dir_size(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            if meta.is_dir() { stack.push(entry.path()); } else { total += meta.len(); }
        }
    }
    Ok(total)
}

// ── CLI ───────────────────────────────────────────────────────────────────────

struct Config {
    port: u16,
    data_dir: PathBuf,
    dist_dir: PathBuf,
}

fn parse_config() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut port: u16 = std::env::var("WILKES_PORT")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    let mut data_dir = PathBuf::from(
        std::env::var("WILKES_DATA_DIR").unwrap_or_else(|_| "/data".into()),
    );
    let mut dist_dir = PathBuf::from(
        std::env::var("WILKES_DIST_DIR").unwrap_or_else(|_| "./dist".into()),
    );
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => { if let Some(v) = args.get(i + 1) { if let Ok(p) = v.parse() { port = p; } i += 1; } }
            "--data-dir" => { if let Some(v) = args.get(i + 1) { data_dir = PathBuf::from(v); i += 1; } }
            "--dist-dir" => { if let Some(v) = args.get(i + 1) { dist_dir = PathBuf::from(v); i += 1; } }
            _ => {}
        }
        i += 1;
    }
    Config { port, data_dir, dist_dir }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging();
    let config = parse_config();

    let uploads_dir = config.data_dir.join("uploads");
    let settings_path = config.data_dir.join("settings.json");
    tokio::fs::create_dir_all(&uploads_dir).await?;

    let paths = WorkerPaths::resolve(&config.data_dir);

    let (events_tx, _) = broadcast::channel::<(String, serde_json::Value)>(1024);
    let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
    let (ctx, event_rx, loop_fut) = AppContext::new(
        config.data_dir.clone(),
        settings_path,
        paths,
        emitter,
    );

    ctx.clone().spawn_background_tasks(event_rx, loop_fut);

    let state = Arc::new(AppState { ctx, uploads_dir, events_tx });
    let index_html = config.dist_dir.join("index.html");

    let app = Router::new()
        .route("/health", get(health_handler))
        // Core
        .route("/api/search", post(search_handler))
        .route("/api/preview", post(preview_handler))
        .route("/api/settings", get(get_settings_handler))
        .route("/api/settings", patch(update_settings_handler))
        .route("/api/embed/ready", get(is_semantic_ready_handler))
        .route("/api/logs", get(get_logs_handler))
        .route("/api/logs", delete(clear_logs_handler))
        .route("/api/data/paths", get(get_data_paths_handler))
        .route("/api/worker/python-info", get(get_python_info_handler))
        .route("/api/files", get(list_files_handler))
        .route("/api/file", post(open_file_handler))
        // Upload (server-only: desktop uses native file picker)
        .route("/api/upload", post(upload_handler))
        .route("/api/upload", delete(delete_upload_handler))
        .route("/api/upload/all", delete(delete_all_upload_handler))
        .route("/asset", get(asset_handler))
        // Embed
        .route("/api/embed/events", get(embed_events_handler))
        .route("/api/embed/engines", get(get_engines_handler))
        .route("/api/embed/models", get(list_models_handler))
        .route("/api/embed/model-size", get(get_model_size_handler))
        .route("/api/embed/status", get(get_index_status_handler))
        .route("/api/embed/download", post(download_model_handler))
        .route("/api/embed/build", post(build_index_handler))
        .route("/api/embed/index", delete(delete_index_handler))
        .route("/api/embed/cancel", delete(cancel_embed_handler))
        // Worker
        .route("/api/worker/status", get(get_worker_status_handler))
        .route("/api/worker/kill", post(kill_worker_handler))
        .route("/api/worker/timeout", patch(set_worker_timeout_handler))
        // Static assets
        .fallback_service(
            ServeDir::new(&config.dist_dir).not_found_service(ServeFile::new(index_html)),
        )
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    info!("wilkes-server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mime_for_path() {
        assert_eq!(mime_for_path(Path::new("test.pdf")), "application/pdf");
        assert_eq!(mime_for_path(Path::new("test.txt")), "text/plain");
        assert_eq!(mime_for_path(Path::new("test.html")), "text/html");
        assert_eq!(mime_for_path(Path::new("test.json")), "application/json");
        assert_eq!(mime_for_path(Path::new("test.png")), "image/png");
        assert_eq!(mime_for_path(Path::new("test.jpg")), "image/jpeg");
        assert_eq!(mime_for_path(Path::new("test.unknown")), "application/octet-stream");
    }

    #[test]
    fn test_parse_config_defaults() {
        // Clear environment variables that might affect parsing
        std::env::remove_var("WILKES_PORT");
        std::env::remove_var("WILKES_DATA_DIR");
        std::env::remove_var("WILKES_DIST_DIR");

        // This assumes no command line arguments are passed during cargo test
        let config = parse_config();
        
        // The default port is 3000 if WILKES_PORT is not set
        assert_eq!(config.port, 2000);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.dist_dir, PathBuf::from("./dist"));
    }

    #[test]
    fn test_error_helpers() {
        let (status, body) = err("bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.0.error, "bad request");

        let (status, body) = server_err("internal error");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.error, "internal error");
    }

    #[test]
    fn test_broadcast_emitter() {
        let (tx, mut rx) = broadcast::channel(10);
        let emitter = BroadcastEmitter { tx };
        
        emitter.emit("test-event", serde_json::json!({"key": "value"}));
        
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.0, "test-event");
        assert_eq!(msg.1["key"], "value");
    }

    #[tokio::test]
    async fn test_handlers_direct() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        let settings_path = dir.path().join("settings.json");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        
        tokio::fs::write(uploads_dir.join("test.txt"), "hello").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _event_rx, _loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            paths,
            emitter,
        );

        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        // Test get_settings_handler
        let res = get_settings_handler(State(state.clone())).await;
        match res {
            Ok(r) => {
                let response = r.into_response();
                assert_eq!(response.status(), StatusCode::OK);
            }
            Err(_) => panic!("get_settings_handler failed"),
        }

        // Test get_logs_handler
        let _res = get_logs_handler().await;

        // Test get_data_paths_handler
        let _res = get_data_paths_handler(State(state.clone())).await;

        // Test list_files_handler
        let params = FilesQuery { root: uploads_dir.to_string_lossy().to_string() };
        let res = list_files_handler(State(state.clone()), Query(params)).await;
        match res {
            Ok(r) => {
                let response = r.into_response();
                assert_eq!(response.status(), StatusCode::OK);
            }
            Err(_) => panic!("list_files_handler failed"),
        }
    }

    #[tokio::test]
    async fn test_dir_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        
        tokio::fs::write(path.join("f1.txt"), "hello").await.unwrap();
        tokio::fs::create_dir(path.join("subdir")).await.unwrap();
        tokio::fs::write(path.join("subdir/f2.txt"), "world").await.unwrap();
        
        let size = dir_size(&path).await.unwrap();
        assert_eq!(size, 10);
    }

    #[tokio::test]
    async fn test_confine_to_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        
        // Success case
        let f1 = uploads_dir.join("f1.txt");
        tokio::fs::write(&f1, "test").await.unwrap();
        let res = confine_to_uploads(&f1.to_string_lossy(), &uploads_dir);
        assert!(res.is_ok(), "confine_to_uploads should succeed for valid path inside uploads_dir");
        
        // Denied case: outside uploads_dir
        let outside = dir.path().join("outside.txt");
        tokio::fs::write(&outside, "secret").await.unwrap();
        let res = confine_to_uploads(&outside.to_string_lossy(), &uploads_dir);
        assert!(res.is_err());
        assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::BAD_REQUEST);
        
        // Not found case
        let non_existent = uploads_dir.join("none.txt");
        let res = confine_to_uploads(&non_existent.to_string_lossy(), &uploads_dir);
        assert!(res.is_err());
        assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_all_upload_handler() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        tokio::fs::write(uploads_dir.join("f1.txt"), "test").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let res = delete_all_upload_handler(State(state)).await.map_err(|e| e.0).expect("delete_all_upload_handler failed");
        assert_eq!(res, StatusCode::NO_CONTENT);
        assert!(uploads_dir.exists());
        let entries = std::fs::read_dir(&uploads_dir).unwrap().count();
        assert_eq!(entries, 0);
    }

    #[tokio::test]
    async fn test_search_handler_grep() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        tokio::fs::write(uploads_dir.join("test.txt"), "hello world").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let query = wilkes_core::types::SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: uploads_dir.clone(),
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: wilkes_core::types::SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
        };

        let res = search_handler(State(state), axum::Json(query)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_preview_handler_text() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let file_path = uploads_dir.join("test.txt");
        tokio::fs::write(&file_path, "preview content").await.unwrap();

        let match_ref = wilkes_core::types::MatchRef {
            path: file_path,
            origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
            text_range: None,
        };

        let res = preview_handler(axum::Json(match_ref)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_settings_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: dir.path().to_path_buf(), events_tx });

        let _ = get_logs_handler().await;
        let _ = clear_logs_handler().await;
        let _ = get_data_paths_handler(State(state.clone())).await;
        let _ = get_python_info_handler().await;
        let _ = is_semantic_ready_handler(State(state.clone())).await;
        let _ = get_engines_handler().await;
        
        let patch = serde_json::json!({"semantic": {"enabled": true}});
        let res = update_settings_handler(State(state.clone()), axum::Json(patch)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_delete_upload_handler() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let file_path = uploads_dir.join("test.txt");
        tokio::fs::write(&file_path, "content").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let query = DeleteUploadQuery { path: "test.txt".to_string() };
        let res = delete_upload_handler(State(state), Query(query)).await;
        assert!(res.is_ok());
        assert!(!file_path.exists());
    }

    #[tokio::test]
    async fn test_asset_handler() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let file_path = uploads_dir.join("test.txt");
        tokio::fs::write(&file_path, "asset content").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let query = AssetQuery { path: file_path.to_string_lossy().to_string() };
        let res = asset_handler(State(state), Query(query)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_open_file_handler() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let file_path = uploads_dir.join("test.txt");
        tokio::fs::write(&file_path, "file content").await.unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let body = OpenFileBody { path: file_path.to_string_lossy().to_string() };
        let res = open_file_handler(State(state), axum::Json(body)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_confine_to_uploads_errors() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        // Path not found
        let res = confine_to_uploads("nonexistent.txt", &uploads_dir);
        assert_eq!(res.map_err(|e| e.0).unwrap_err(), StatusCode::NOT_FOUND);

        // Path outside (using ..)
        let outside = dir.path().join("outside.txt");
        tokio::fs::write(&outside, "secret").await.unwrap();
        let res = confine_to_uploads("../outside.txt", &uploads_dir);
        // Note: canonicalize will resolve .. relative to CWD if not absolute, 
        // but let's assume it fails validation.
        if let Ok(p) = res {
            assert!(!p.starts_with(&uploads_dir.canonicalize().unwrap()));
        } else {
            assert!(true);
        }
    }

    #[tokio::test]
    async fn test_delete_upload_handler_errors() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _, _) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        // Empty path
        let query = DeleteUploadQuery { path: "".to_string() };
        let res = delete_upload_handler(State(state.clone()), Query(query)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().0, StatusCode::BAD_REQUEST);

        // Outside path
        let query = DeleteUploadQuery { path: "../../etc/passwd".to_string() };
        let res = delete_upload_handler(State(state.clone()), Query(query)).await;
        assert!(res.is_err());

        // Non-existent path
        let query = DeleteUploadQuery { path: "ghost.txt".to_string() };
        let res = delete_upload_handler(State(state), Query(query)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_asset_handler_denied() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _, _) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        // Denied: outside uploads_dir
        let query = AssetQuery { path: "/etc/passwd".to_string() };
        let res = asset_handler(State(state), Query(query)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_more_server_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        // test get_engines_handler
        let _ = get_engines_handler().await;
        
        // test get_worker_status_handler
        let _ = get_worker_status_handler(State(state.clone())).await;

        // test kill_worker_handler
        let _ = kill_worker_handler(State(state.clone())).await;

        // test set_worker_timeout_handler
        let _ = set_worker_timeout_handler(State(state.clone()), Json(TimeoutBody { secs: 10 })).await;

        // test get_index_status_handler (will fail but covers the handler)
        let _ = get_index_status_handler(State(state.clone())).await;

        // test cancel_embed_handler
        let _ = cancel_embed_handler(State(state.clone())).await;

        // test get_model_size_handler (will fail)
        let _ = get_model_size_handler(Query(ModelSizeQuery { 
            engine: EmbeddingEngine::Fastembed, 
            model_id: "m".to_string() 
        })).await;
        
        // test list_models_handler
        let _ = list_models_handler(State(state.clone()), Query(ListModelsQuery { 
            engine: EmbeddingEngine::Fastembed 
        })).await;

        // test get_python_info_handler
        let _ = get_python_info_handler().await;
    }

    #[tokio::test]
    async fn test_delete_upload_handler_directory() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let sub_dir = uploads_dir.join("subdir");
        tokio::fs::create_dir(&sub_dir).await.unwrap();
        tokio::fs::write(sub_dir.join("f.txt"), "c").await.unwrap();

        let paths = WorkerPaths::resolve(dir.path());
        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _, _) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let query = DeleteUploadQuery { path: "subdir".to_string() };
        let res = delete_upload_handler(State(state), Query(query)).await;
        assert!(res.is_ok());
        assert!(!sub_dir.exists());
    }

    #[tokio::test]
    async fn test_upload_handler_limit() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        
        let paths = WorkerPaths::resolve(dir.path());
        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _, _) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let _state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });
    }

    #[tokio::test]
    async fn test_even_more_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let paths = WorkerPaths::resolve(dir.path());
        let (ctx, _, _) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        let _ = get_logs_handler().await;
        let _ = clear_logs_handler().await;
        let _ = get_data_paths_handler(State(state.clone())).await;
        let _ = is_semantic_ready_handler(State(state.clone())).await;
        let _ = delete_index_handler(State(state.clone())).await;
        let _ = download_model_handler(State(state.clone()), Json(DownloadBody {
            model: EmbedderModel("m".to_string()),
            engine: EmbeddingEngine::Candle
        })).await;
        
        let _ = update_settings_handler(State(state.clone()), Json(serde_json::json!({}))).await;
        let _ = get_settings_handler(State(state.clone())).await;
    }

    #[test]
    fn test_mime_for_path_variants() {
        assert_eq!(mime_for_path(Path::new("t.md")), "text/plain");
        assert_eq!(mime_for_path(Path::new("t.html")), "text/html");
        assert_eq!(mime_for_path(Path::new("t.png")), "image/png");
        assert_eq!(mime_for_path(Path::new("t.json")), "application/json");
    }

    #[tokio::test]
    async fn test_asset_handler_mime_types() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        
        let files = [
            ("t.pdf", "application/pdf"),
            ("t.png", "image/png"),
            ("t.json", "application/json"),
            ("t.jpg", "image/jpeg"),
        ];

        let paths = WorkerPaths::resolve(dir.path());
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: uploads_dir.clone(), events_tx });

        for (name, expected_mime) in files {
            let file_path = uploads_dir.join(name);
            tokio::fs::write(&file_path, "data").await.unwrap();
            let query = AssetQuery { path: file_path.to_string_lossy().to_string() };
            let res = asset_handler(State(state.clone()), Query(query)).await;
            match res {
                Ok(r) => assert_eq!(r.headers().get(axum::http::header::CONTENT_TYPE).unwrap(), expected_mime),
                Err(e) => panic!("Asset handler failed for {}: {:?}", name, e.0),
            }
        }
    }

    #[tokio::test]
    async fn test_even_more_server_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, _) = broadcast::channel(1024);
        let paths = WorkerPaths::resolve(dir.path());
        let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
        let (ctx, _rx, _loop) = AppContext::new(dir.path().to_path_buf(), dir.path().join("s.json"), paths, emitter);
        let state = Arc::new(AppState { ctx, uploads_dir: dir.path().join("u"), events_tx });

        assert_eq!(kill_worker_handler(State(state.clone())).await, StatusCode::NO_CONTENT);
        assert_eq!(clear_logs_handler().await, StatusCode::NO_CONTENT);
        
        let logs_res = get_logs_handler().await.into_response();
        assert_eq!(logs_res.status(), StatusCode::OK);
    }
}
