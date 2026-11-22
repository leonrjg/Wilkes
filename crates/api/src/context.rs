use parking_lot::Mutex as PLMutex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use wilkes_core::completion::{
    run_completion, CompletionDependencies, CompletionEvent, CompletionFeedback, CompletionRequest,
    CompletionSession, SessionSteering,
};
use wilkes_core::directory_watcher::{DirectoryChangeBatch, DirectoryWatcher};
use wilkes_core::embed::cluster::WardTree;
use wilkes_core::embed::engines::remote::{
    EmbedderAddress, InstallEvent, RemoteHost, RemoteInstaller, RemoteSeal,
};
use wilkes_core::embed::index::db::{
    BuildScope, ChunkAccumulation, ManagedChunkData, ManagedChunkNeighbors, ManagedDocumentData,
    TopicChunkData, TopicCoveragePrototype,
};
use wilkes_core::embed::index::semantic_updater::process_directory_change;
use wilkes_core::embed::index::{
    BuildReporter, DocumentOutcome, IndexActivity, IndexJobJournal, JobState, SemanticIndex,
};
use wilkes_core::embed::installer::EmbedderInstaller;
use wilkes_core::embed::{dispatch, ChunkRef, Embedder, ExtractionRecipe};
use wilkes_core::generate::engines::dispatch as generate_dispatch;
use wilkes_core::generate::tasks::cluster_label::{
    cluster_label, cluster_label_stream, validate_cluster_label,
};
use wilkes_core::generate::tasks::document_summary::{
    summarize_document as generate_document_summary, DocumentSummaryInput,
};
use wilkes_core::generate::tasks::relation::{
    explain_relation, DocumentSummary, MAX_EXCERPT_CHARS,
};
use wilkes_core::generate::tasks::search_results_summary::{
    summarize_search_results as generate_search_results_summary, SearchResultsSummaryInput,
};
use wilkes_core::generate::{Generated, GenerationEngine, Generator};
use wilkes_core::integrations::zotero::model::ZoteroItem;
use wilkes_core::integrations::zotero::ZoteroClient;
use wilkes_core::integrations::{IntegrationRegistry, LiteratureSource};
use wilkes_core::metadata::cache::{FileIdentity, MetadataCache, MetadataSource};
use wilkes_core::metadata::doi::normalize_doi;
use wilkes_core::models::progress::{EmbedProgress, ReindexProgress};
use wilkes_core::types::{
    Bookmark, BookmarkCluster, BookmarkClustersQuery, BookmarkClustersResult, ChunkTopic,
    ChunkTopicMember, ChunkTopicsQuery, ChunkTopicsResult, CitationCoverage, CitationGraph,
    CitationGraphDocument, CitationGraphEdge, CitationGraphQuery, CitationLinks,
    CitationLinksQuery, CollectionValidation, DocumentMetadata, DocumentTagUpdate, EmbeddingEngine,
    ExternalCitedWork, FileEntry, FileListResponse, FileType, IndexStatus, IndexingConfig,
    MetadataConflictValue, MetadataSourcePreference, NewBookmark, NewSmartCollection, NewTag,
    OmittedFileReason, PreviewData, ReadingOrder, ReadingOrderDocument, ReadingOrderLink,
    ReadingOrderQuery, ReadingOrderReason, ReadingOrderSubject, ReadingPlacement, RelatedDocument,
    RelatedDocumentsQuery, RemoteEmbeddingSettings, RootCoverage, SearchDocument, SearchLogEntry,
    SearchMode, SearchQuery, SearchScope, SelectedEmbedder, SemanticSettings, Settings,
    SmartCollection, Tag, TidyDestination, TidyPlacement, TidyPlan, TidySuggestion,
    TopicLibraryCoverage, UpdateSmartCollection, UpdateTag,
};
use wilkes_core::types::{
    GenerationSettings, GenerationStreamEvent, GenerationTask, GeneratorDescriptor,
    LiteratureSearchResult, NewSavedWork, SavedWork,
};
use wilkes_core::worker::ipc::WorkerKind;
use wilkes_core::worker::manager::{
    ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus,
};
use wilkes_core::worker::{python_env_inventory, remove_python_envs, PythonEnvInventory};

use crate::commands::search::{start_search, SearchHandle};
use wilkes_core::consumer::{ConsumerError, ConsumerErrorCode};
use wilkes_core::search::grep::StoredText;
use wilkes_core::text_index::upkeep::{
    apply_changes, reconcile_library, ChangedDocuments, LibraryDocuments, UpkeepReport,
};
use wilkes_core::text_index::DocumentTextIndex;
use wilkes_core::{consumer_bail, consumer_ensure};

use crate::commands::settings::{get_scoped_settings, update_scoped_settings};
#[cfg(test)]
use crate::commands::settings::{get_settings, update_settings};
use crate::research::{CachedBookmarkEmbedding, ResearchStore, SearchLogTracker};

