use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

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
use wilkes_core::embed::worker_manager::WorkerPaths;
use wilkes_core::path::is_under;
use wilkes_core::types::{EmbedderModel, EmbeddingEngine, MatchRef, ModelDescriptor, SearchQuery, SearchStats};

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

// ── Search ────────────────────────────────────────────────────────────────────

async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(query): Json<SearchQuery>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let ctx = Arc::clone(&state.ctx);

    tokio::spawn(async move {
        let mut handle = match Arc::clone(&ctx).start_search(query).await {
            Ok(h) => h,
            Err(e) => {
                let event = Event::default().event("error").data(e);
                let _ = tx.send(Ok(event)).await;
                return;
            }
        };

        let started = Instant::now();
        let mut total_matches = 0usize;
        let mut files_scanned = 0usize;

        while let Some(fm) = handle.next().await {
            total_matches += fm.matches.len();
            files_scanned += 1;
            let data = match serde_json::to_string(&fm) {
                Ok(s) => s,
                Err(e) => { error!("search serialize: {e}"); continue; }
            };
            if tx.send(Ok(Event::default().event("result").data(data))).await.is_err() {
                return;
            }
        }

        let errors = handle.finish().await;
        let stats = SearchStats {
            files_scanned,
            total_matches,
            elapsed_ms: started.elapsed().as_millis() as u64,
            errors,
        };
        let data = serde_json::to_string(&stats).unwrap_or_default();
        let _ = tx.send(Ok(Event::default().event("complete").data(data))).await;
    });

    Sse::new(ReceiverStream::new(rx))
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

async fn get_settings_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = wilkes_api::commands::settings::get_settings(&state.ctx.settings_path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(settings))
}

async fn update_settings_handler(
    State(state): State<Arc<AppState>>,
    Json(patch): Json<serde_json::Value>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = wilkes_api::commands::settings::update_settings(&state.ctx.settings_path, patch)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(settings))
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
    let root = PathBuf::from(&params.root);
    if !is_under(&root, &state.ctx.data_dir) {
        return Err(err("Path outside data directory"));
    }
    let settings = wilkes_api::commands::settings::get_settings(&state.ctx.settings_path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let files = wilkes_api::commands::files::list_files(root, settings.supported_extensions)
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
    let path = PathBuf::from(&body.path);
    if !is_under(&path, &state.ctx.data_dir) {
        return Err(err("Path outside data directory"));
    }
    let settings = wilkes_api::commands::settings::get_settings(&state.ctx.settings_path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let data = wilkes_api::commands::files::open_file(path, settings.supported_extensions)
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
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "..")
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

/// Subscribe to a stream of embed/manager events (progress, done, error,
/// manager-event). Connect before triggering a download or build.
async fn embed_events_handler(
    State(state): State<Arc<AppState>>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let mut rx = state.events_tx.subscribe();
    let (tx, stream_rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);

    tokio::spawn(async move {
        loop {
            match rx.recv().await {
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
        .map_err(|e| server_err(e))?;
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
        .map_err(|e| server_err(e))?;
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
    let status = state.ctx.get_worker_status().await.map_err(|e| server_err(e.to_string()))?;
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
        .ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
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

    let worker_bin = std::env::current_exe()?.with_file_name("wilkes-rust-worker");
    if !worker_bin.exists() {
        error!("Worker binary NOT FOUND at {}", worker_bin.display());
    }

    // WorkerPaths: Python fields are unused when only fastembed/candle engines
    // are active, but WorkerManager requires them to be present.
    let paths = WorkerPaths {
        python_path: PathBuf::new(),
        python_package_dir: PathBuf::new(),
        requirements_path: PathBuf::new(),
        venv_dir: config.data_dir.join("sbert_venv"),
        worker_bin,
    };

    let (events_tx, _) = broadcast::channel::<(String, serde_json::Value)>(1024);
    let emitter = Arc::new(BroadcastEmitter { tx: events_tx.clone() });
    let (ctx, event_rx, loop_fut) = AppContext::new(
        config.data_dir.clone(),
        settings_path,
        paths,
        emitter,
    );

    tokio::spawn(loop_fut);
    let ctx1 = Arc::clone(&ctx);
    tokio::spawn(async move { ctx1.run_event_forwarder(event_rx).await; });
    let ctx2 = Arc::clone(&ctx);
    tokio::spawn(async move { ctx2.restore_state().await; });

    let state = Arc::new(AppState { ctx, uploads_dir, events_tx });
    let index_html = config.dist_dir.join("index.html");

    let app = Router::new()
        // Core
        .route("/api/search", post(search_handler))
        .route("/api/preview", post(preview_handler))
        .route("/api/settings", get(get_settings_handler))
        .route("/api/settings", patch(update_settings_handler))
        .route("/api/logs", get(get_logs_handler))
        .route("/api/logs", delete(clear_logs_handler))
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
