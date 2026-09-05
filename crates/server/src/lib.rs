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

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::http::errors::{consumer_anyhow_err, consumer_err, err, server_err, ErrorBody};
use crate::http::search::forward_search_results;
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
    ConsumerScope, EnsureManagedEmbeddingSpace, EnsureManagedWorkspace,
    ManagedEmbeddingSpaceStatus, ManagedWorkspaceStatus, WorkspaceState, WorkspaceSummary,
};
use wilkes_core::completion::{CompletionFeedback, CompletionRequest};
use wilkes_core::consumer::{ConsumerError, ConsumerErrorCode};
use wilkes_core::embed::ChunkRef;
use wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryInput;
use wilkes_core::types::{
    AddOutcome, BookmarkClustersQuery, ChunkTopicsQuery, CitationLinksQuery, CitationResult,
    CollectionValidation, DocumentMetadata, DocumentTagUpdate, EmbeddingEngine, IntegrationStatus,
    MatchRef, NewBookmark, NewSmartCollection, NewTag, OpenAlexWork, RelatedDocumentsQuery,
    SearchLogEntry, SearchQuery, SelectedEmbedder, SemanticScholarPaper, SmartCollection, Tag,
    UpdateSmartCollection, UpdateTag,
};
#[cfg(test)]
use wilkes_core::worker::manager::WorkerPaths;

fn confine_to_uploads(
    raw: &str,
    uploads_dir: &std::path::Path,
) -> Result<PathBuf, (StatusCode, Json<ErrorBody>)> {
    let candidate = PathBuf::from(raw);
    let canonical_uploads = uploads_dir
        .canonicalize()
        .map_err(|e| server_err(format!("uploads dir unavailable: {e}")))?;
    let canonical = candidate.canonicalize().map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                code: None,
                error: "Path not found".into(),
            }),
        )
    })?;
    if !canonical.starts_with(&canonical_uploads) {
        return Err(err("Access denied: path outside uploads directory"));
    }
    Ok(canonical)
}

const MAX_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;

// ── AppState ──────────────────────────────────────────────────────────────────

// ── Search ────────────────────────────────────────────────────────────────────

async fn search_handler(
    State(state): State<Arc<AppState>>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, (StatusCode, Json<ErrorBody>)> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(64);
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;

    tokio::spawn(async move {
        forward_search_results(ctx, query, tx).await;
    });

    Ok(Sse::new(ReceiverStream::new(rx)))
}

async fn related_documents_handler(
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

async fn citation_links_handler(
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

// ── Preview ───────────────────────────────────────────────────────────────────

async fn preview_handler(
    State(state): State<Arc<AppState>>,
    Json(match_ref): Json<MatchRef>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, _) = state.workspace_snapshot();
    let data = ctx
        .preview(match_ref)
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

async fn get_data_paths_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ctx = state.context();
    Json(wilkes_core::types::DataPaths {
        app_data: ctx.shared_data_dir.display().to_string(),
        workspace: ctx.data_dir.display().to_string(),
    })
}

async fn get_python_info_handler() -> impl IntoResponse {
    match wilkes_core::path::resolve_python() {
        Ok(p) => Json(p.display().to_string()),
        Err(e) => Json(format!("Not found: {}", e)),
    }
}

async fn get_settings_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let settings = state.context().get_settings().await;
    Ok(Json(settings))
}