pub mod feeds;
pub mod web_article;

mod bookmarks;
mod chunks;
mod citations;
mod embedding;
mod exports;
mod files;
mod generation;
mod image_analysis;
mod indexing;
mod literature;
mod managed;
mod metadata;
mod research;
mod restore;
mod search;
mod settings;
mod text_index;
mod topics;
mod workers;

pub use self::chunks::*;
use self::citations::*;
pub use self::embedding::*;
pub use self::exports::*;
use self::files::*;
pub use self::generation::*;
pub use self::indexing::*;
pub(crate) use self::managed::*;
use self::metadata::*;
pub use self::search::*;
use self::text_index::*;
use self::topics::*;

const BOOKMARK_EMBEDDING_RECIPE_VERSION: i64 = 1;

/// Bump whenever the cluster-label prompt or grammar changes: that is the whole
/// point of the field, and a stale cache would otherwise serve labels produced
/// by a recipe that no longer exists.
const BOOKMARK_CLUSTER_LABEL_RECIPE_VERSION: i64 = 6;
const CHUNK_CLUSTER_LABEL_RECIPE_VERSION: i64 = 4;

/// A run producing more clusters than this is not worth labelling: the worker
/// serialises requests, so 20 labels already means several seconds of queue.
const MAX_LABELLED_CLUSTERS: usize = 20;

// ── EventEmitter ──────────────────────────────────────────────────────────────

/// Platform-agnostic event sink. The desktop implements this with Tauri's
/// `app.emit()`; the server implements it with a broadcast channel.
pub trait EventEmitter: Send + Sync + 'static {
    fn emit(&self, name: &str, payload: serde_json::Value);
}

/// Who one workspace's event is for.
pub struct EventAudience<'a> {
    /// The views — desktop windows, or the web UI — showing the workspace.
    pub views: &'a [String],
    /// Whether it is the installation's default workspace: the one a request
    /// naming none is answered from, whose events the shared HTTP event
    /// stream carries.
    pub default: bool,
}

/// A shell's event sink, as a [`crate::workspace::WorkspaceManager`] uses it.
///
/// Two entry points because there are two kinds of event. `emit` is the
/// installation's own — a workspace list that changed, a setting every
/// window shows — and reaches everything. `emit_to` is one workspace's, from
/// the context serving it, and reaches only who is looking at that workspace:
/// with a window per workspace, a build in one is not progress in the other.
pub trait ViewEventSink: EventEmitter {
    fn emit_to(&self, audience: EventAudience<'_>, name: &str, payload: serde_json::Value);
}

// ── AppContext ────────────────────────────────────────────────────────────────

pub use wilkes_core::embed::EmbeddedTexts;

/// Shared application state and lifecycle logic. Both the desktop (Tauri) and
/// the server (axum) create exactly one `Arc<AppContext>` and delegate all
/// business operations to it.
/// What a managed workspace needs resident to do its job.
///
/// Two roles, and they need different things. The canonical corpus owns
/// passages; a projection owns coordinates. Only one of them has a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedRuntime {
    /// A projection: it computes vectors, so its model is loaded.
    WithEmbedder,
    /// The canonical corpus: an index for its passages, and no model at all.
    IndexOnly,
}

impl AppContext {
    pub fn new(
        data_dir: PathBuf,
        settings_path: PathBuf,
        paths: WorkerPaths,
        events: Arc<dyn EventEmitter>,
    ) -> (
        Arc<Self>,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        Self::new_scoped(
            data_dir.clone(),
            data_dir,
            settings_path.clone(),
            settings_path,
            paths,
            events,
        )
    }

