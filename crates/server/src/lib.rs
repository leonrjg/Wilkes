//! The Wilkes HTTP API, as a library.
//!
//! It lives here rather than inside the binary because a workspace has exactly
//! one owner — whichever process opened its databases. Every other consumer has
//! to reach that process over HTTP instead of opening the same files a second
//! time, so the API cannot belong to `wilkes-server` alone: the desktop app
//! owns a workspace far more often than the server does, and it mounts
//! [`api_router`] on a loopback port to let those consumers in.
//!
//! [`api_router`] serves the API and nothing else. Serving the web UI's static
//! assets stays in the binary, the only shell with no UI of its own.

pub mod config;
pub mod http;
mod routes;

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::http::auth::{self, require_bearer_token, ApiToken};
use crate::http::errors::{consumer_anyhow_err, consumer_err, err, server_err, ErrorBody};
#[cfg(test)]
use crate::http::state::BroadcastEmitter;
use crate::http::state::{
    asset_access_plan, sanitize_relative_upload_path, upload_write_plan, AppState, ServerFs,
    TokioServerFs,
};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use wilkes_api::context::AppContext;
use wilkes_api::workspace::{
    corpus_spaces_remedy, describe_corpus_spaces, ConsumerScope, EnsureManagedEmbeddingSpace,
    EnsureManagedWorkspace, ManagedEmbeddingSpaceStatus, ManagedWorkspaceStatus, WorkspaceState,
    WorkspaceSummary,
};
use wilkes_core::completion::{CompletionFeedback, CompletionRequest};
use wilkes_core::consumer::{ConsumerError, ConsumerErrorCode};
use wilkes_core::embed::engines::remote::{EmbedderAddress, InstallEvent};
use wilkes_core::embed::ChunkRef;
use wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryInput;
use wilkes_core::types::{
    AddOutcome, BookmarkClustersQuery, ChunkTopicsQuery, CitationGraphQuery, CitationLinksQuery,
    CitationResult, CollectionValidation, DocumentMetadata, DocumentTagUpdate, EmbeddingEngine,
    IntegrationStatus, LiteratureSearchResult, MatchRef, NewBookmark, NewSavedWork,
    NewSmartCollection, NewTag, ReadingOrderQuery, RelatedDocumentsQuery, SearchLogEntry,
    SearchQuery, SelectedEmbedder, SmartCollection, Tag, UpdateSmartCollection, UpdateTag,
};
#[cfg(test)]
use wilkes_core::worker::manager::WorkerPaths;

use crate::routes::*;

// ── AppState ──────────────────────────────────────────────────────────────────

// ── Search ────────────────────────────────────────────────────────────────────

// ── Preview ───────────────────────────────────────────────────────────────────

// ── Editing a PDF ─────────────────────────────────────────────────────────────

// ── Settings ──────────────────────────────────────────────────────────────────

// ── Catalogue ────────────────────────────────────────────────────────────────
//
// Four wrappers. The operations themselves live in
// `wilkes_api::commands::catalogue`, so the desktop shell reaches the same
// mirror through the same code rather than through a second implementation of
// the sync loop.
//
// None of these routes goes through `managed_context`, and that is deliberate
// rather than an oversight. Every other consumer route is scoped by a corpus
// and an embedding space because it reads the user's own documents. A
// catalogue record is not the user's document and has no vectors — it
// describes something nobody here holds yet. There is no corpus to pin, so
// pinning one would be theatre.

// ── Feeds ────────────────────────────────────────────────────────────────────

/// A server with no workspace manager can serve no corpus, so there is no id
/// it could be said to disagree about.
fn workspace_manager_unavailable() -> (StatusCode, Json<ErrorBody>) {
    consumer_err(ConsumerError::new(
        ConsumerErrorCode::ManagedWorkspaceNotFound,
        "workspace manager is unavailable",
    ))
}