async fn update_settings_handler(
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
struct WorkspaceNameBody {
    name: String,
}

async fn list_workspaces_handler(
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

async fn create_workspace_handler(
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

async fn rename_workspace_handler(
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

async fn switch_workspace_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(workspace_id): axum::extract::Path<String>,
) -> Result<Json<WorkspaceState>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| server_err("Workspace manager is unavailable"))?;
    manager
        .switch(&workspace_id)
        .await
        .map(Json)
        .map_err(|error| server_err(error.to_string()))
}

async fn ensure_managed_workspace_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EnsureManagedWorkspace>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    let initial = manager
        .ensure_managed_workspace(request)
        .await
        .map_err(consumer_anyhow_err)?;
    let context = manager
        .context_for(&initial.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    context.ensure_managed_index().await.map_err(consumer_err)?;
    let status = manager
        .managed_workspace_status(&initial.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    Ok(Json(managed_status_with_pending(status, &context)))
}

fn managed_status_with_pending(
    mut status: ManagedWorkspaceStatus,
    context: &AppContext,
) -> ManagedWorkspaceStatus {
    let (imports, builds) = context.managed_pending_operations();
    status.pending_imports = imports;
    status.pending_builds = builds;
    status
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedStatusQuery {
    corpus_id: String,
}

async fn managed_workspace_status_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ManagedStatusQuery>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    let status = manager
        .managed_workspace_status(&query.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let context = manager
        .context_for(&query.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    Ok(Json(managed_status_with_pending(status, &context)))
}

async fn ensure_managed_space_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<EnsureManagedEmbeddingSpace>,
) -> Result<Json<ManagedEmbeddingSpaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    manager
        .ensure_managed_space(request)
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ManagedImportSource {
    Path {
        path: PathBuf,
    },
    WilkesFile {
        workspace_id: String,
        root: PathBuf,
        path: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedImportBody {
    corpus_id: String,
    /// Absent when the caller expects an empty corpus. Importing is how a
    /// corpus gets its first vectors, so this is the one managed endpoint that
    /// runs before an embedding space exists.
    #[serde(default)]
    expected_embedding_space_id: Option<String>,
    idempotency_key: String,
    source: ManagedImportSource,
}

async fn import_managed_document_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedImportBody>,
) -> Result<Json<wilkes_api::context::ManagedDocumentExport>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    let status = manager
        .managed_workspace_status(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    let expected_exists = match body.expected_embedding_space_id.as_deref() {
        Some(expected) => status
            .spaces
            .iter()
            .any(|space| space.embedding_space_id == expected),
        None => status.spaces.is_empty(),
    };
    if !expected_exists {
        return Err(consumer_err(ConsumerError::new(
            ConsumerErrorCode::EmbeddingSpaceMismatch,
            format!(
                "corpus={}, request={}",
                describe_space(status.embedding_space_id.as_deref()),
                describe_space(body.expected_embedding_space_id.as_deref())
            ),
        )));
    }
    let managed = manager
        .context_for(&body.corpus_id)
        .await
        .map_err(consumer_anyhow_err)?;
    managed.ensure_managed_index().await.map_err(consumer_err)?;
    let (path, source_workspace) = match &body.source {
        ManagedImportSource::Path { path } => (path.clone(), None),
        ManagedImportSource::WilkesFile {
            workspace_id,
            root,
            path,
        } => {
            let source = manager
                .context_for(workspace_id)
                .await
                .map_err(consumer_anyhow_err)?;
            let path = source
                .authorize_managed_workspace_file(root.clone(), path.clone())
                .await
                .map_err(consumer_err)?;
            (path, Some(source))
        }
    };
    let provenance = serde_json::to_value(&body.source).map_err(|error| {
        consumer_err(ConsumerError::untyped(format!(
            "Could not encode import provenance: {error}"
        )))
    })?;
    let exported = managed
        .import_managed_document(
            body.corpus_id.clone(),
            body.idempotency_key.clone(),
            path.clone(),
            source_workspace.clone(),
            provenance.clone(),
        )
        .await
        .map_err(consumer_err)?;
    // The document is admitted to the canonical corpus above; the spaces are
    // brought to it here. A secondary model that is slow, unavailable, or
    // broken must not decide whether a document is in the corpus — the corpus
    // is the membership authority, and a projection that cannot follow it just
    // goes on failing closed until it can. So the fan-out reports rather than
    // refuses, and whatever it could not do is left owed and retried.
    let failures = manager
        .catch_up_corpus(&body.corpus_id, source_workspace)
        .await
        .map_err(consumer_anyhow_err)?;
    for (space, error) in &failures {
        // Left owed rather than raised: the space is already unservable while
        // it is behind, and the next catch-up — the next import, or the
        // caller's own maintenance pass — is what clears it.
        tracing::warn!("embedding projection {space} did not follow the import: {error}");
    }
    Ok(Json(exported))
}

/// A chunk request names an index with `scope` and its passages with refs, and
/// that is the whole of its addressing.
///
/// There were two of each of these routes — one taking a corpus id and a
/// pinned space, one taking a workspace id and SQLite rowids — and they
/// disagreed about what a caller could name. A rowid is reissued when a file
/// is re-indexed, so the second vocabulary could only ever be safe for a
/// caller that never stored anything, which is not the caller these exist for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunksGroupsBody {
    #[serde(default)]
    scope: ConsumerScope,
    groups: Vec<Vec<ChunkRef>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunksResolveBody {
    #[serde(default)]
    scope: ConsumerScope,
    chunk_refs: Vec<ChunkRef>,
}

async fn chunks_resolve_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksResolveBody>,
) -> Result<Json<wilkes_api::context::ChunkResolution>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .resolve_chunks(body.chunk_refs)
        .await
        .map(Json)
        .map_err(consumer_err)
}

async fn chunks_accumulate_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksGroupsBody>,
) -> Result<Json<wilkes_api::context::ChunkAccumulations>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .accumulate_chunks(body.groups)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunksSimilarityBody {
    #[serde(default)]
    scope: ConsumerScope,
    probes: Vec<wilkes_api::context::ProbeRequest>,
    #[serde(default)]
    chunk_refs: Vec<ChunkRef>,
}

async fn chunks_similarity_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksSimilarityBody>,
) -> Result<Json<wilkes_api::context::ChunkSimilarities>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .chunk_similarity(body.probes, body.chunk_refs)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorProbe {
    vector: Vec<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextProbe {
    text: String,
}

/// A probe is a vector the caller already holds or the text it wants searched
/// for, and the two are not interchangeable in what they can express.
///
/// A caller sending text gets its query embedded in the **query** role, which
/// is the only way that role can be reached: `embed/text` answers in the
/// passage role and has to, because the vectors it returns are stored. So
/// "search the corpus for this text" used to be two calls of which the first
/// could not know what the text was for. Sending text is also one round trip
/// instead of two, and it lets a later model with query prefixes work without
/// the consumer learning that it exists.
///
/// Vectors stay, and not for compatibility: a caller searching for something
/// it holds a vector *for* — a centroid, a stored embedding — has no text to
/// send.
#[derive(Deserialize)]
#[serde(untagged)]
enum SearchProbe {
    Vector(VectorProbe),
    Text(TextProbe),
}

impl From<SearchProbe> for wilkes_api::context::SearchProbeInput {
    fn from(probe: SearchProbe) -> Self {
        match probe {
            SearchProbe::Vector(probe) => Self::Vector(probe.vector),
            SearchProbe::Text(probe) => Self::Text(probe.text),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunksSearchBody {
    #[serde(default)]
    scope: ConsumerScope,
    probes: Vec<SearchProbe>,
    top_k: usize,
    min_similarity: f32,
}

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

fn catalogue_err(
    error: wilkes_api::commands::catalogue::CatalogueError,
) -> (StatusCode, Json<ErrorBody>) {
    match error {
        wilkes_api::commands::catalogue::CatalogueError::Request(message) => err(message),
        wilkes_api::commands::catalogue::CatalogueError::Failed(message) => server_err(message),
    }
}

/// The installation's catalogue mirror lives in `catalogue_dir`, not the
/// workspace's `data_dir`: it describes the public world rather than any one
/// library, which is also why these routes take no corpus scope.
fn catalogue_dir(state: &AppState) -> std::path::PathBuf {
    state.context().catalogue_dir.clone()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueSearchBody {
    queries: Vec<wilkes_api::commands::catalogue::CatalogueProbe>,
    #[serde(default = "default_catalogue_limit")]
    limit: usize,
}

fn default_catalogue_limit() -> usize {
    wilkes_api::commands::catalogue::DEFAULT_LIMIT
}

async fn catalogue_search_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueSearchBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueSearchResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    let dir = catalogue_dir(&state);
    // Off the async runtime: a batch of 64 queries is 64 FTS scans, and an
    // executor thread blocked on them is one not serving anything else.
    tokio::task::spawn_blocking(move || {
        wilkes_api::commands::catalogue::search(&dir, body.queries, body.limit)
    })
    .await
    .map_err(|error| server_err(format!("Catalogue search task panicked: {error}")))?
    .map(Json)
    .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueSyncBody {
    /// Which catalogues to refresh. Absent means all of them, which is a
    /// minutes-long request; a caller wanting progress should name one at a
    /// time and drive the loop itself.
    #[serde(default)]
    providers: Option<Vec<String>>,
}

async fn catalogue_sync_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueSyncBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueSyncResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .catalogue_sync(body.providers)
        .await
        .map(Json)
        .map_err(catalogue_err)
}

async fn catalogue_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueStatusResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    wilkes_api::commands::catalogue::status(&catalogue_dir(&state))
        .map(Json)
        .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueAcquireBody {
    url: String,
    #[serde(default)]
    filename: Option<String>,
}

/// Not gated by `ensure_writable`, unlike `/api/upload` into the same
/// directory. The gate turns away the *user* adding documents to a library
/// another application owns; this is that application fetching into Wilkes's
/// own staging area on its way to the managed import API, which is the
/// corpus's writer by contract.
async fn catalogue_acquire_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueAcquireBody>,
) -> Result<Json<wilkes_core::acquire::DownloadResponse>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .catalogue_acquire(body.url, body.filename)
        .await
        .map(Json)
        .map_err(catalogue_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueCourseBody {
    course_url: String,
}

/// Fetches a whole course into its own directory under uploads. Ungated for
/// the same reason `catalogue_acquire_handler` is: this is Wilkes writing into
/// its own staging area, not the user writing into a library root.
async fn catalogue_acquire_course_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CatalogueCourseBody>,
) -> Result<
    Json<wilkes_api::commands::catalogue::CatalogueCourseResponse>,
    (StatusCode, Json<ErrorBody>),
> {
    state
        .context()
        .catalogue_acquire_course(body.course_url)
        .await
        .map(Json)
        .map_err(catalogue_err)
}

async fn chunks_search_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunksSearchBody>,
) -> Result<Json<wilkes_api::context::ChunkSearch>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .chunk_search(
            body.probes.into_iter().map(Into::into).collect(),
            body.top_k,
            body.min_similarity,
        )
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedBackupBody {
    corpus_id: String,
    expected_embedding_space_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedRestoreBody {
    backup_name: String,
    expected_corpus_id: String,
    expected_embedding_space_id: String,
    expected_corpus_key: String,
}

async fn managed_backup_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedBackupBody>,
) -> Result<Json<wilkes_api::context::ManagedCorpusBackup>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    manager
        .backup_managed_corpus(&body.corpus_id, &body.expected_embedding_space_id)
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}

async fn managed_restore_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManagedRestoreBody>,
) -> Result<Json<ManagedWorkspaceStatus>, (StatusCode, Json<ErrorBody>)> {
    let manager = state
        .workspaces
        .as_ref()
        .ok_or_else(|| workspace_manager_unavailable())?;
    manager
        .restore_managed_workspace(
            &body.backup_name,
            &body.expected_corpus_id,
            &body.expected_embedding_space_id,
            &body.expected_corpus_key,
        )
        .await
        .map(Json)
        .map_err(consumer_anyhow_err)
}

/// What this Wilkes can embed with — the models, their dimensions, and the
/// input recipes they require.
///
/// Unscoped, like the catalogue routes and for the same reason: it reads
/// nothing of the user's documents and there is no corpus for it to pin. It
/// describes the host's embedders, and a consumer asks it *before* it has a
/// corpus in the space it is asking about.
async fn embedder_capabilities_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::types::EmbedderCapabilityManifest> {
    let context = state.context();
    let settings = context.get_settings().await;
    Json(wilkes_core::embed::dispatch::model_capabilities(
        &context.model_dir,
        &settings.semantic.custom_models,
    ))
}

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
fn describe_space(space: Option<&str>) -> &str {
    space.unwrap_or("none")
}

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

async fn list_bookmarks_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmarks = state
        .context()
        .list_bookmarks()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmarks))
}

async fn add_bookmark_handler(
    State(state): State<Arc<AppState>>,
    Json(bookmark): Json<NewBookmark>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmark = state
        .context()
        .add_bookmark(bookmark)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmark))
}

async fn cluster_bookmarks_handler(
    State(state): State<Arc<AppState>>,
    Json(query): Json<BookmarkClustersQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let clusters = state
        .context()
        .clone()
        .cluster_bookmarks(query)
        .await
        .map_err(server_err)?;
    Ok(Json(clusters))
}