    pub fn new_scoped(
        data_dir: PathBuf,
        shared_data_dir: PathBuf,
        settings_path: PathBuf,
        workspace_path: PathBuf,
        paths: WorkerPaths,
        events: Arc<dyn EventEmitter>,
    ) -> (
        Arc<Self>,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (worker_manager, event_rx, loop_fut) =
            WorkerManager::new(paths.clone(), WorkerKind::Embed);
        let (generate_manager, generate_event_rx, generate_loop_fut) =
            WorkerManager::new(paths.clone(), WorkerKind::Generate);
        // A third, because a manager holds one worker and a worker serves one
        // role: a build alternates recognition and embedding file by file, and
        // sharing a manager between them would restart and reload a model on
        // every alternation.
        wilkes_core::worker::sweep_python_envs(&paths);
        let (recognize_manager, recognize_event_rx, recognize_loop_fut) =
            WorkerManager::new(paths, WorkerKind::Recognize);
        let bookmarks_path = data_dir.join("bookmarks.json");
        let model_dir = shared_data_dir.join("models");
        let catalogue_dir = shared_data_dir.join("catalogue");
        let cover_cache_dir = shared_data_dir.join("covers");
        let ctx = Arc::new(Self {
            data_dir,
            shared_data_dir,
            model_dir,
            catalogue_dir,
            cover_cache_dir,
            bookmarks_path,
            settings_path,
            workspace_path,
            embedder: PLMutex::new(None),
            index: PLMutex::new(Arc::new(Mutex::new(None))),
            metadata_cache: PLMutex::new(None),
            job_journal: PLMutex::new(None),
            research_store: PLMutex::new(None),
            text_index: PLMutex::new(None),
            text_index_upkeep: TextIndexUpkeep::default(),
            reading_order: ReadingOrderState::default(),
            feed_upkeep: feeds::FeedUpkeep::default(),
            directory_watcher: PLMutex::new(None),
            embed_task: PLMutex::new(None),
            embed_cancel_in_progress: AtomicBool::new(false),
            winddown: Arc::new(tokio::sync::Notify::new()),
            shutting_down: AtomicBool::new(false),
            worker_manager,
            generate_manager,
            recognize_manager,
            generator: PLMutex::new(None),
            generator_load_lock: tokio::sync::Mutex::new(()),
            image_analyzer_load_lock: tokio::sync::Mutex::new(()),
            generator_epoch: AtomicU64::new(0),
            cluster_label_task: PLMutex::new(None),
            topic_operations: PLMutex::new(HashMap::new()),
            topic_tree_caches: PLMutex::new(TopicTreeCaches::default()),
            topic_tree_build_lock: tokio::sync::Mutex::new(()),
            semantic_index_revision: AtomicU64::new(0),
            completion_session: PLMutex::new(CompletionSession::default()),
            completion_operations: PLMutex::new(HashMap::new()),
            compose_operations: PLMutex::new(HashMap::new()),
            events,
            settings_lock: tokio::sync::Mutex::new(()),
            managed_runtime_lock: tokio::sync::Mutex::new(()),
            remote_seal: RemoteSeal::new(),
            model_embedder: tokio::sync::Mutex::new(None),
            managed_import_lock: tokio::sync::Mutex::new(()),
            managed_pending_imports: AtomicU64::new(0),
            bookmarks_lock: tokio::sync::Mutex::new(()),
        });

        // Both managers are driven by the single future `new` already returns,
        // and both event streams are merged into the single receiver it already
        // returns. `new` stays free of side effects — it is called outside a
        // runtime in tests — and callers do not have to learn that there are
        // now two workers to drive.
        let (merged_tx, merged_rx) = mpsc::channel(64);
        let combined = async move {
            tokio::join!(
                loop_fut,
                generate_loop_fut,
                recognize_loop_fut,
                forward_manager_events(event_rx, merged_tx.clone()),
                forward_manager_events(generate_event_rx, merged_tx.clone()),
                forward_manager_events(recognize_event_rx, merged_tx),
            );
        };
        (ctx, merged_rx, combined)
    }

    /// Spawns the required background tasks for the application context.
    pub fn spawn_background_tasks(
        self: Arc<Self>,
        event_rx: mpsc::Receiver<ManagerEvent>,
        loop_fut: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        tokio::spawn(loop_fut);

        // The forwarder gets the emitter, not the context. It runs for as long
        // as the manager loops do, and they run for as long as the context's
        // managers exist — so a forwarder holding the context would keep a
        // retired workspace, and every database it opened, alive for good.
        tokio::spawn(Self::run_event_forwarder(
            Arc::clone(&self.events),
            event_rx,
        ));

        let ctx2 = Arc::clone(&self);
        tokio::spawn(async move {
            ctx2.restore_state().await;
        });
    }

    /// Forward manager-loop events through the EventEmitter. Run this as a
    /// background task after `new`.
    pub async fn run_event_forwarder(
        events: Arc<dyn EventEmitter>,
        mut rx: mpsc::Receiver<ManagerEvent>,
    ) {
        while let Some(event) = rx.recv().await {
            let name = match event {
                ManagerEvent::WorkerStarting => "WorkerStarting",
                ManagerEvent::ReindexingDone => "ReindexingDone",
            };
            events.emit("manager-event", serde_json::json!(name));
        }
    }

    fn emit_embed_error(&self, operation: &str, message: impl Into<String>) {
        let message = message.into();
        if message.is_empty() {
            info!("{operation} cancelled");
        } else {
            error!("{operation} failed: {message}");
        }
        self.events.emit(
            "embed-error",
            serde_json::json!({
                "operation": operation,
                "message": message,
            }),
        );
    }

    // ── Business Logic ────────────────────────────────────────────────────────

    pub async fn get_settings(&self) -> Settings {
        get_scoped_settings(&self.settings_path, &self.workspace_path)
            .await
            .unwrap_or_default()
    }

    async fn settings(&self) -> Settings {
        self.get_settings().await
    }
}

async fn forward_manager_events(
    mut rx: mpsc::Receiver<ManagerEvent>,
    tx: mpsc::Sender<ManagerEvent>,
) {
    while let Some(event) = rx.recv().await {
        if tx.send(event).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests;