/// Render a corpus or request embedding space for an error message. A corpus
/// with no index has no space, which is a distinct state from disagreeing
/// about which space is in use.
/// The index behind a scope, for a route that is about to name passages.
///
/// Every `/api/chunks` route needs the same two things: the resolver's
/// verdict on the scope, and the refusal owed to an index whose chunk refs
/// are all null. Answering the second per route is how one of them ends up
/// returning a list of nulls that reads like a list of passages.
async fn addressable_context(
    state: &Arc<AppState>,
    scope: &ConsumerScope,
) -> Result<Arc<wilkes_api::context::AppContext>, (StatusCode, Json<ErrorBody>)> {
    let index = state.consumer_index(scope).await?;
    index.addressable_space_id().map_err(consumer_err)?;
    Ok(index.into_context())
}

// ── File listing / open ───────────────────────────────────────────────────────

// ── Upload ────────────────────────────────────────────────────────────────────

// ── Asset serving ─────────────────────────────────────────────────────────────

// ── Health check ──────────────────────────────────────────────────────────────

// ── App events SSE ────────────────────────────────────────────────────────────

/// Subscribe to the shared stream of server-pushed application events. Connect
/// before triggering any operation whose lifecycle arrives through events.
///
/// A keepalive comment is sent every 30 s so that stale connections (network
/// drops without a clean TCP close) are detected promptly via send failure.
async fn events_handler(
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

// ── Worker handlers ───────────────────────────────────────────────────────────

// ── Helpers ───────────────────────────────────────────────────────────────────

// ── Router ────────────────────────────────────────────────────────────────────

/// Every HTTP route Wilkes answers, over a state that already owns a
/// workspace. Both shells mount this: the binary behind static assets, the
/// desktop app on a loopback port. One definition, so a consumer cannot find
/// an endpoint on one and not the other.
pub fn api_router(state: Arc<AppState>, token: ApiToken) -> Router {
    // Both the headless server and the desktop's shared API serve the same
    // workspace-aware MCP. Test-only contexts without a registry expose no MCP.
    let mcp = state.workspaces.as_ref().map(|manager| {
        wilkes_agent::mcp::api_router(manager.clone()).route_layer(
            axum::middleware::from_fn_with_state(Arc::clone(&token), require_bearer_token),
        )
    });
    let login_token = Arc::clone(&token);
    let router = Router::new()
        // Answers 204 to a caller that holds the token, and 401 otherwise: how
        // the web interface learns whether to ask for it.
        .route("/api/session", get(|| async { StatusCode::NO_CONTENT }))
        // Core
        .route("/api/search", post(search_handler))
        .route("/api/related-documents", post(related_documents_handler))
        .route("/api/citation-links", post(citation_links_handler))
        .route("/api/reading-order", post(reading_order_handler))
        .route("/api/citation-graph", post(citation_graph_handler))
        .route(
            "/api/citation-graph/fetch-missing",
            post(fetch_missing_citations_handler),
        )
        .route("/api/preview", post(preview_handler))
        .route("/api/surrogate", get(surrogate_handler))
        .route("/api/pdf-edit/font", post(resolve_pdf_font_handler))
        .route("/api/pdf-edit/font-bytes", get(pdf_font_bytes_handler))
        .route(
            "/api/pdf-edit/export",
            // An export describes every moved area of every changed page; a
            // long cascade is far past the default body limit.
            post(export_pdf_edits_handler).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/api/comic/page", get(comic_page_handler))
        .route("/api/settings", get(get_settings_handler))
        .route("/api/settings", patch(update_settings_handler))
        .route(
            "/api/workspaces",
            get(list_workspaces_handler).post(create_workspace_handler),
        )
        .route(
            "/api/workspaces/{id}",
            patch(rename_workspace_handler).delete(delete_workspace_handler),
        )
        .route(
            "/api/workspaces/{id}/activate",
            post(switch_workspace_handler),
        )
        .route("/api/corpora", put(ensure_managed_workspace_handler))
        .route("/api/corpora/spaces", put(ensure_managed_space_handler))
        .route("/api/corpora/status", get(managed_workspace_status_handler))
        .route(
            "/api/corpora/documents/import",
            post(import_managed_document_handler).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/api/corpora/documents/snapshot",
            post(document_snapshot_handler),
        )
        .route("/api/corpora/documents/cover", post(document_cover_handler))
        .route("/api/chunks/resolve", post(chunks_resolve_handler))
        .route("/api/chunks/accumulate", post(chunks_accumulate_handler))
        .route(
            "/api/chunks/similarity",
            // The one endpoint whose *request* is large: MAX_SIMILARITY_PROBES
            // vectors of the index's dimension, spelled out as JSON floats.
            // 512 × 768 dims at ~13 bytes a float is a little over 5 MB, and
            // axum's 2 MB default silently turned the documented cap into a
            // 413 for anything past about half of it. A cap the transport
            // cannot carry is not a cap, it is a trap.
            post(chunks_similarity_handler).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/api/chunks/search",
            post(chunks_search_handler).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/api/catalogue/search", post(catalogue_search_handler))
        .route(
            "/api/literature/providers",
            get(literature_providers_handler),
        )
        .route("/api/literature/search", post(literature_search_handler))
        .route("/api/literature/acquire", post(literature_acquire_handler))
        .route("/api/literature/browse", post(literature_browse_handler))
        .route("/api/feeds", get(feeds_list_handler).post(feed_add_handler))
        .route("/api/feeds/discover", post(feed_discover_handler))
        .route("/api/feeds/refresh", post(feeds_refresh_handler))
        .route(
            "/api/feeds/refresh/cancel",
            post(feeds_refresh_cancel_handler),
        )
        .route("/api/feeds/items", get(feed_items_handler))
        .route("/api/feeds/items/{id}", post(feed_item_set_status_handler))
        .route(
            "/api/feeds/{id}",
            patch(feed_update_handler).delete(feed_remove_handler),
        )
        .route("/api/literature/suggest", post(literature_suggest_handler))
        .route("/api/literature/compose", post(literature_compose_handler))
        .route(
            "/api/literature/compose/cancel",
            post(literature_compose_cancel_handler),
        )
        .route("/api/literature/status", post(literature_status_handler))
        .route("/api/literature/lookup", post(literature_lookup_handler))
        .route("/api/catalogue/sync", post(catalogue_sync_handler))
        .route("/api/catalogue/remove", post(catalogue_remove_handler))
        .route("/api/catalogue/status", get(catalogue_status_handler))
        .route(
            "/api/catalogue/acquire-course",
            post(catalogue_acquire_course_handler),
        )
        .route("/api/corpora/backup", post(managed_backup_handler))
        .route("/api/corpora/restore", post(managed_restore_handler))
        .route("/api/bookmarks", get(list_bookmarks_handler))
        .route("/api/bookmarks", post(add_bookmark_handler))
        .route("/api/bookmarks/clusters", post(cluster_bookmarks_handler))
        .route("/api/topics/chunks", post(chunk_topics_handler))
        .route(
            "/api/topics/chunks/{request_id}",
            delete(cancel_chunk_topics_handler),
        )
        .route("/api/bookmarks/{id}", delete(remove_bookmark_handler))
        .route("/api/bookmarks/{id}", patch(update_bookmark_note_handler))
        .route(
            "/api/saved-works",
            get(list_saved_works_handler).post(save_work_handler),
        )
        .route(
            "/api/saved-works/acquired",
            post(mark_saved_work_acquired_handler),
        )
        .route(
            "/api/saved-works/{id}",
            patch(update_saved_work_note_handler).delete(remove_saved_work_handler),
        )
        .route("/api/tags", get(list_tags_handler).post(create_tag_handler))
        .route(
            "/api/tags/{id}",
            patch(update_tag_handler).delete(delete_tag_handler),
        )
        .route("/api/documents/tags", patch(update_document_tags_handler))
        .route(
            "/api/smart-collections",
            get(list_collections_handler).post(create_collection_handler),
        )
        .route(
            "/api/smart-collections/validate",
            post(validate_collection_handler),
        )
        .route(
            "/api/smart-collections/{id}",
            patch(update_collection_handler).delete(delete_collection_handler),
        )
        .route(
            "/api/search-log",
            get(list_search_log_handler).delete(clear_search_log_handler),
        )
        .route("/api/search-log/{id}", delete(delete_search_log_handler))
        .route(
            "/api/integrations/zotero/status",
            get(zotero_status_handler),
        )
        .route(
            "/api/integrations/zotero/add",
            post(zotero_add_item_handler),
        )
        .route(
            "/api/integrations/zotero/citation",
            post(zotero_generate_citation_handler),
        )
        .route(
            "/api/integrations/custom/summary",
            post(custom_integration_summary_handler),
        )
        .route(
            "/api/integrations/custom/authoring-prompt",
            get(custom_integration_authoring_prompt_handler),
        )
        .route(
            "/api/integrations/custom/probe",
            post(custom_integration_probe_handler),
        )
        .route("/api/embed/ready", get(is_semantic_ready_handler))
        .route("/api/embed/text", post(embed_text_handler))
        .route("/api/embed/query", post(embed_query_handler))
        .route("/api/export/chunks", post(export_chunks_handler))
        .route("/api/export/figures", post(export_figures_handler))
        .route("/api/chunks/figures", post(chunk_figures_handler))
        .route("/api/figure", post(figure_image_handler))
        .route("/api/export/outline", post(export_outline_handler))
        .route("/api/export/files", post(export_files_handler))
        .route("/api/generation/ready", get(is_generation_ready_handler))
        .route(
            "/api/generation/models",
            get(list_generation_models_handler),
        )
        .route(
            "/api/generation/models/size",
            get(generation_model_size_handler),
        )
        .route("/api/generation/load", post(load_generation_model_handler))
        .route(
            "/api/image-analysis/catalogue",
            get(image_recognizer_catalogue_handler),
        )
        .route(
            "/api/image-analysis/inventory",
            get(image_recognizer_inventory_handler),
        )
        .route(
            "/api/image-analysis/install",
            post(install_image_recognizer_handler),
        )
        .route(
            "/api/image-analysis/install-detector",
            post(install_layout_detector_handler),
        )
        .route(
            "/api/generation/explain-related",
            post(explain_related_document_handler),
        )
        .route(
            "/api/generation/summarize",
            post(summarize_document_handler),
        )
        .route(
            "/api/generation/summarize-results",
            post(summarize_search_results_handler),
        )
        .route(
            "/api/completion/{completion_id}",
            post(request_completion_handler).delete(cancel_completion_handler),
        )
        .route(
            "/api/completion/{completion_id}/feedback",
            post(completion_feedback_handler),
        )
        .route(
            "/api/completion/session",
            get(session_steering_handler).delete(reset_session_steering_handler),
        )
        .route("/api/document/save", post(save_document_handler))
        .route(
            "/api/http-requests",
            get(get_http_calls_handler).delete(clear_http_calls_handler),
        )
        .route("/api/logs", get(get_logs_handler))
        .route("/api/logs", delete(clear_logs_handler))
        .route("/api/data/paths", get(get_data_paths_handler))
        .route("/api/worker/python-envs", get(get_python_envs_handler))
        .route(
            "/api/worker/python-envs/remove",
            post(remove_python_envs_handler),
        )
        .route(
            "/api/models/custom/remove",
            post(remove_custom_model_handler),
        )
        .route("/api/files", get(list_files_handler))
        .route("/api/file", post(open_file_handler))
        .route("/api/file/rename", post(rename_file_handler))
        .route("/api/file/cover", get(file_cover_handler))
        .route("/api/file/metadata", post(get_file_metadata_handler))
        .route(
            "/api/file/metadata/resolve",
            post(resolve_file_metadata_handler),
        )
        .route(
            "/api/file/metadata/refresh",
            post(refresh_file_metadata_handler),
        )
        // Upload (server-only: desktop uses native file picker)
        .route(
            "/api/upload",
            post(upload_handler).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES as usize)),
        )
        .route("/api/upload", delete(delete_upload_handler))
        .route("/api/upload/all", delete(delete_all_upload_handler))
        .route("/asset", get(asset_handler))
        // Embed
        .route("/api/events", get(events_handler))
        .route(
            "/api/embed/capabilities",
            get(embedder_capabilities_handler),
        )
        .route("/api/embed/model-size", get(get_model_size_handler))
        .route("/api/embed/model-search", get(search_custom_models_handler))
        .route("/api/embed/status", get(get_index_status_handler))
        .route("/api/embed/download", post(download_model_handler))
        .route("/api/embed/install", post(install_model_handler))
        .route("/api/embed/build", post(build_index_handler))
        .route("/api/embed/activity", get(index_activity_handler))
        .route("/api/embed/coverage", post(index_coverage_handler))
        .route("/api/embed/continue", post(continue_index_job_handler))
        .route(
            "/api/embed/index-missing",
            post(index_missing_documents_handler),
        )
        .route(
            "/api/embed/retry-failed",
            post(retry_failed_documents_handler),
        )
        .route("/api/embed/index", delete(delete_index_handler))
        .route("/api/embed/cancel", delete(cancel_embed_handler))
        // Worker
        .route("/api/worker/status", get(get_worker_status_handler))
        .route("/api/worker/statuses", get(get_worker_statuses_handler))
        .route("/api/worker/kill", post(kill_worker_handler))
        .route("/api/worker/timeout", patch(set_worker_timeout_handler))
        // Everything above requires the token whenever one is set; the two
        // routes below never do, since one reports the server is up and the
        // other is how a browser presents the token in the first place.
        .route_layer(axum::middleware::from_fn_with_state(
            token,
            require_bearer_token,
        ))
        .route("/health", get(health_handler))
        .route(
            "/api/login",
            post(move |Json(body): Json<auth::LoginBody>| {
                auth::login(Arc::clone(&login_token), body)
            }),
        )
        .layer(CorsLayer::permissive())
        .layer(axum::middleware::from_fn(log_http_call))
        .with_state(state);
    match mcp {
        Some(mcp) => router.merge(mcp),
        None => router,
    }
}

// ── Serving ───────────────────────────────────────────────────────────────────

/// A running [`api_router`], and the handle that stops it.
///
/// Shells that already have a process and a runtime — the desktop app — need
/// to start and stop the API repeatedly as a setting is toggled, so binding
/// and shutdown live here with the router instead of being rebuilt by each
/// caller out of axum parts.
pub struct ApiRuntime {
    addr: std::net::SocketAddr,
    token: ApiToken,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl ApiRuntime {
    /// Where it is actually listening. Worth reading rather than reconstructing
    /// from the requested port: port 0 binds an arbitrary free port, and this
    /// is the only place that knows which one.
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The token this listener requires, if it requires one.
    pub fn token(&self) -> Option<String> {
        self.token
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Require `token` from now on — or, given `None`, no token — without
    /// dropping the listener.
    pub fn set_token(&self, token: Option<String>) {
        *self
            .token
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = token;
    }

    /// Stops accepting, lets in-flight requests finish, and waits for the
    /// listener to actually be gone — so a caller that restarts on a settings
    /// change cannot race its own predecessor for the port.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        if let Err(e) = self.task.await {
            tracing::warn!("wilkes API listener did not stop cleanly: {e}");
        }
    }
}

/// Binds the API on `bind_address:port` and serves it until
/// [`ApiRuntime::shutdown`].
///
/// Binding is awaited here rather than inside the spawned task so that "the
/// port is taken" is returned to the caller as an error instead of surfacing
/// as a listener that silently never came up. With `token`, every route but
/// `/health` and the login requires it; without, the API is open to whoever
/// can reach `bind_address`.
pub async fn start_api(
    bind_address: std::net::IpAddr,
    port: u16,
    state: Arc<AppState>,
    token: Option<String>,
) -> anyhow::Result<ApiRuntime> {
    anyhow::ensure!(port != 0, "HTTP API port must be between 1 and 65535");
    let token: ApiToken = Arc::new(std::sync::RwLock::new(token));
    let listener = tokio::net::TcpListener::bind((bind_address, port)).await?;
    let addr = listener.local_addr()?;
    let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
    let app = api_router(state, Arc::clone(&token));
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
        if let Err(e) = served {
            tracing::error!("wilkes API listener stopped: {e}");
        }
    });
    Ok(ApiRuntime {
        addr,
        token,
        shutdown,
        task,
    })
}

#[cfg(test)]
mod tests;