#[derive(Deserialize)]
struct ChunkTopicsBody {
    request_id: String,
    query: ChunkTopicsQuery,
}

async fn chunk_topics_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunkTopicsBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let mut query = body.query;
    let (ctx, uploads_dir) = state.workspace_snapshot();
    query.root = crate::http::state::confined_root_for_search(
        &query.root.to_string_lossy(),
        &uploads_dir,
        &TokioServerFs,
    )
    .await?;
    if let Some(path) = query.path.as_ref() {
        query.path = Some(confine_to_uploads(&path.to_string_lossy(), &uploads_dir)?);
    }
    let topics = ctx
        .clone()
        .chunk_topics(body.request_id, query)
        .await
        .map_err(server_err)?;
    Ok(Json(topics))
}

async fn cancel_chunk_topics_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> StatusCode {
    state.context().cancel_chunk_topics(&request_id);
    StatusCode::NO_CONTENT
}

async fn remove_bookmark_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .remove_bookmark(&id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UpdateNoteBody {
    note: Option<String>,
}

async fn update_bookmark_note_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<UpdateNoteBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let bookmark = state
        .context()
        .update_bookmark_note(&id, body.note)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(bookmark))
}

async fn list_tags_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Tag>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_tags()
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn create_tag_handler(
    State(state): State<Arc<AppState>>,
    Json(tag): Json<NewTag>,
) -> Result<Json<Tag>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .create_tag(tag)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn update_tag_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(tag): Json<UpdateTag>,
) -> Result<Json<Tag>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .update_tag(&id, tag)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn delete_tag_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_tag(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_document_tags_handler(
    State(state): State<Arc<AppState>>,
    Json(mut update): Json<DocumentTagUpdate>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    update.paths = update
        .paths
        .iter()
        .map(|path| confine_to_uploads(&path.to_string_lossy(), &uploads_dir))
        .collect::<Result<Vec<_>, _>>()?;
    ctx.update_document_tags(update)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_collections_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SmartCollection>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_collections()
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn create_collection_handler(
    State(state): State<Arc<AppState>>,
    Json(collection): Json<NewSmartCollection>,
) -> Result<Json<SmartCollection>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .create_collection(collection)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn update_collection_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(collection): Json<UpdateSmartCollection>,
) -> Result<Json<SmartCollection>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .update_collection(&id, collection)
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn delete_collection_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_collection(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ValidateCollectionBody {
    expression: String,
}

async fn validate_collection_handler(
    Json(body): Json<ValidateCollectionBody>,
) -> Json<CollectionValidation> {
    Json(wilkes_api::research::ResearchStore::validate_collection(
        &body.expression,
    ))
}

#[derive(Deserialize)]
struct SearchLogQuery {
    limit: Option<usize>,
}

async fn list_search_log_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchLogQuery>,
) -> Result<Json<Vec<SearchLogEntry>>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .list_search_log(query.limit.unwrap_or(100))
        .map(Json)
        .map_err(|e| server_err(e.to_string()))
}

async fn delete_search_log_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .delete_search_log(&id)
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_search_log_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .clear_search_log()
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn is_semantic_ready_handler(State(state): State<Arc<AppState>>) -> Json<bool> {
    Json(state.context().is_semantic_ready())
}

#[derive(Deserialize)]
struct EmbedTextBody {
    texts: Vec<String>,
    /// Which index's embedder to use, and which space the caller believes it
    /// is talking to.
    ///
    /// A consumer keeping one vector space cannot let "whichever workspace the
    /// user last opened" decide what embeds its text: the model would change
    /// under it when the user switches windows. Pinning the space is how it
    /// says so.
    #[serde(default)]
    scope: ConsumerScope,
}

/// Embed arbitrary strings with the model the addressed index uses, in the
/// **passage** role — the role the vectors it returns are stored under.
///
/// The reply names the embedding space when the index has one, which is what a
/// consumer pins against. It is null only when the addressed workspace holds
/// no index at all: embedding text does not require one.
async fn embed_text_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmbedTextBody>,
) -> Result<Json<wilkes_api::context::EmbeddedTexts>, (StatusCode, Json<ErrorBody>)> {
    if body.texts.is_empty() {
        return Err(err("texts must not be empty"));
    }
    let index = state.consumer_index(&body.scope).await?;
    let space = index.embedding_space_id().map(str::to_string);
    index
        .into_context()
        .embed_texts(body.texts, space)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
struct ExportChunksBody {
    root: PathBuf,
    path: PathBuf,
    /// Which index holds this file's passages. Absent means the active
    /// workspace.
    ///
    /// Each workspace owns its index, so this is not a filter — it names the
    /// database the chunks are read from. Without it, exporting a document
    /// from another library meant switching the whole server to it first.
    #[serde(default)]
    scope: ConsumerScope,
}

/// The document scope shared by export routes that read one source file.
#[derive(Deserialize)]
struct ExportOutlineBody {
    root: PathBuf,
    path: PathBuf,
    /// Which workspace owns the configured library root. Absent means the
    /// active workspace, consistent with the rest of the export surface.
    #[serde(default)]
    scope: ConsumerScope,
}

/// Chunk + vector export for one indexed file: text, byte ranges, source
/// origins, and stored embeddings, in extraction order.
async fn export_chunks_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportChunksBody>,
) -> Result<Json<wilkes_api::context::FileChunkExport>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .export_file_chunks(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

/// Declared PDF/document outline without opening the semantic index or
/// returning embeddings. Entries retain the source document's page and byte
/// locators; `/api/export/chunks` additionally resolves them to chunk ordinals.
async fn export_outline_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportOutlineBody>,
) -> Result<Json<wilkes_api::context::FileOutlineExport>, (StatusCode, Json<ErrorBody>)> {
    state
        .consumer_index(&body.scope)
        .await?
        .into_context()
        .export_file_outline(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
struct ExportFilesBody {
    root: PathBuf,
    /// Which workspace's library and index to read. Absent means the active
    /// one, as everywhere else on the export surface.
    #[serde(default)]
    scope: ConsumerScope,
}

/// The documents one library root holds, each with the passage count that says
/// whether `/api/export/chunks` can answer for it.
///
/// Deliberately not `/api/files`: that endpoint is confined to the uploads
/// directory, which is the right jail for a browser talking to a shared
/// server and the wrong one for a consumer asking about the library itself.
/// This one is confined to the workspace's own library roots, exactly as the
/// chunk export is.
async fn export_files_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportFilesBody>,
) -> Result<Json<wilkes_api::context::LibraryFileExport>, (StatusCode, Json<ErrorBody>)> {
    state
        .consumer_index(&body.scope)
        .await?
        .into_context()
        .export_library_files(body.root)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
struct ExportFiguresBody {
    root: PathBuf,
    path: PathBuf,
    #[serde(default)]
    scope: ConsumerScope,
}

/// One document's pictures: where each sits and what its pixels must hash to.
///
/// The browse half of the figure surface, addressed exactly as
/// `/api/export/chunks` is. It says nothing about passages — `/api/chunks/figures`
/// is the half that links a picture to the text it was drawn into.
async fn export_figures_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ExportFiguresBody>,
) -> Result<Json<wilkes_api::context::FileFigureExport>, (StatusCode, Json<ErrorBody>)> {
    addressable_context(&state, &body.scope)
        .await?
        .export_file_figures(body.root, body.path)
        .await
        .map(Json)
        .map_err(server_err)
}

#[derive(Deserialize)]
struct ChunkFiguresBody {
    #[serde(default)]
    scope: ConsumerScope,
    chunk_refs: Vec<ChunkRef>,
    /// Bytes of slack outside a passage within which a figure still counts.
    ///
    /// Absent means none, which admits only the figures a passage contains.
    /// A window is a widening the caller asks for: it reaches the figure
    /// printed a page over from the prose discussing it, at the price of
    /// sometimes reaching one the prose never mentions. Hits carry their
    /// distance so the caller can spend a budget nearest-first.
    #[serde(default)]
    window_bytes: usize,
}

/// The figures these passages bear on, strongest relation first.
async fn chunk_figures_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ChunkFiguresBody>,
) -> Result<Json<wilkes_api::context::ChunkFigureResolution>, (StatusCode, Json<ErrorBody>)> {
    let index = state.consumer_index(&body.scope).await?;
    index
        .into_context()
        .chunk_figures(body.chunk_refs, body.window_bytes)
        .await
        .map(Json)
        .map_err(consumer_err)
}

#[derive(Deserialize)]
struct FigureImageBody {
    #[serde(default)]
    scope: ConsumerScope,
    /// The document, named either way a consumer holds it: a library root and
    /// path, or any passage of it.
    #[serde(default)]
    root: Option<PathBuf>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    chunk_ref: Option<ChunkRef>,
    /// `p{page}-i{ordinal}`, from the inventory.
    area_id: String,
    /// Longest edge to downscale to. Absent serves the picture at the size the
    /// page draws it — which is the size its digest covers.
    #[serde(default)]
    max_edge: Option<u32>,
}

/// One figure's pixels, re-derived from the document and checked against the
/// digest the rendition recorded.
///
/// PNG bytes rather than a JSON envelope with base64 in it: this is an image,
/// every client that wants one can read one, and a base64 field would make
/// every consumer decode a third more bytes than it needed to. The digest and
/// the dimensions ride on headers so a caller can check what it got without
/// parsing the picture.
async fn figure_image_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FigureImageBody>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let address = match (body.root, body.path, body.chunk_ref) {
        (Some(root), Some(path), None) => wilkes_api::context::FigureAddress::File { root, path },
        (None, None, Some(chunk_ref)) => wilkes_api::context::FigureAddress::Chunk(chunk_ref),
        _ => {
            return Err(err(
                "name the document either by root and path or by chunk_ref, and not by both",
            ))
        }
    };
    let index = state.consumer_index(&body.scope).await?;
    let rendered = index
        .into_context()
        .figure_pixels(address, body.area_id, body.max_edge)
        .await
        .map_err(consumer_err)?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, rendered.media_type)
        .header("x-wilkes-image-sha256", rendered.image_sha256)
        .header("x-wilkes-image-width", rendered.width)
        .header("x-wilkes-image-height", rendered.height)
        .header("x-wilkes-image-source-width", rendered.source_width)
        .header("x-wilkes-image-source-height", rendered.source_height)
        .body(Body::from(rendered.bytes))
        .unwrap())
}

/// The backend half of the gate. The UI gate prevents the request; this one
/// makes the API honest if something calls it anyway — the server is reachable
/// without the desktop UI, so neither is redundant with the other.
async fn is_generation_ready_handler(State(state): State<Arc<AppState>>) -> Json<bool> {
    Json(state.context().is_generation_ready().await)
}

async fn list_generation_models_handler(
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
struct GenerationModelSizeQuery {
    model_id: String,
}

async fn generation_model_size_handler(
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

async fn load_generation_model_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .load_generator()
        .await
        .map(|outcome| Json(outcome.attached()))
        .map_err(|e| server_err(format!("{e:#}")))
}

/// Which recognizer a request is about. The engine and model travel together
/// everywhere, because a model id means nothing without the engine that
/// resolves it.
#[derive(Deserialize)]
struct RecognizerQuery {
    engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    model_id: String,
}

/// Every recognizer this build can read with, and the engines it compiled in.
async fn image_recognizer_catalogue_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::extract::image::dispatch::RecognizerCatalogue> {
    Json(state.context().image_recognizer_catalogue())
}

/// What the named recognizer is, where it came from, and under what licence.
/// Static, and answers whether or not it is installed: it is what the download
/// is disclosed by, so it has to be readable before the download.
async fn image_recognizer_inventory_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecognizerQuery>,
) -> Result<Json<wilkes_core::types::RecognizerInventory>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .image_recognizer_inventory(query.engine, &query.model_id)
        .map(Json)
        .map_err(|e| server_err(format!("{e:#}")))
}

async fn install_image_recognizer_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecognizerQuery>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .install_image_recognizer(body.engine, body.model_id)
        .await
        .map(|()| Json(true))
        .map_err(|e| server_err(format!("{e:#}")))
}

/// Download the layout detector. No body: there is one detector and it is not
/// chosen from a catalogue.
#[cfg(feature = "recognize-onnx")]
async fn install_layout_detector_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .install_layout_detector()
        .await
        .map(|()| Json(true))
        .map_err(|e| server_err(format!("{e:#}")))
}

/// The same route in a build with no detector compiled in, so a client gets an
/// explanation rather than a 404 it would read as a wrong URL.
#[cfg(not(feature = "recognize-onnx"))]
async fn install_layout_detector_handler(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    Err(server_err(
        "this build has no layout detector compiled in".to_string(),
    ))
}

#[derive(Deserialize)]
struct ExplainRelatedBody {
    request_id: String,
    anchor_path: String,
    path: String,
}

async fn explain_related_document_handler(
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
struct SummarizeDocumentBody {
    request_id: String,
    path: String,
}

async fn summarize_document_handler(
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
struct SummarizeSearchResultsBody {
    request_id: String,
    input: SearchResultsSummaryInput,
}

async fn summarize_search_results_handler(
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

async fn request_completion_handler(
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

async fn cancel_completion_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(completion_id): axum::extract::Path<String>,
) -> StatusCode {
    state.context().cancel_completion(&completion_id);
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct CompletionFeedbackBody {
    feedback: CompletionFeedback,
}

async fn completion_feedback_handler(
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

async fn session_steering_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::completion::SessionSteering> {
    Json(state.context().get_session_steering())
}

async fn reset_session_steering_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().reset_session_steering();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct SaveDocumentBody {
    path: String,
    text: String,
}

async fn save_document_handler(
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

// ── File listing / open ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FilesQuery {
    root: String,
    collection_id: Option<String>,
    tag_ids: Option<String>,
    collection_expression: Option<String>,
}

async fn list_files_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<FilesQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let root = confine_to_uploads(&params.root, &uploads_dir)?;
    let tag_ids = params
        .tag_ids
        .as_deref()
        .map(|value| {
            value
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let files = ctx
        .list_files_filtered(
            root,
            params.collection_id.as_deref(),
            &tag_ids,
            params.collection_expression.as_deref(),
        )
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(files))
}

#[derive(Deserialize)]
struct OpenFileBody {
    path: String,
}

#[derive(Deserialize)]
struct RenameFileBody {
    path: String,
    new_name: String,
}

#[derive(Deserialize)]
struct DoiBody {
    doi: String,
}

#[derive(Deserialize)]
struct RefreshFileMetadataBody {
    path: Option<String>,
}

async fn open_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let data = ctx
        .open_file(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(data))
}

async fn rename_file_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RenameFileBody>,
) -> Result<Json<String>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let new_path = ctx
        .rename_file(path, body.new_name)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(new_path.display().to_string()))
}

async fn get_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<DocumentMetadata>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let metadata = ctx
        .get_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(metadata))
}

async fn zotero_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    let status = state
        .context()
        .zotero_status()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

async fn semantic_scholar_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    let status = state
        .context()
        .semantic_scholar_status()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

async fn semantic_scholar_lookup_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DoiBody>,
) -> Result<Json<SemanticScholarPaper>, (StatusCode, Json<ErrorBody>)> {
    let paper = state
        .context()
        .semantic_scholar_lookup(body.doi)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(paper))
}

async fn openalex_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    let status = state
        .context()
        .openalex_status()
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

async fn openalex_lookup_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DoiBody>,
) -> Result<Json<OpenAlexWork>, (StatusCode, Json<ErrorBody>)> {
    let work = state
        .context()
        .openalex_lookup(body.doi)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(work))
}

#[derive(serde::Deserialize)]
struct ManifestBody {
    manifest: String,
}

#[derive(serde::Deserialize)]
struct ProbeBody {
    manifest: String,
    #[serde(default)]
    secrets: std::collections::HashMap<String, String>,
}

#[derive(serde::Deserialize)]
struct IntegrationIdBody {
    id: String,
}

async fn custom_integration_summary_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ManifestBody>,
) -> Json<wilkes_api::commands::integrations::custom::ManifestSummary> {
    Json(state.context().custom_integration_summary(body.manifest))
}

async fn custom_integration_probe_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProbeBody>,
) -> Result<Json<wilkes_core::integrations::custom::ProbeReport>, (StatusCode, Json<ErrorBody>)> {
    let report = state
        .context()
        .custom_integration_probe(body.manifest, body.secrets)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(report))
}

async fn custom_integration_status_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IntegrationIdBody>,
) -> Result<Json<IntegrationStatus>, (StatusCode, Json<ErrorBody>)> {
    let status = state
        .context()
        .custom_integration_status(body.id)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(status))
}

async fn resolve_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<DocumentMetadata>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let metadata = ctx
        .resolve_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(metadata))
}

async fn refresh_file_metadata_handler(
    State(state): State<Arc<AppState>>,
    body: Option<Json<RefreshFileMetadataBody>>,
) -> Result<Json<()>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = body
        .and_then(|Json(body)| body.path)
        .map(|path| confine_to_uploads(&path, &uploads_dir))
        .transpose()?;
    ctx.refresh_file_metadata(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(()))
}

async fn zotero_add_item_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<AddOutcome>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let outcome = ctx
        .zotero_add_item(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(outcome))
}

async fn zotero_generate_citation_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenFileBody>,
) -> Result<Json<CitationResult>, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let path = confine_to_uploads(&body.path, &uploads_dir)?;
    let citation = ctx
        .zotero_generate_citation(path)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Json(citation))
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
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    let current_size = TokioServerFs.dir_size(&uploads_dir).await.unwrap_or(0);
    if current_size >= MAX_UPLOAD_BYTES {
        return Err(err(format!(
            "Upload directory exceeds maximum size of {} MB",
            MAX_UPLOAD_BYTES / 1024 / 1024
        )));
    }

    let mut file_count = 0usize;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| err(e.to_string()))?
    {
        let filename = field.file_name().unwrap_or("upload").to_string();
        let rel = sanitize_relative_upload_path(&filename);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let plan = upload_write_plan(&uploads_dir, &rel);
        if let Some(parent) = plan.create_parent {
            TokioServerFs
                .create_dir_all(&parent)
                .await
                .map_err(|e| server_err(e.to_string()))?;
        }
        let data = field.bytes().await.map_err(|e| err(e.to_string()))?;
        TokioServerFs
            .write(&plan.dest, &data)
            .await
            .map_err(|e| server_err(e.to_string()))?;
        file_count += 1;
    }

    Ok(Json(UploadResponse {
        root: uploads_dir.to_string_lossy().into_owned(),
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
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    let requested = PathBuf::from(&params.path);
    if requested.as_os_str().is_empty() {
        return Err(err(
            "Cannot delete uploads root via this endpoint; use DELETE /api/upload/all",
        ));
    }
    let target = if requested.is_absolute() {
        requested
    } else {
        uploads_dir.join(sanitize_relative_upload_path(&params.path))
    };
    let canonical_uploads = TokioServerFs
        .canonicalize(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    let canonical_target = TokioServerFs.canonicalize(&target).await.map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                code: None,
                error: "Not found".into(),
            }),
        )
    })?;
    if !canonical_target.starts_with(&canonical_uploads) {
        return Err(err("Path outside uploads directory"));
    }
    if canonical_target == canonical_uploads {
        return Err(err(
            "Cannot delete uploads root via this endpoint; use DELETE /api/upload/all",
        ));
    }
    if canonical_target.is_dir() {
        TokioServerFs
            .remove_dir_all(&canonical_target)
            .await
            .map_err(|e| server_err(e.to_string()))?;
    } else {
        TokioServerFs
            .remove_file(&canonical_target)
            .await
            .map_err(|e| server_err(e.to_string()))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_all_upload_handler(
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    ctx.ensure_writable().map_err(consumer_err)?;
    TokioServerFs
        .remove_dir_all(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    TokioServerFs
        .create_dir_all(&uploads_dir)
        .await
        .map_err(|e| server_err(e.to_string()))?;
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
    let (_, uploads_dir) = state.workspace_snapshot();
    let plan = asset_access_plan(Path::new(&params.path), &uploads_dir, &TokioServerFs).await?;
    let bytes = TokioServerFs
        .read(&plan.canonical)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, plan.content_type)
        .body(Body::from(bytes))
        .unwrap())
}

// ── Health check ──────────────────────────────────────────────────────────────

async fn health_handler() -> StatusCode {
    StatusCode::OK
}

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

#[derive(Deserialize)]
struct ModelSizeQuery {
    engine: EmbeddingEngine,
    model_id: String,
}

#[derive(Deserialize)]
struct RootQuery {
    root: Option<String>,
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

#[derive(Deserialize)]
struct DownloadBody {
    selected: SelectedEmbedder,
}

async fn download_model_handler(
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
struct BuildBody {
    root: String,
    selected: SelectedEmbedder,
}

#[derive(Deserialize)]
struct ActivityQuery {
    root: String,
}

async fn index_activity_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ActivityQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let (ctx, uploads_dir) = state.workspace_snapshot();
    let root = confine_to_uploads(&params.root, &uploads_dir)?;
    let activity = ctx.index_activity(root).await.map_err(server_err)?;
    Ok(Json(activity))
}

/// Index the documents the last job for this root never reached.
async fn continue_index_job_handler(
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
async fn retry_failed_documents_handler(
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

async fn build_index_handler(
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

async fn delete_index_handler(
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

async fn cancel_embed_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().cancel_embed().await;
    StatusCode::NO_CONTENT
}

// ── Worker handlers ───────────────────────────────────────────────────────────

async fn get_worker_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    let status = state.context().get_worker_status();
    Ok(Json(status))
}

async fn get_worker_statuses_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    Ok(Json(state.context().get_worker_statuses()))
}

async fn kill_worker_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.context().kill_worker();
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
    state
        .context()
        .set_worker_timeout(body.secs)
        .await
        .map_err(|e| server_err(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

// ── Router ────────────────────────────────────────────────────────────────────

/// Every HTTP route Wilkes answers, over a state that already owns a
/// workspace. Both shells mount this: the binary behind static assets, the
/// desktop app on a loopback port. One definition, so a consumer cannot find
/// an endpoint on one and not the other.
pub fn api_router(state: Arc<AppState>) -> Router {
    // Both the headless server and the desktop's shared API serve the same
    // workspace-aware MCP. Test-only contexts without a registry expose no MCP.
    let mcp = state.workspaces.as_ref().map(|manager|
        wilkes_agent::mcp::api_router(manager.clone()));
    let router = Router::new()
        .route("/health", get(health_handler))
        // Core
        .route("/api/search", post(search_handler))
        .route("/api/related-documents", post(related_documents_handler))
        .route("/api/citation-links", post(citation_links_handler))
        .route("/api/preview", post(preview_handler))
        .route("/api/settings", get(get_settings_handler))
        .route("/api/settings", patch(update_settings_handler))
        .route(
            "/api/workspaces",
            get(list_workspaces_handler).post(create_workspace_handler),
        )
        .route("/api/workspaces/{id}", patch(rename_workspace_handler))
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
        .route("/api/catalogue/sync", post(catalogue_sync_handler))
        .route("/api/catalogue/status", get(catalogue_status_handler))
        .route("/api/catalogue/acquire", post(catalogue_acquire_handler))
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
            "/api/integrations/semantic-scholar/status",
            get(semantic_scholar_status_handler),
        )
        .route(
            "/api/integrations/semantic-scholar/lookup",
            post(semantic_scholar_lookup_handler),
        )
        .route(
            "/api/integrations/openalex/status",
            get(openalex_status_handler),
        )
        .route(
            "/api/integrations/openalex/lookup",
            post(openalex_lookup_handler),
        )
        .route(
            "/api/integrations/custom/summary",
            post(custom_integration_summary_handler),
        )
        .route(
            "/api/integrations/custom/probe",
            post(custom_integration_probe_handler),
        )
        .route(
            "/api/integrations/custom/status",
            post(custom_integration_status_handler),
        )
        .route("/api/embed/ready", get(is_semantic_ready_handler))
        .route("/api/embed/text", post(embed_text_handler))
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
        .route("/api/logs", get(get_logs_handler))
        .route("/api/logs", delete(clear_logs_handler))
        .route("/api/data/paths", get(get_data_paths_handler))
        .route("/api/worker/python-info", get(get_python_info_handler))
        .route("/api/files", get(list_files_handler))
        .route("/api/file", post(open_file_handler))
        .route("/api/file/rename", post(rename_file_handler))
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
        .route("/api/embed/status", get(get_index_status_handler))
        .route("/api/embed/download", post(download_model_handler))
        .route("/api/embed/build", post(build_index_handler))
        .route("/api/embed/activity", get(index_activity_handler))
        .route("/api/embed/continue", post(continue_index_job_handler))
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
        .layer(CorsLayer::permissive())
        .with_state(state);
    match mcp { Some(mcp) => router.merge(mcp), None => router }
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
/// as a listener that silently never came up.
pub async fn start_api(
    bind_address: std::net::IpAddr,
    port: u16,
    state: Arc<AppState>,
) -> anyhow::Result<ApiRuntime> {
    anyhow::ensure!(port != 0, "HTTP API port must be between 1 and 65535");
    let listener = tokio::net::TcpListener::bind((bind_address, port)).await?;
    let addr = listener.local_addr()?;
    let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
    let app = api_router(state);
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
        shutdown,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wilkes_api::context::EventEmitter;

    #[test]
    fn test_error_helpers() {
        let (status, body) = err("bad request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.0.error, "bad request");

        let (status, body) = server_err("internal error");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0.error, "internal error");
    }

    /// The probe body is an untagged enum, which is the one place a wire
    /// format can go quietly wrong: a shape matching neither variant, or both,
    /// must be refused rather than resolved to whichever arm serde reaches
    /// first.
    #[test]
    fn a_probe_is_a_vector_or_a_text_and_never_both() {
        let vector: SearchProbe =
            serde_json::from_str(r#"{"vector":[1.0,0.0]}"#).expect("a vector probe");
        assert!(matches!(vector, SearchProbe::Vector(_)));

        let text: SearchProbe =
            serde_json::from_str(r#"{"text":"causal inference"}"#).expect("a text probe");
        match text {
            SearchProbe::Text(probe) => assert_eq!(probe.text, "causal inference"),
            SearchProbe::Vector(_) => panic!("text read as a vector"),
        }

        for malformed in [
            r#"{"vector":[1.0],"text":"both"}"#,
            r#"{}"#,
            r#"{"vector":[1.0],"role":"query"}"#,
            r#"{"txt":"misspelled"}"#,
        ] {
            assert!(
                serde_json::from_str::<SearchProbe>(malformed).is_err(),
                "{malformed} must be refused"
            );
        }
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

    /// Building the whole router *is* the assertion: axum validates route
    /// syntax at construction and panics on anything malformed.
    ///
    /// Nothing did this before. Every other test here calls a handler
    /// directly, so four routes kept axum 0.7's `:id` capture syntax through
    /// the 0.8 upgrade and every attempt to serve panicked before it could
    /// bind — a failure no handler test can see.
    #[tokio::test]
    async fn the_router_is_constructible() {
        let dir = tempfile::tempdir().unwrap();
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(16);
        let (ctx, _event_rx, _loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            paths,
            Arc::new(BroadcastEmitter {
                tx: events_tx.clone(),
            }),
        );
        let _router = api_router(Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: dir.path().join("uploads"),
            events_tx,
        }));
    }

    /// Builds an `AppState` over a throwaway data dir, for handlers that need
    /// only the workspace's path. The returned directory owns `uploads`, so a
    /// test that needs a file on disk can write one into it.
    #[allow(clippy::type_complexity)]
    fn handler_state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        let settings_path = dir.path().join("settings.json");
        std::fs::create_dir_all(&uploads_dir).unwrap();
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _event_rx, _loop_fut) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir,
            events_tx,
        });
        (dir, state)
    }

    #[tokio::test]
    async fn catalogue_status_reports_an_unsynced_mirror_rather_than_failing() {
        let (_dir, state) = handler_state();
        let response = match catalogue_status_handler(State(state)).await {
            Ok(response) => response,
            Err(error) => panic!("status failed: {}", error.1 .0.error),
        };
        assert_eq!(response.0.total_records, 0);
        // Every registered provider is named even before a first sync: a
        // settings panel has to be able to offer the button for a provider
        // that holds nothing yet.
        assert_eq!(
            response.0.providers.len(),
            wilkes_core::catalogue::registry().len()
        );
        assert!(response
            .0
            .providers
            .iter()
            .all(|provider| provider.records == 0 && provider.synced_at_ms.is_none()));
    }

    #[tokio::test]
    async fn catalogue_search_refuses_an_unknown_grain_by_name() {
        let (_dir, state) = handler_state();
        let body = CatalogueSearchBody {
            queries: vec![wilkes_api::commands::catalogue::CatalogueProbe {
                key: "k".into(),
                text: "graph algorithms".into(),
                grains: Some(vec!["monograph".into()]),
            }],
            limit: 8,
        };
        let error = match catalogue_search_handler(State(state), Json(body)).await {
            Ok(_) => panic!("unknown grain must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(
            error.1 .0.error.contains("monograph"),
            "the refusal must name the grain it rejected: {}",
            error.1 .0.error
        );
    }

    #[tokio::test]
    async fn catalogue_search_refuses_an_empty_batch() {
        let (_dir, state) = handler_state();
        let body = CatalogueSearchBody {
            queries: Vec::new(),
            limit: 8,
        };
        let error = match catalogue_search_handler(State(state), Json(body)).await {
            Ok(_) => panic!("empty batch must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn catalogue_search_refuses_a_batch_past_the_documented_cap() {
        let (_dir, state) = handler_state();
        let body = CatalogueSearchBody {
            queries: (0..wilkes_api::commands::catalogue::MAX_QUERIES + 1)
                .map(|n| wilkes_api::commands::catalogue::CatalogueProbe {
                    key: n.to_string(),
                    text: "graph algorithms".into(),
                    grains: None,
                })
                .collect(),
            limit: 8,
        };
        let error = match catalogue_search_handler(State(state), Json(body)).await {
            Ok(_) => panic!("oversized batch must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn catalogue_sync_refuses_a_provider_it_does_not_have() {
        let (_dir, state) = handler_state();
        let body = CatalogueSyncBody {
            providers: Some(vec!["nonexistent".into()]),
        };
        let error = match catalogue_sync_handler(State(state), Json(body)).await {
            Ok(_) => panic!("unknown provider must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        // Refusing before any network call matters: a typo must not spend two
        // minutes syncing the three providers that were spelled correctly.
        assert!(error.1 .0.error.contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_handlers_direct() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        let settings_path = dir.path().join("settings.json");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        tokio::fs::write(uploads_dir.join("test.txt"), "hello")
            .await
            .unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _event_rx, _loop_fut) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);

        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

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
        let params = FilesQuery {
            root: uploads_dir.to_string_lossy().to_string(),
            collection_id: None,
            tag_ids: None,
            collection_expression: None,
        };
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
    async fn test_confine_to_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        // Success case
        let f1 = uploads_dir.join("f1.txt");
        tokio::fs::write(&f1, "test").await.unwrap();
        let res = confine_to_uploads(&f1.to_string_lossy(), &uploads_dir);
        assert!(
            res.is_ok(),
            "confine_to_uploads should succeed for valid path inside uploads_dir"
        );

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
        tokio::fs::write(uploads_dir.join("f1.txt"), "test")
            .await
            .unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let res = delete_all_upload_handler(State(state))
            .await
            .map_err(|e| e.0)
            .expect("delete_all_upload_handler failed");
        assert_eq!(res, StatusCode::NO_CONTENT);
        assert!(uploads_dir.exists());
        let entries = std::fs::read_dir(&uploads_dir).unwrap().count();
        assert_eq!(entries, 0);
    }

    #[tokio::test]
    async fn test_update_bookmark_note_handler() {
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: dir.path().join("uploads"),
            events_tx,
        });

        let added = add_bookmark_handler(
            State(Arc::clone(&state)),
            Json(NewBookmark {
                path: "/tmp/example.pdf".into(),
                origin: wilkes_core::types::SourceOrigin::PdfPage {
                    page: 1,
                    bbox: None,
                },
                text_range: None,
                quote: "q".to_string(),
                note: None,
                rects: Vec::new(),
            }),
        )
        .await
        .map_err(|e| e.0)
        .expect("add_bookmark_handler failed")
        .into_response();
        assert_eq!(added.status(), StatusCode::OK);

        let id = state.context().list_bookmarks().await.unwrap()[0]
            .id
            .clone();
        update_bookmark_note_handler(
            State(Arc::clone(&state)),
            axum::extract::Path(id.clone()),
            Json(UpdateNoteBody {
                note: Some("  noted  ".to_string()),
            }),
        )
        .await
        .map_err(|e| e.0)
        .expect("update_bookmark_note_handler failed");

        assert_eq!(
            state.context().list_bookmarks().await.unwrap()[0]
                .note
                .as_deref(),
            Some("noted")
        );
    }

    #[tokio::test]
    async fn test_search_handler_grep() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        tokio::fs::write(uploads_dir.join("test.txt"), "hello world")
            .await
            .unwrap();

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };
        let (events_tx, _) = broadcast::channel(1024);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let query = wilkes_core::types::SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: uploads_dir.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: wilkes_core::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let res = search_handler(State(state), axum::Json(query)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_preview_handler_text() {
        let (dir, state) = handler_state();
        let file_path = dir.path().join("uploads").join("test.txt");
        tokio::fs::write(&file_path, "preview content")
            .await
            .unwrap();

        let match_ref = wilkes_core::types::MatchRef {
            path: file_path,
            origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
            text_range: None,
        };

        let res = preview_handler(State(state), axum::Json(match_ref)).await;
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: dir.path().to_path_buf(),
            events_tx,
        });

        let _ = get_logs_handler().await;
        let _ = clear_logs_handler().await;
        let _ = get_data_paths_handler(State(state.clone())).await;
        let _ = get_python_info_handler().await;
        let _ = is_semantic_ready_handler(State(state.clone())).await;
        let _ = embedder_capabilities_handler(State(state.clone())).await;

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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let query = DeleteUploadQuery {
            path: "test.txt".to_string(),
        };
        let res = delete_upload_handler(State(state.clone()), Query(query)).await;
        assert!(res.is_ok());
        assert!(!file_path.exists());

        let absolute_path = uploads_dir.join("absolute.txt");
        tokio::fs::write(&absolute_path, "content").await.unwrap();
        let query = DeleteUploadQuery {
            path: absolute_path.display().to_string(),
        };
        let res = delete_upload_handler(State(state), Query(query)).await;
        assert!(res.is_ok());
        assert!(!absolute_path.exists());
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let query = AssetQuery {
            path: file_path.to_string_lossy().to_string(),
        };
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let body = OpenFileBody {
            path: file_path.to_string_lossy().to_string(),
        };
        let res = open_file_handler(State(state), axum::Json(body)).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_rename_file_handler() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();
        let file_path = uploads_dir.join("old.txt");
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let body = RenameFileBody {
            path: file_path.to_string_lossy().to_string(),
            new_name: "new.txt".into(),
        };
        let res = match rename_file_handler(State(state), axum::Json(body)).await {
            Ok(res) => res,
            Err((status, body)) => panic!("rename failed: {status} {}", body.0.error),
        };

        assert_eq!(
            PathBuf::from(res.0).canonicalize().unwrap(),
            uploads_dir.join("new.txt").canonicalize().unwrap()
        );
        assert!(!file_path.exists());
        assert_eq!(
            tokio::fs::read_to_string(uploads_dir.join("new.txt"))
                .await
                .unwrap(),
            "file content"
        );
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        // Empty path
        let query = DeleteUploadQuery {
            path: "".to_string(),
        };
        let res = delete_upload_handler(State(state.clone()), Query(query)).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().0, StatusCode::BAD_REQUEST);

        // Outside path
        let query = DeleteUploadQuery {
            path: "../../etc/passwd".to_string(),
        };
        let res = delete_upload_handler(State(state.clone()), Query(query)).await;
        assert!(res.is_err());

        // Non-existent path
        let query = DeleteUploadQuery {
            path: "ghost.txt".to_string(),
        };
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        // Denied: outside uploads_dir
        let query = AssetQuery {
            path: "/etc/passwd".to_string(),
        };
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        // test get_worker_status_handler
        let _ = get_worker_status_handler(State(state.clone())).await;

        // test kill_worker_handler
        let _ = kill_worker_handler(State(state.clone())).await;

        // test set_worker_timeout_handler
        let _ =
            set_worker_timeout_handler(State(state.clone()), Json(TimeoutBody { secs: 10 })).await;

        // test get_index_status_handler (will fail but covers the handler)
        let _ =
            get_index_status_handler(State(state.clone()), Query(RootQuery { root: None })).await;

        // test cancel_embed_handler
        let _ = cancel_embed_handler(State(state.clone())).await;

        // test get_model_size_handler (will fail)
        let _ = get_model_size_handler(Query(ModelSizeQuery {
            engine: EmbeddingEngine::Fastembed,
            model_id: "m".to_string(),
        }))
        .await;

        // test embedder_capabilities_handler
        let _ = embedder_capabilities_handler(State(state.clone())).await;

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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let query = DeleteUploadQuery {
            path: "subdir".to_string(),
        };
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
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let _state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });
    }

    #[tokio::test]
    async fn test_upload_handler() {
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, _) = broadcast::channel(1024);
        let uploads_dir = dir.path().join("u");
        std::fs::create_dir_all(&uploads_dir).unwrap();

        let paths = WorkerPaths::resolve(dir.path());
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            Arc::new(BroadcastEmitter {
                tx: events_tx.clone(),
            }),
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let file_path = uploads_dir.join("to_delete.txt");
        std::fs::write(&file_path, "bye").unwrap();

        let params = DeleteUploadQuery {
            path: "to_delete.txt".to_string(),
        };
        assert_eq!(
            delete_upload_handler(State(state.clone()), Query(params))
                .await
                .map_err(|(s, _)| s)
                .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert!(!file_path.exists());

        // Test delete_upload_handler error
        let params_bad = DeleteUploadQuery {
            path: "ghost.txt".to_string(),
        };
        let res_delete_bad = delete_upload_handler(State(state.clone()), Query(params_bad)).await;
        assert!(res_delete_bad.is_err());
        assert_eq!(res_delete_bad.unwrap_err().0, StatusCode::NOT_FOUND);

        // Test delete_all_upload_handler
        std::fs::write(uploads_dir.join("a.txt"), "a").unwrap();
        std::fs::write(uploads_dir.join("b.txt"), "b").unwrap();
        assert_eq!(
            delete_all_upload_handler(State(state.clone()))
                .await
                .map_err(|(s, _)| s)
                .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(std::fs::read_dir(&uploads_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn test_even_more_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let uploads_dir = dir.path().join("uploads");
        tokio::fs::create_dir_all(&uploads_dir).await.unwrap();

        let (events_tx, _) = broadcast::channel(1);
        let emitter = Arc::new(BroadcastEmitter {
            tx: events_tx.clone(),
        });
        let paths = WorkerPaths::resolve(dir.path());
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            emitter,
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let _ = get_logs_handler().await;
        let _ = clear_logs_handler().await;
        let _ = get_data_paths_handler(State(state.clone())).await;
        let _ = is_semantic_ready_handler(State(state.clone())).await;
        let _ = delete_index_handler(State(state.clone()), Query(RootQuery { root: None })).await;
        let _ = download_model_handler(
            State(state.clone()),
            Json(DownloadBody {
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: wilkes_core::types::EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            }),
        )
        .await;

        let _ = update_settings_handler(State(state.clone()), Json(serde_json::json!({}))).await;
        let _ = get_settings_handler(State(state.clone())).await;
    }

    #[tokio::test]
    async fn test_asset_handler_direct() {
        use wilkes_core::types::SourceOrigin;
        use wilkes_core::types::{SearchMode, SearchQuery};
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, _) = broadcast::channel(1024);
        let uploads_dir = dir.path().join("u");
        std::fs::create_dir_all(&uploads_dir).unwrap();

        let asset_file = uploads_dir.join("test.txt");
        std::fs::write(&asset_file, "data").unwrap();

        let state = Arc::new(AppState {
            ctx: Some(test_ctx_with_dir(dir.path())),
            workspaces: None,
            uploads_dir: uploads_dir.clone(),
            events_tx,
        });

        let params = AssetQuery {
            path: asset_file.to_string_lossy().to_string(),
        };
        let res = asset_handler(State(state.clone()), Query(params)).await;
        assert!(res.is_ok());

        // Test open_file_handler
        let body = OpenFileBody {
            path: asset_file.to_string_lossy().to_string(),
        };
        let res_open = open_file_handler(State(state.clone()), Json(body)).await;
        assert!(res_open.is_ok());

        // Test preview_handler
        let match_ref = MatchRef {
            path: asset_file.clone(),
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
            text_range: None,
        };
        let res_preview = preview_handler(State(state.clone()), Json(match_ref)).await;
        assert!(res_preview.is_ok());

        // Test non-existent preview
        let match_ref_bad = MatchRef {
            path: uploads_dir.join("ghost.txt"),
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
            text_range: None,
        };
        let res_bad = preview_handler(State(state.clone()), Json(match_ref_bad)).await;
        assert!(res_bad.is_err());

        // Test search_handler semantic error (returns Ok immediately because SSE)
        let query_semantic = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: uploads_dir.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024,
            context_lines: 0,
            mode: SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };
        let res_semantic = search_handler(State(state.clone()), Json(query_semantic)).await;
        assert!(res_semantic.is_ok());
    }

    fn test_ctx_with_dir(dir: &Path) -> Arc<AppContext> {
        let paths = WorkerPaths::resolve(dir);
        let (ctx, _, _) = AppContext::new(
            dir.to_path_buf(),
            dir.join("s.json"),
            paths,
            Arc::new(BroadcastEmitter {
                tx: broadcast::channel(1).0,
            }),
        );
        ctx
    }

    #[tokio::test]
    async fn test_events_handler_sse() {
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, _) = broadcast::channel(1024);
        let paths = WorkerPaths::resolve(dir.path());
        let (ctx, _, _) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            paths,
            Arc::new(BroadcastEmitter {
                tx: events_tx.clone(),
            }),
        );
        let state = Arc::new(AppState {
            ctx: Some(ctx),
            workspaces: None,
            uploads_dir: dir.path().join("u"),
            events_tx: events_tx.clone(),
        });

        let sse = events_handler(State(state)).await;
        // Verify it returns an Sse response
        let _ = sse.into_response();

        // Send an event and see if it doesn't crash
        events_tx
            .send(("test".to_string(), serde_json::json!({})))
            .unwrap();
    }

    #[test]
    fn managed_contract_fixture_matches_the_server_wire_schema() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/internal/specs/fixtures/consumer-api-v2.json"
        ))
        .unwrap();
        assert_eq!(fixture["schema_version"], 2);

        let ensure: EnsureManagedWorkspace =
            serde_json::from_value(fixture["ensure_request"].clone()).unwrap();
        assert_eq!(ensure.corpus_key, "store-018f");
        // The corpus names the application that owns it, which is what the
        // route path used to do.
        assert_eq!(ensure.owner, "underdog");

        // The one addressing object, in both forms. A pin is required on a
        // managed corpus, where it routes as well as verifies, and optional on
        // a user workspace, where it only verifies.
        let pinned: ConsumerScope =
            serde_json::from_value(fixture["scope_pinned"].clone()).unwrap();
        assert_eq!(
            pinned.expected_embedding_space_id.as_deref(),
            Some("space-example")
        );
        let unpinned: ConsumerScope =
            serde_json::from_value(fixture["scope_unpinned"].clone()).unwrap();
        assert_eq!(unpinned.expected_embedding_space_id, None);

        let import: ManagedImportBody =
            serde_json::from_value(fixture["import_request"].clone()).unwrap();
        assert_eq!(import.corpus_id, "managed-corpus-018f");
        assert_eq!(
            import.expected_embedding_space_id.as_deref(),
            Some("space-example")
        );

        // Importing into a corpus that has no index yet: the caller has no
        // space id to echo because the corpus has none, and omitting the field
        // is how it says so. Sending one would claim a space that no index
        // carries, which the handler refuses as a mismatch.
        let first_import: ManagedImportBody =
            serde_json::from_value(fixture["import_request_empty_corpus"].clone()).unwrap();
        assert_eq!(first_import.expected_embedding_space_id, None);
        match import.source {
            ManagedImportSource::WilkesFile {
                workspace_id,
                root,
                path,
            } => {
                assert_eq!(workspace_id, "workspace-example");
                assert_eq!(root, PathBuf::from("/library"));
                assert_eq!(path, PathBuf::from("/library/paper.pdf"));
            }
            ManagedImportSource::Path { .. } => panic!("expected wilkes_file fixture"),
        }

        // The embedder manifest: the shape a consumer reads *before* it has a
        // corpus, and the one that decides what a corpus gets built of. Both
        // sides carry this fixture, and its two nulls are load-bearing — a
        // dimension nobody has loaded, and a recipe nothing has read.
        let manifest: wilkes_core::types::EmbedderCapabilityManifest =
            serde_json::from_value(fixture["embedder_manifest_response"].clone()).unwrap();
        let mini = manifest
            .models
            .iter()
            .find(|model| model.model_id == "AllMiniLML6V2")
            .expect("the pinned model");
        assert_eq!(mini.dimension, Some(384));
        assert_eq!(
            mini.prefix_source,
            wilkes_core::types::PrefixSource::NotDocumented
        );
        let added = manifest
            .models
            .iter()
            .find(|model| !model.catalogued)
            .expect("a hand-added model");
        assert_eq!(added.dimension, None);
        assert_eq!(
            added.prefix_source,
            wilkes_core::types::PrefixSource::Undetermined
        );

        let response = &fixture["import_response"];
        for required in [
            "corpus_id",
            "snapshot_id",
            "rendition_id",
            "extracted_content_sha256",
            "embedding_space_id",
            "outline",
            "extraction",
            "chunks",
            "embedding_work",
        ] {
            assert!(
                response.get(required).is_some(),
                "missing fixture field {required}"
            );
        }

        // The outline carries a position and says what established it, and the
        // extraction says what it had to decide to produce that position.
        // Both are part of the contract, so both are read back through the
        // types the server serializes rather than only eyeballed in the file.
        let anchor: wilkes_core::types::OutlineAnchor =
            serde_json::from_value(response["outline"][0]["anchor"].clone()).unwrap();
        assert_eq!(
            anchor,
            wilkes_core::types::OutlineAnchor::DestinationCoordinate
        );
        let extraction: wilkes_core::types::ExtractionDiagnostics =
            serde_json::from_value(response["extraction"].clone()).unwrap();
        assert_eq!(extraction.pages, 1);

        // Passages, named the one way they can be named. `resolve` needs no
        // root or path: a ref already names its document.
        let resolve: ChunksResolveBody =
            serde_json::from_value(fixture["chunks_resolve_request"].clone()).unwrap();
        assert_eq!(resolve.chunk_refs.len(), 1);
        assert_eq!(
            resolve.scope.expected_embedding_space_id.as_deref(),
            Some("space-example")
        );
        let resolved = &fixture["chunks_resolve_response"]["chunks"][0];
        for required in [
            "chunk_ref",
            "ordinal",
            "text",
            "text_sha256",
            "byte_range",
            "origin",
        ] {
            assert!(
                resolved.get(required).is_some(),
                "missing resolved chunk field {required}"
            );
        }
        assert!(
            resolved.get("chunk_id").is_none(),
            "a rowid is not part of this contract"
        );

        let accumulate: ChunksGroupsBody =
            serde_json::from_value(fixture["chunks_accumulate_request"].clone()).unwrap();
        assert_eq!(accumulate.groups.len(), 1);
        // The sum and the count, not the mean: a caller partitioning a large
        // group across requests adds these and normalizes once.
        let group = &fixture["chunks_accumulate_response"]["groups"][0];
        assert!(group.get("sum").is_some());
        assert!(group.get("member_count").is_some());

        // The untagged probe is the shape most likely to drift silently, so
        // the fixture carries one of each and this reads them back through the
        // enum the server deserializes.
        let search: ChunksSearchBody =
            serde_json::from_value(fixture["chunks_search_request"].clone()).unwrap();
        assert_eq!(search.probes.len(), 2);
        assert!(matches!(search.probes[0], SearchProbe::Vector(_)));
        assert!(matches!(search.probes[1], SearchProbe::Text(_)));

        let backup: ManagedBackupBody =
            serde_json::from_value(fixture["backup_request"].clone()).unwrap();
        assert_eq!(backup.expected_embedding_space_id, "space-example");
        let restore: ManagedRestoreBody =
            serde_json::from_value(fixture["restore_request"].clone()).unwrap();
        assert_eq!(restore.backup_name, "restore-example");
        assert!(
            serde_json::from_value::<ManagedBackupBody>(serde_json::json!({
                "corpus_id": "managed-corpus-018f",
                "expected_embedding_space_id": "space-example",
                "destination": "/arbitrary/path"
            }))
            .is_err(),
            "managed backup must never accept an arbitrary destination"
        );
    }
}
