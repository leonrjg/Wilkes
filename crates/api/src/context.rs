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
use wilkes_core::embed::index::db::{TopicChunkData, TopicCoveragePrototype};
use wilkes_core::embed::index::semantic_updater::process_directory_change;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedderInstaller;
use wilkes_core::embed::{dispatch, Embedder};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::{ContentExtractor, ExtractorRegistry};
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
use wilkes_core::integrations::openalex::OpenAlexClient;
use wilkes_core::integrations::semantic_scholar::SemanticScholarClient;
use wilkes_core::integrations::zotero::model::ZoteroItem;
use wilkes_core::integrations::zotero::ZoteroClient;
use wilkes_core::metadata::cache::{FileIdentity, MetadataCache, MetadataSource};
use wilkes_core::metadata::doi::find_dois;
use wilkes_core::models::progress::EmbedProgress;
use wilkes_core::types::{
    Bookmark, BookmarkCluster, BookmarkClustersQuery, BookmarkClustersResult, ChunkTopic,
    ChunkTopicMember, ChunkTopicsQuery, ChunkTopicsResult, CitationLinks, CitationLinksQuery,
    CitationReference, CollectionValidation, DocumentMetadata, DocumentTagUpdate, EmbedderModel,
    FileEntry, FileListResponse, FileType, IndexStatus, IndexingConfig, MetadataConflictValue,
    MetadataSourcePreference, NewBookmark, NewSmartCollection, NewTag, PreviewData,
    RelatedDocument, RelatedDocumentsQuery, SearchLogEntry, SearchMode, SearchQuery, SearchScope,
    SelectedEmbedder, SemanticSettings, Settings, SmartCollection, Tag, TopicLibraryCoverage,
    UpdateSmartCollection, UpdateTag,
};
use wilkes_core::types::{
    GenerationSettings, GenerationStreamEvent, GenerationTask, GeneratorDescriptor,
};
use wilkes_core::worker::manager::{
    ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus,
};

use crate::commands::search::{start_search, SearchHandle};
use crate::commands::settings::{get_settings, update_settings};
use crate::research::{CachedBookmarkEmbedding, ResearchStore, SearchLogTracker};

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

#[async_trait::async_trait]
impl wilkes_agent::search::SearchService for AppContext {
    async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
        self.get_settings().await.last_directory
    }

    async fn library_roots(self: Arc<Self>) -> Vec<PathBuf> {
        let settings = self.get_settings().await;
        library_roots(&settings).0
    }

    async fn list_smart_collections(self: Arc<Self>) -> Result<Vec<SmartCollection>, String> {
        self.list_collections().map_err(|e| e.to_string())
    }

    async fn integrations(self: Arc<Self>) -> wilkes_core::types::IntegrationsSettings {
        self.get_settings().await.integrations
    }

    async fn search(
        self: Arc<Self>,
        query: SearchQuery,
        max_files: usize,
    ) -> Result<wilkes_agent::search::CollectedSearch, String> {
        let handle = self.start_search_as(query, "agent").await?;
        let mut files = Vec::new();
        let mut truncated = false;
        let stats = handle
            .run(|file_matches| {
                if files.len() < max_files {
                    files.push(file_matches);
                } else {
                    truncated = true;
                }
                async { true }
            })
            .await;

        Ok(wilkes_agent::search::CollectedSearch {
            files,
            stats,
            truncated,
        })
    }

    async fn related_documents(
        self: Arc<Self>,
        query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String> {
        AppContext::related_documents(self, query).await
    }

    async fn list_documents(self: Arc<Self>, root: PathBuf) -> Result<FileListResponse, String> {
        self.list_files(root).await.map_err(|e| e.to_string())
    }

    async fn document_metadata(self: Arc<Self>, path: PathBuf) -> Result<DocumentMetadata, String> {
        self.document_metadata_full(path)
            .await
            .map_err(|e| e.to_string())
    }
}

// ── Embed task handle ─────────────────────────────────────────────────────────

pub struct EmbedTaskHandle {
    pub operation: EmbedOperation,
    pub cancel: CancellationToken,
    pub cancel_flag: Arc<AtomicBool>,
    pub join: JoinHandle<anyhow::Result<()>>,
}

#[derive(Copy, Clone)]
pub enum EmbedOperation {
    Download,
    Build,
}

impl EmbedOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Build => "Build",
        }
    }
}

#[derive(Clone, Debug)]
struct BuildIndexPlan {
    root_path: PathBuf,
    device: String,
    chunk_size: usize,
    chunk_overlap: usize,
    supported_extensions: Vec<String>,
}

#[derive(Clone, Debug)]
struct DownloadModelPlan {
    device: String,
}

#[derive(Clone, Debug)]
struct RestoreStatePlan {
    db_status: IndexStatus,
    selected: SelectedEmbedder,
    device: String,
}

struct RestoreLoadedState {
    plan: RestoreStatePlan,
    embedder: Arc<dyn Embedder>,
    index: SemanticIndex,
}

enum RestoreStatePreparation {
    Ready(RestoreStatePlan),
    ResetStaleSelection {
        db_status: IndexStatus,
        selected: SelectedEmbedder,
    },
}

struct SemanticRuntime {
    embedder: Arc<dyn Embedder>,
    index: Arc<Mutex<Option<SemanticIndex>>>,
    indexing: IndexingConfig,
}

fn library_roots(settings: &Settings) -> (Vec<PathBuf>, Vec<String>) {
    let mut roots = Vec::new();
    if let Some(root) = settings.last_directory.clone() {
        roots.push(root);
    }
    for root in settings
        .favorites
        .iter()
        .chain(settings.recent_dirs.iter())
        .cloned()
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let mut errors = Vec::new();
    let mut canonical: Vec<PathBuf> = Vec::new();
    for root in roots {
        match std::fs::canonicalize(&root) {
            Ok(root) if root.is_dir() => {
                if !canonical.contains(&root) {
                    canonical.push(root);
                }
            }
            Ok(_) => errors.push(format!(
                "Library path is not a directory: {}",
                root.display()
            )),
            Err(err) => errors.push(format!(
                "Library directory is unavailable: {} ({err})",
                root.display()
            )),
        }
    }
    canonical.sort_by_key(|root| root.components().count());
    let mut covered = Vec::<PathBuf>::new();
    for root in canonical {
        if !covered.iter().any(|parent| root.starts_with(parent)) {
            covered.push(root);
        }
    }
    (covered, errors)
}

fn metadata_source_preference(source: &MetadataSourcePreference) -> MetadataSource {
    match source {
        MetadataSourcePreference::File => MetadataSource::File,
        MetadataSourcePreference::Zotero => MetadataSource::Zotero,
        MetadataSourcePreference::SemanticScholar => MetadataSource::SemanticScholar,
        MetadataSourcePreference::OpenAlex => MetadataSource::OpenAlex,
    }
}

/// Outcome of `load_generator`. The three cases are distinct at every call
/// site: "no model attached" for want of configuration is the normal resting
/// state, while "superseded" means a newer load is already doing the work and
/// this one deliberately left the generator alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratorLoad {
    Attached,
    NotConfigured,
    Superseded,
}

impl GeneratorLoad {
    pub fn attached(self) -> bool {
        self == GeneratorLoad::Attached
    }
}

// ── AppContext ────────────────────────────────────────────────────────────────

struct TopicTreeCache {
    root: PathBuf,
    path: Option<PathBuf>,
    requested_input_cap: usize,
    index_revision: u64,
    tree: WardTree,
    sampled: Vec<TopicChunkData>,
    /// Retained only for a document-scoped tree. Root trees deliberately drop
    /// their input vectors after Ward construction because their O(n^2)
    /// similarity matrix is already the dominant cache allocation.
    sampled_embeddings: Option<Vec<Vec<f32>>>,
    total_chunk_count: usize,
    total_document_count: usize,
    sampled_document_count: usize,
    input_cap: usize,
}

impl TopicTreeCache {
    fn matches(
        &self,
        root: &Path,
        path: Option<&Path>,
        requested_input_cap: usize,
        index_revision: u64,
    ) -> bool {
        self.root == root
            && self.path.as_deref() == path
            && self.requested_input_cap == requested_input_cap
            && self.index_revision == index_revision
    }
}

#[derive(Default)]
struct TopicTreeCaches {
    root: Option<Arc<TopicTreeCache>>,
    document: Option<Arc<TopicTreeCache>>,
}

impl TopicTreeCaches {
    fn slot(&self, path: Option<&Path>) -> &Option<Arc<TopicTreeCache>> {
        if path.is_some() {
            &self.document
        } else {
            &self.root
        }
    }

    fn set(&mut self, path: Option<&Path>, cache: Arc<TopicTreeCache>) {
        if path.is_some() {
            self.document = Some(cache);
        } else {
            self.root = Some(cache);
        }
    }
}

struct TopicOperation {
    cancel: CancellationToken,
    cancel_flag: Arc<AtomicBool>,
    label_task: Option<JoinHandle<()>>,
}

struct CompletionOperation {
    cancel: Arc<AtomicBool>,
}

/// Shared application state and lifecycle logic. Both the desktop (Tauri) and
/// the server (axum) create exactly one `Arc<AppContext>` and delegate all
/// business operations to it.
pub struct AppContext {
    pub data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub bookmarks_path: PathBuf,
    embedder: PLMutex<Option<Arc<dyn Embedder>>>,
    index: PLMutex<Arc<Mutex<Option<SemanticIndex>>>>,
    /// Persistent cache of extracted document metadata, opened lazily. Shared
    /// with the index watcher so renames re-key rather than re-extract.
    metadata_cache: PLMutex<Option<Arc<Mutex<MetadataCache>>>>,
    research_store: PLMutex<Option<Arc<Mutex<ResearchStore>>>>,
    directory_watcher: PLMutex<Option<DirectoryWatcher>>,
    embed_task: PLMutex<Option<EmbedTaskHandle>>,
    embed_cancel_in_progress: AtomicBool,
    shutting_down: AtomicBool,
    pub worker_manager: WorkerManager,
    /// Generation runs in its own process with its own manager. Sharing the
    /// embedding manager would evict a multi-gigabyte model on every
    /// alternation, because the worker caches exactly one model and restarts on
    /// role change.
    pub generate_manager: WorkerManager,
    generator: PLMutex<Option<Arc<dyn Generator>>>,
    /// Serialises `load_generator`, so two settings changes cannot download and
    /// attach concurrently.
    generator_load_lock: tokio::sync::Mutex<()>,
    /// Claimed by each load before it queues behind the lock. Only the newest
    /// claim may assign the generator; an older one that finishes later would
    /// otherwise attach a model the user has already switched away from.
    generator_epoch: AtomicU64,
    /// At most one labelling run in flight: a newer `cluster_bookmarks` call
    /// makes the previous run's results describe a partition nobody is looking
    /// at any more.
    cluster_label_task: PLMutex<Option<JoinHandle<()>>>,
    /// Root and document clouds can be visible together. Each request owns one
    /// cancellation lifecycle; closing or redrawing one surface must not stop
    /// the other.
    topic_operations: PLMutex<HashMap<String, TopicOperation>>,
    /// Exactly one retained root tree and one current-document tree. This
    /// prevents viewer navigation from evicting the library tree without
    /// turning the O(n²) cache into unbounded history.
    topic_tree_caches: PLMutex<TopicTreeCaches>,
    /// Serialises cache misses so rapid redraw requests cannot build duplicate
    /// O(n²) trees concurrently.
    topic_tree_build_lock: tokio::sync::Mutex<()>,
    /// Incremented before every semantic-index mutation. A build captures this
    /// value and may install its tree only if the value is still current.
    semantic_index_revision: AtomicU64,
    /// Completion caches, Rocchio feedback, and inspector history are scoped to
    /// this application session and share the single core completion pipeline.
    completion_session: PLMutex<CompletionSession>,
    completion_operations: PLMutex<HashMap<String, CompletionOperation>>,
    events: Arc<dyn EventEmitter>,
    settings_lock: tokio::sync::Mutex<()>,
    bookmarks_lock: tokio::sync::Mutex<()>,
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
        let (worker_manager, event_rx, loop_fut) = WorkerManager::new(paths.clone());
        let (generate_manager, generate_event_rx, generate_loop_fut) = WorkerManager::new(paths);
        let ctx = Arc::new(Self {
            data_dir,
            bookmarks_path: settings_path.with_file_name("bookmarks.json"),
            settings_path,
            embedder: PLMutex::new(None),
            index: PLMutex::new(Arc::new(Mutex::new(None))),
            metadata_cache: PLMutex::new(None),
            research_store: PLMutex::new(None),
            directory_watcher: PLMutex::new(None),
            embed_task: PLMutex::new(None),
            embed_cancel_in_progress: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            worker_manager,
            generate_manager,
            generator: PLMutex::new(None),
            generator_load_lock: tokio::sync::Mutex::new(()),
            generator_epoch: AtomicU64::new(0),
            cluster_label_task: PLMutex::new(None),
            topic_operations: PLMutex::new(HashMap::new()),
            topic_tree_caches: PLMutex::new(TopicTreeCaches::default()),
            topic_tree_build_lock: tokio::sync::Mutex::new(()),
            semantic_index_revision: AtomicU64::new(0),
            completion_session: PLMutex::new(CompletionSession::default()),
            completion_operations: PLMutex::new(HashMap::new()),
            events,
            settings_lock: tokio::sync::Mutex::new(()),
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
                forward_manager_events(event_rx, merged_tx.clone()),
                forward_manager_events(generate_event_rx, merged_tx),
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

        let ctx1 = Arc::clone(&self);
        tokio::spawn(async move {
            ctx1.run_event_forwarder(event_rx).await;
        });

        let ctx2 = Arc::clone(&self);
        tokio::spawn(async move {
            ctx2.restore_state().await;
        });
    }

    /// Forward manager-loop events through the EventEmitter. Run this as a
    /// background task after `new`.
    pub async fn run_event_forwarder(self: Arc<Self>, mut rx: mpsc::Receiver<ManagerEvent>) {
        while let Some(event) = rx.recv().await {
            let name = match event {
                ManagerEvent::WorkerStarting => "WorkerStarting",
                ManagerEvent::ReindexingDone => "ReindexingDone",
            };
            self.events.emit("manager-event", serde_json::json!(name));
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
        get_settings(&self.settings_path).await.unwrap_or_default()
    }

    pub async fn list_bookmarks(&self) -> anyhow::Result<Vec<Bookmark>> {
        let store = self.research_store()?;
        let mut bookmarks = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_bookmarks()?;
        let settings = self.get_settings().await;
        self.resolve_bookmark_paths(&mut bookmarks, &settings);
        Ok(bookmarks)
    }

    /// Re-point bookmarks whose stored `path` no longer exists to wherever their
    /// content identity now lives, using the metadata cache as the identity
    /// registry (the same mechanism that re-keys the index and metadata rows on
    /// rename). Resolution is read-only and recomputed each call: nothing is
    /// persisted, so there is no write on this read path.
    ///
    /// Best-effort by design — a bookmark keeps its stored path when it still
    /// exists, has no captured identity, the cache is unavailable, or the
    /// identity is absent/ambiguous in the cache (e.g. the renamed file has not
    /// been re-listed yet). Such a bookmark simply resolves on a later read once
    /// a listing has populated the cache; it never fails to load.
    fn resolve_bookmark_paths(&self, bookmarks: &mut [Bookmark], settings: &Settings) {
        // Avoid opening/locking the cache unless something is actually stale.
        if bookmarks.iter().all(|b| b.path.exists()) {
            return;
        }
        let (preferred_roots, _) = library_roots(settings);
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let Ok(guard) = cache.lock() else {
            return;
        };
        for bookmark in bookmarks.iter_mut() {
            if bookmark.path.exists() {
                continue;
            }
            let Some(identity) = bookmark.identity else {
                continue;
            };
            match guard.find_current_path(identity, &preferred_roots) {
                Ok(Some(current)) => {
                    bookmark.path = current;
                }
                Ok(None) => {}
                Err(e) => error!("bookmark path resolution {}: {e:#}", bookmark.id),
            }
        }
    }

    pub async fn add_bookmark(&self, bookmark: NewBookmark) -> anyhow::Result<Bookmark> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .add_bookmark(bookmark)
    }

    pub async fn remove_bookmark(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_bookmark(id)
    }

    pub async fn update_bookmark_note(
        &self,
        id: &str,
        note: Option<String>,
    ) -> anyhow::Result<Bookmark> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_bookmark_note(id, note)
    }

    pub async fn cluster_bookmarks(
        self: Arc<Self>,
        query: BookmarkClustersQuery,
    ) -> Result<BookmarkClustersResult, String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before grouping bookmarks.",
        )?;

        let granularity = query.granularity;
        let mut requested_ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in query.bookmark_ids {
            if seen.insert(id.clone()) {
                requested_ids.push(id);
            }
        }
        if requested_ids.is_empty() {
            return Ok(BookmarkClustersResult::default());
        }

        let bookmarks = self.list_bookmarks().await.map_err(|e| e.to_string())?;
        let mut by_id: std::collections::HashMap<String, Bookmark> = bookmarks
            .into_iter()
            .map(|bookmark| (bookmark.id.clone(), bookmark))
            .collect();
        let mut prepared = Vec::new();
        let mut inherently_unclustered = Vec::new();
        for id in &requested_ids {
            let bookmark = by_id
                .remove(id)
                .ok_or_else(|| format!("Bookmark not found: {id}"))?;
            let input = bookmark_embedding_input(&bookmark);
            if input.trim().is_empty() {
                inherently_unclustered.push(id.clone());
                continue;
            }
            let input_hash = format!("{:x}", Sha256::digest(input.as_bytes()));
            prepared.push((id.clone(), input, input_hash));
        }

        if prepared.len() < 3 {
            return Ok(BookmarkClustersResult {
                clusters: Vec::new(),
                unclustered_bookmark_ids: requested_ids,
            });
        }

        let embedder = self.embedder.lock().clone().ok_or_else(|| {
            "Semantic model unavailable. Build or restore the semantic index first.".to_string()
        })?;
        let engine = embedder.engine().as_str().to_string();
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();

        let cached = self
            .research_store()
            .map_err(|e| e.to_string())?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cached_bookmark_embeddings(
                &engine,
                &model_id,
                dimension,
                BOOKMARK_EMBEDDING_RECIPE_VERSION,
            )
            .map_err(|e| e.to_string())?;
        let cached_by_id: std::collections::HashMap<_, _> = cached
            .into_iter()
            .map(|cached| (cached.bookmark_id.clone(), cached))
            .collect();

        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; prepared.len()];
        let mut misses = Vec::new();
        for (index, (id, input, input_hash)) in prepared.iter().enumerate() {
            match cached_by_id.get(id) {
                Some(cached) if cached.input_hash == *input_hash => {
                    vectors[index] = Some(cached.embedding.clone());
                }
                _ => misses.push((index, id.clone(), input.clone(), input_hash.clone())),
            }
        }

        if !misses.is_empty() {
            let texts: Vec<String> = misses
                .iter()
                .map(|(_, _, input, _)| input.clone())
                .collect();
            let embedder_for_task = Arc::clone(&embedder);
            let embeddings = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                embedder_for_task.embed_passages(&refs)
            })
            .await
            .map_err(|e| format!("Bookmark embedding task panicked: {e}"))?
            .map_err(|e| format!("Could not embed bookmarks: {e:#}"))?;
            if embeddings.len() != misses.len() {
                return Err(format!(
                    "Embedder returned {} bookmark vectors for {} inputs",
                    embeddings.len(),
                    misses.len()
                ));
            }

            let store = self.research_store().map_err(|e| e.to_string())?;
            let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
            for ((index, id, _, input_hash), embedding) in misses.into_iter().zip(embeddings) {
                if embedding.len() != dimension {
                    return Err(format!(
                        "Bookmark embedding dimension mismatch. Expected {dimension}, received {}",
                        embedding.len()
                    ));
                }
                if embedding.iter().any(|value| !value.is_finite()) {
                    return Err("Embedder returned a non-finite bookmark vector".to_string());
                }
                let cached = CachedBookmarkEmbedding {
                    bookmark_id: id,
                    input_hash,
                    embedding: embedding.clone(),
                };
                if let Err(error) = store.upsert_bookmark_embedding(
                    &cached,
                    &engine,
                    &model_id,
                    dimension,
                    BOOKMARK_EMBEDDING_RECIPE_VERSION,
                ) {
                    error!("Could not cache bookmark embedding: {error:#}");
                }
                vectors[index] = Some(embedding);
            }
        }

        let vectors: Vec<Vec<f32>> = vectors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "A bookmark embedding was not produced".to_string())?;
        let clustered = tokio::task::spawn_blocking(move || {
            wilkes_core::embed::cluster::cluster_embeddings(&vectors, granularity)
        })
        .await
        .map_err(|e| format!("Bookmark clustering task panicked: {e}"))?
        .map_err(|e| e.to_string())?;

        let mut clusters: Vec<BookmarkCluster> = clustered
            .clusters
            .into_iter()
            .map(|cluster| {
                let members: Vec<(String, String)> = cluster
                    .item_indices
                    .iter()
                    .map(|index| (prepared[*index].0.clone(), prepared[*index].2.clone()))
                    .collect();
                BookmarkCluster {
                    cluster_key: cluster_key(&members),
                    bookmark_ids: members.iter().map(|(id, _)| id.clone()).collect(),
                    representative_bookmark_id: prepared[cluster.representative_index].0.clone(),
                    cohesion: cluster.cohesion,
                    label: None,
                }
            })
            .collect();

        // Labels are asynchronous: decode is ~370ms each, so labelling inline
        // would add seconds to a call that is otherwise pure compute.
        self.attach_cluster_labels(&mut clusters, &prepared).await;

        let clustered_unassigned = clustered
            .unclustered_indices
            .into_iter()
            .map(|index| prepared[index].0.clone());
        inherently_unclustered.extend(clustered_unassigned);
        let unclustered_set: std::collections::HashSet<_> =
            inherently_unclustered.into_iter().collect();
        let unclustered_bookmark_ids = requested_ids
            .into_iter()
            .filter(|id| unclustered_set.contains(id))
            .collect();

        Ok(BookmarkClustersResult {
            clusters,
            unclustered_bookmark_ids,
        })
    }

    /// Fill in cached labels, then kick off a background run for the misses.
    /// Returns immediately; late labels arrive as `bookmark-cluster-labelled`
    /// events, patched by `cluster_key`.
    async fn attach_cluster_labels(
        self: &Arc<Self>,
        clusters: &mut [BookmarkCluster],
        prepared: &[(String, String, String)],
    ) {
        // A newer run supersedes the previous one outright: its results are for
        // a partition that is no longer displayed.
        if let Some(previous) = self.cluster_label_task.lock().take() {
            previous.abort();
        }
        if clusters.is_empty() {
            return;
        }

        let Some(generator) = self.generator.lock().clone() else {
            return;
        };
        let settings = self.generation_settings().await;
        if !settings.enabled {
            return;
        }
        let model_id = generator.model_id().to_string();

        let keys: Vec<String> = clusters.iter().map(|c| c.cluster_key.clone()).collect();
        let cached = match self.research_store() {
            Ok(store) => store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .cached_cluster_labels(&keys, &model_id, BOOKMARK_CLUSTER_LABEL_RECIPE_VERSION),
            Err(e) => Err(e),
        };
        let cached = match cached {
            Ok(cached) => cached,
            Err(e) => {
                error!("Could not read cached cluster labels: {e:#}");
                return;
            }
        };

        let inputs: std::collections::HashMap<&str, &str> = prepared
            .iter()
            .map(|(id, input, _)| (id.as_str(), input.as_str()))
            .collect();

        let mut pending: Vec<(String, Vec<String>)> = Vec::new();
        for cluster in clusters.iter_mut() {
            let members = label_inputs(cluster, &inputs);
            if let Some(label) = cached.get(&cluster.cluster_key) {
                let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                if validate_cluster_label(label, &refs).is_ok() {
                    cluster.label = Some(label.clone());
                    continue;
                }
                warn!(
                    "Ignoring invalid cached cluster label for {}",
                    cluster.cluster_key
                );
            }
            pending.push((cluster.cluster_key.clone(), members));
        }

        if pending.is_empty() {
            return;
        }
        if clusters.len() > MAX_LABELLED_CLUSTERS {
            info!(
                "Skipping cluster labelling: {} clusters exceeds the {MAX_LABELLED_CLUSTERS} cap",
                clusters.len()
            );
            return;
        }

        let ctx = Arc::clone(self);
        let task = tokio::spawn(async move {
            for (key, members) in pending {
                let generator = Arc::clone(&generator);
                let members_for_task = members.clone();
                let generated = tokio::task::spawn_blocking(move || {
                    let refs: Vec<&str> = members_for_task.iter().map(String::as_str).collect();
                    cluster_label(generator.as_ref(), &refs)
                })
                .await;

                let label = match generated {
                    Ok(Ok(label)) => label,
                    // Every failure path leaves the label absent and logs: a
                    // missing label is not something the user asked for.
                    Ok(Err(e)) => {
                        warn!("Could not label cluster {key}: {e:#}");
                        continue;
                    }
                    Err(e) => {
                        warn!("Cluster label task failed for {key}: {e}");
                        continue;
                    }
                };

                if let Ok(store) = ctx.research_store() {
                    if let Err(e) = store
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .upsert_cluster_label(
                            &key,
                            &label,
                            &model_id,
                            BOOKMARK_CLUSTER_LABEL_RECIPE_VERSION,
                        )
                    {
                        error!("Could not cache cluster label: {e:#}");
                    }
                }

                ctx.events.emit(
                    "bookmark-cluster-labelled",
                    serde_json::json!({ "cluster_key": key, "label": label }),
                );
            }
        });
        *self.cluster_label_task.lock() = Some(task);
    }

    fn invalidate_topic_tree_cache(&self) {
        self.semantic_index_revision.fetch_add(1, Ordering::AcqRel);
        *self.topic_tree_caches.lock() = TopicTreeCaches::default();
    }

    fn matching_topic_tree_cache(
        &self,
        root: &Path,
        path: Option<&Path>,
        requested_input_cap: usize,
        index_revision: u64,
    ) -> Option<Arc<TopicTreeCache>> {
        self.topic_tree_caches
            .lock()
            .slot(path)
            .as_ref()
            .filter(|cached| cached.matches(root, path, requested_input_cap, index_revision))
            .cloned()
    }

    fn start_topic_operation(
        &self,
        request_id: &str,
    ) -> Result<(CancellationToken, Arc<AtomicBool>), String> {
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err("Invalid chunk-topic request id".to_string());
        }
        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let previous = self.topic_operations.lock().insert(
            request_id.to_string(),
            TopicOperation {
                cancel: cancel.clone(),
                cancel_flag: Arc::clone(&cancel_flag),
                label_task: None,
            },
        );
        if let Some(previous) = previous {
            previous.cancel_flag.store(true, Ordering::Relaxed);
            previous.cancel.cancel();
            if let Some(task) = previous.label_task {
                task.abort();
            }
        }
        Ok((cancel, cancel_flag))
    }

    fn finish_topic_operation(&self, request_id: &str) {
        self.topic_operations.lock().remove(request_id);
    }

    pub fn cancel_chunk_topics(&self, request_id: &str) {
        if let Some(operation) = self.topic_operations.lock().remove(request_id) {
            operation.cancel_flag.store(true, Ordering::Relaxed);
            operation.cancel.cancel();
            if let Some(task) = operation.label_task {
                task.abort();
            }
        }
    }

    async fn topic_tree_for(
        self: &Arc<Self>,
        root: PathBuf,
        path: Option<PathBuf>,
        requested_input_cap: usize,
        cancel: &CancellationToken,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<Arc<TopicTreeCache>, String> {
        loop {
            if cancel.is_cancelled() {
                return Err("Chunk topic operation cancelled".to_string());
            }
            let index_revision = self.semantic_index_revision.load(Ordering::Acquire);
            if let Some(cached) = self.matching_topic_tree_cache(
                &root,
                path.as_deref(),
                requested_input_cap,
                index_revision,
            ) {
                return Ok(cached);
            }

            let _build_guard = tokio::select! {
                guard = self.topic_tree_build_lock.lock() => guard,
                _ = cancel.cancelled() => {
                    return Err("Chunk topic operation cancelled".to_string());
                }
            };
            let index_revision = self.semantic_index_revision.load(Ordering::Acquire);
            if let Some(cached) = self.matching_topic_tree_cache(
                &root,
                path.as_deref(),
                requested_input_cap,
                index_revision,
            ) {
                return Ok(cached);
            }

            let index_arc = self.index.lock().clone();
            let build_root = root.clone();
            let build_path = path.clone();
            let build_cancel = Arc::clone(cancel_flag);
            let built = tokio::task::spawn_blocking(move || {
                // Release the database mutex before Ward starts. The bulk read
                // is the only part that needs the live semantic index.
                let all = {
                    let guard = index_arc
                        .lock()
                        .map_err(|_| "Semantic index lock was poisoned".to_string())?;
                    let index = guard.as_ref().ok_or_else(|| {
                        "Semantic index unavailable. Build or restore the semantic index first."
                            .to_string()
                    })?;
                    match build_path.as_deref() {
                        Some(path) => index.topic_chunks_for_file(&build_root, path),
                        None => index.topic_chunks_for_root(&build_root),
                    }
                    .map_err(|error| format!("Could not load indexed chunks: {error:#}"))?
                };

                if build_cancel.load(Ordering::Relaxed) {
                    return Err("Chunk topic operation cancelled".to_string());
                }

                let total_chunk_count = all.len();
                let input_cap = requested_input_cap.min(total_chunk_count);
                let total_document_count = all
                    .iter()
                    .map(|chunk| chunk.file_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let mut sampled = cap_topic_chunks(all, input_cap);
                let sampled_document_count = sampled
                    .iter()
                    .map(|chunk| chunk.file_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len();

                // Root caches need passage metadata, not another copy of every
                // vector, so they retain only the Ward similarity matrix.
                // Document caches keep their bounded vectors as the prototypes
                // needed to project local topics across the full library.
                let vectors: Vec<Vec<f32>> = sampled
                    .iter_mut()
                    .map(|chunk| std::mem::take(&mut chunk.embedding))
                    .collect();
                let tree = WardTree::build_with_cancel(&vectors, &build_cancel)
                    .map_err(|error| format!("Could not cluster indexed chunks: {error:#}"))?;
                let sampled_embeddings = build_path.is_some().then_some(vectors);

                Ok::<_, String>(Arc::new(TopicTreeCache {
                    root: build_root,
                    path: build_path,
                    requested_input_cap,
                    index_revision,
                    tree,
                    sampled,
                    sampled_embeddings,
                    total_chunk_count,
                    total_document_count,
                    sampled_document_count,
                    input_cap,
                }))
            })
            .await
            .map_err(|error| format!("Chunk topic task panicked: {error}"))??;

            if cancel.is_cancelled() {
                return Err("Chunk topic operation cancelled".to_string());
            }
            let mut caches = self.topic_tree_caches.lock();
            if self.semantic_index_revision.load(Ordering::Acquire) == index_revision {
                caches.set(path.as_deref(), Arc::clone(&built));
                return Ok(built);
            }
            // The index changed while Ward was building. Discard the stale
            // tree and retry against the new revision before serving results.
        }
    }

    pub async fn chunk_topics(
        self: Arc<Self>,
        request_id: String,
        query: ChunkTopicsQuery,
    ) -> Result<ChunkTopicsResult, String> {
        let (cancel, cancel_flag) = self.start_topic_operation(&request_id)?;
        let result = Arc::clone(&self)
            .chunk_topics_inner(&request_id, query, &cancel, &cancel_flag)
            .await;
        if result.is_err() {
            self.finish_topic_operation(&request_id);
        }
        result
    }

    async fn chunk_topics_inner(
        self: Arc<Self>,
        request_id: &str,
        query: ChunkTopicsQuery,
        cancel: &CancellationToken,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<ChunkTopicsResult, String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before finding topics.",
        )?;
        if query.root.as_os_str().is_empty() {
            return Err("Choose a library root before finding topics.".to_string());
        }

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Chunk topics root")?;
        let path = match query.path {
            Some(path) => {
                let (path, _) = Self::canonicalize_supported_file(
                    &root,
                    &path,
                    &settings.supported_extensions,
                    "Chunk topics",
                )?;
                Self::ensure_path_in_library(&path, &library_roots, "Chunk topics file")?;
                Some(path)
            }
            None => None,
        };
        let requested_input_cap = settings.semantic.topic_cloud_input_cap.max(3);
        let granularity = query.granularity;
        let cached = self
            .topic_tree_for(root, path, requested_input_cap, cancel, cancel_flag)
            .await?;
        let cached_for_cut = Arc::clone(&cached);
        let cut_cancel = Arc::clone(cancel_flag);
        let (mut result, coverage_prototypes) = tokio::task::spawn_blocking(move || {
            let clustered = cached_for_cut
                .tree
                .cut_with_cancel(granularity, &cut_cancel)
                .map_err(|error| format!("Could not cut chunk topic tree: {error:#}"))?;
            let document_scoped = cached_for_cut.path.is_some();
            let document_embeddings =
                match (document_scoped, cached_for_cut.sampled_embeddings.as_ref()) {
                    (true, Some(embeddings)) => Some(embeddings),
                    (true, None) => {
                        return Err(
                            "Document topic cache is missing retained embeddings".to_string()
                        )
                    }
                    (false, _) => None,
                };
            let mut topics = Vec::with_capacity(clustered.clusters.len());
            let mut coverage_prototypes = Vec::with_capacity(clustered.clusters.len());
            for cluster in clustered.clusters {
                if let Some(embeddings) = document_embeddings {
                    coverage_prototypes.push(TopicCoveragePrototype {
                        mean_member_embedding: mean_normalized_embeddings(
                            embeddings,
                            &cluster.item_indices,
                        )?,
                        cohesion: cluster.cohesion,
                    });
                }
                let chunks: Vec<ChunkTopicMember> = cluster
                    .item_indices
                    .iter()
                    .map(|index| topic_member(&cached_for_cut.sampled[*index]))
                    .collect();
                let distinct_document_count = chunks
                    .iter()
                    .map(|chunk| &chunk.file_path)
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                topics.push(ChunkTopic {
                    cluster_key: chunk_cluster_key(chunks.iter().map(|chunk| chunk.chunk_id)),
                    chunk_count: chunks.len(),
                    distinct_document_count,
                    chunks,
                    representative_chunk_id: cached_for_cut.sampled[cluster.representative_index]
                        .chunk_id,
                    cohesion: cluster.cohesion,
                    library_coverage: None,
                    label: None,
                });
            }
            Ok::<_, String>((
                ChunkTopicsResult {
                    topics,
                    total_chunk_count: cached_for_cut.total_chunk_count,
                    sampled_chunk_count: cached_for_cut.sampled.len(),
                    total_document_count: cached_for_cut.total_document_count,
                    sampled_document_count: cached_for_cut.sampled_document_count,
                    input_cap: cached_for_cut.input_cap,
                },
                coverage_prototypes,
            ))
        })
        .await
        .map_err(|e| format!("Chunk topic task panicked: {e}"))??;

        if let Some(source_path) = cached.path.clone() {
            if self.semantic_index_revision.load(Ordering::Acquire) != cached.index_revision {
                return Err("Semantic index changed while calculating topic coverage".to_string());
            }
            let index_arc = self.index.lock().clone();
            let coverage_roots = library_roots;
            let coverage_cancel = Arc::clone(cancel_flag);
            let coverage = tokio::task::spawn_blocking(move || {
                let guard = index_arc
                    .lock()
                    .map_err(|_| "Semantic index lock was poisoned".to_string())?;
                let index = guard.as_ref().ok_or_else(|| {
                    "Semantic index unavailable. Build or restore the semantic index first."
                        .to_string()
                })?;
                index
                    .topic_library_coverage(
                        &coverage_roots,
                        &source_path,
                        &coverage_prototypes,
                        &coverage_cancel,
                    )
                    .map_err(|error| format!("Could not calculate topic coverage: {error:#}"))
            })
            .await
            .map_err(|error| format!("Topic coverage task panicked: {error}"))??;
            if self.semantic_index_revision.load(Ordering::Acquire) != cached.index_revision {
                return Err("Semantic index changed while calculating topic coverage".to_string());
            }
            if coverage.related_document_counts.len() != result.topics.len()
                || coverage.related_chunks.len() != result.topics.len()
            {
                return Err("Topic coverage result count does not match topic count".to_string());
            }
            for ((topic, related_document_count), chunks) in result
                .topics
                .iter_mut()
                .zip(coverage.related_document_counts)
                .zip(coverage.related_chunks)
            {
                topic.library_coverage = Some(TopicLibraryCoverage {
                    related_document_count,
                    eligible_document_count: coverage.eligible_document_count,
                    chunks,
                });
            }
        }

        self.attach_chunk_topic_labels(
            request_id,
            Arc::clone(cancel_flag),
            &mut result.topics,
            &cached.sampled,
        )
        .await;
        Ok(result)
    }

    /// Fill cached topic labels immediately and generate only the misses in the
    /// background. Late labels are patched by the membership-derived key.
    async fn attach_chunk_topic_labels(
        self: &Arc<Self>,
        request_id: &str,
        cancel_flag: Arc<AtomicBool>,
        topics: &mut [ChunkTopic],
        sampled: &[TopicChunkData],
    ) {
        if topics.is_empty() {
            self.finish_topic_operation(request_id);
            return;
        }
        let Some(generator) = self.generator.lock().clone() else {
            self.finish_topic_operation(request_id);
            return;
        };
        let settings = self.generation_settings().await;
        if !settings.enabled {
            self.finish_topic_operation(request_id);
            return;
        }
        let model_id = generator.model_id().to_string();
        let keys: Vec<String> = topics
            .iter()
            .map(|topic| topic.cluster_key.clone())
            .collect();
        let cached = match self.research_store() {
            Ok(store) => store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .cached_chunk_cluster_labels(&keys, &model_id, CHUNK_CLUSTER_LABEL_RECIPE_VERSION),
            Err(error) => Err(error),
        };
        let cached = match cached {
            Ok(cached) => cached,
            Err(error) => {
                error!("Could not read cached chunk-cluster labels: {error:#}");
                self.finish_topic_operation(request_id);
                return;
            }
        };

        let text_by_id: std::collections::HashMap<i64, &str> = sampled
            .iter()
            .map(|chunk| (chunk.chunk_id, chunk.chunk_text.as_str()))
            .collect();
        let mut pending = Vec::new();
        for topic in topics.iter_mut() {
            let members = chunk_label_inputs(topic, &text_by_id);
            if let Some(label) = cached.get(&topic.cluster_key) {
                let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                if validate_cluster_label(label, &refs).is_ok() {
                    topic.label = Some(label.clone());
                    continue;
                }
                warn!(
                    "Ignoring invalid cached chunk-topic label for {}",
                    topic.cluster_key
                );
            }
            pending.push((topic.cluster_key.clone(), members));
        }
        if pending.is_empty() {
            self.finish_topic_operation(request_id);
            return;
        }
        let ctx = Arc::clone(self);
        let task_request_id = request_id.to_string();
        let task = tokio::spawn(async move {
            for (key, members) in pending {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                let generator = Arc::clone(&generator);
                let generation_cancel = Arc::clone(&cancel_flag);
                let generated = tokio::task::spawn_blocking(move || {
                    let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                    cluster_label_stream(generator.as_ref(), &refs, &mut |_| {
                        if generation_cancel.load(Ordering::Relaxed) {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    })
                })
                .await;
                let label = match generated {
                    Ok(Ok(label)) => label,
                    Ok(Err(error)) => {
                        if cancel_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        warn!("Could not label chunk topic {key}: {error:#}");
                        continue;
                    }
                    Err(error) => {
                        warn!("Chunk-topic label task failed for {key}: {error}");
                        continue;
                    }
                };
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(store) = ctx.research_store() {
                    if let Err(error) = store
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .upsert_chunk_cluster_label(
                            &key,
                            &label,
                            &model_id,
                            CHUNK_CLUSTER_LABEL_RECIPE_VERSION,
                        )
                    {
                        error!("Could not cache chunk-topic label: {error:#}");
                    }
                }
                ctx.events.emit(
                    "chunk-topic-labelled",
                    serde_json::json!({
                        "request_id": task_request_id.clone(),
                        "cluster_key": key,
                        "label": label
                    }),
                );
            }
            ctx.finish_topic_operation(&task_request_id);
        });
        let mut operations = self.topic_operations.lock();
        if let Some(operation) = operations.get_mut(request_id) {
            operation.label_task = Some(task);
        } else {
            task.abort();
        }
    }

    pub async fn list_files(
        &self,
        root: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        self.list_files_filtered(root, None, &[], None).await
    }

    pub async fn list_files_filtered(
        &self,
        root: PathBuf,
        collection_id: Option<&str>,
        tag_ids: &[String],
        collection_expression: Option<&str>,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        let s = self.get_settings().await;
        let mut response = crate::commands::files::list_files(
            root.clone(),
            s.supported_extensions.clone(),
            s.max_file_size,
        )
        .await?;

        // Populate document metadata from the cache and
        // schedule background extraction for anything not yet cached.
        let mut misses: Vec<PathBuf> = Vec::new();
        if let Some(cache) = self.metadata_cache() {
            let primary_source = metadata_source_preference(&s.primary_metadata_source);
            if let Ok(guard) = cache.lock() {
                for entry in response.files.iter_mut() {
                    let Some(identity) = FileIdentity::from_entry(entry) else {
                        continue;
                    };
                    match guard.get_valid_with_primary(&entry.path, identity, primary_source) {
                        Ok(Some(cached)) => {
                            let meta = cached.metadata;
                            let citation_count = provider_citation_count(&meta);
                            entry.title = meta.title;
                            entry.author = meta.author;
                            entry.doi = meta.doi;
                            entry.publication_date = meta.created_at;
                            entry.citation_count = citation_count;
                            entry.metadata_conflicts = cached
                                .conflicts
                                .into_iter()
                                .map(|(key, values)| {
                                    (
                                        key,
                                        values
                                            .into_iter()
                                            .map(|value| MetadataConflictValue {
                                                source: value.source,
                                                value: value.value,
                                            })
                                            .collect(),
                                    )
                                })
                                .collect();
                        }
                        Ok(None) => misses.push(entry.path.clone()),
                        Err(e) => error!("metadata cache read {}: {e:#}", entry.path.display()),
                    }
                }
            }

            if !misses.is_empty() {
                self.spawn_metadata_fill(misses, s.clone(), cache);
            }
        }

        let store = self.research_store()?;
        let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
        store.enrich_files(&mut response.files)?;
        if !tag_ids.is_empty() {
            response.files.retain(|entry| {
                tag_ids
                    .iter()
                    .all(|id| entry.tags.iter().any(|tag| tag.id == *id))
            });
            response.omitted.retain(|entry| {
                tag_ids
                    .iter()
                    .all(|id| entry.file.tags.iter().any(|tag| tag.id == *id))
            });
        }
        if let Some(collection_id) = collection_id {
            let eligible = store.eligible_paths(collection_id, &root, &response.files)?;
            response
                .files
                .retain(|entry| eligible.contains(&entry.path));
            response
                .omitted
                .retain(|entry| eligible.contains(&entry.file.path));
        }
        if let Some(expression) = collection_expression.filter(|value| !value.trim().is_empty()) {
            let eligible =
                store.eligible_paths_for_expression(expression, &root, &response.files)?;
            response
                .files
                .retain(|entry| eligible.contains(&entry.path));
            response
                .omitted
                .retain(|entry| eligible.contains(&entry.file.path));
        }

        Ok(response)
    }

    fn research_store(&self) -> anyhow::Result<Arc<Mutex<ResearchStore>>> {
        let mut guard = self.research_store.lock();
        if let Some(store) = guard.as_ref() {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(Mutex::new(ResearchStore::open(
            &self.data_dir,
            &self.bookmarks_path,
        )?));
        *guard = Some(Arc::clone(&store));
        Ok(store)
    }

    pub fn list_tags(&self) -> anyhow::Result<Vec<Tag>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_tags()
    }

    pub fn create_tag(&self, new: NewTag) -> anyhow::Result<Tag> {
        let tag = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .create_tag(new)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(tag)
    }

    pub fn update_tag(&self, id: &str, update: UpdateTag) -> anyhow::Result<Tag> {
        let tag = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_tag(id, update)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(tag)
    }

    pub fn delete_tag(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_tag(id)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(())
    }

    pub fn update_document_tags(&self, update: DocumentTagUpdate) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_document_tags(update)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"document_tags"}),
        );
        Ok(())
    }

    pub fn validate_collection(&self, expression: &str) -> CollectionValidation {
        ResearchStore::validate_collection(expression)
    }

    pub fn list_collections(&self) -> anyhow::Result<Vec<SmartCollection>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_collections()
    }

    pub fn create_collection(&self, new: NewSmartCollection) -> anyhow::Result<SmartCollection> {
        let item = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .create_collection(new)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(item)
    }

    pub fn update_collection(
        &self,
        id: &str,
        update: UpdateSmartCollection,
    ) -> anyhow::Result<SmartCollection> {
        let item = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_collection(id, update)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(item)
    }

    pub fn delete_collection(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_collection(id)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(())
    }

    pub fn list_search_log(&self, limit: usize) -> anyhow::Result<Vec<SearchLogEntry>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_search_log(limit)
    }

    pub fn delete_search_log(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_search_log(id)
    }

    pub fn clear_search_log(&self) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear_search_log()
    }

    /// Lazily open (once) the shared document-metadata cache. Returns `None` if
    /// it cannot be opened; callers then simply skip metadata population.
    fn metadata_cache(&self) -> Option<Arc<Mutex<MetadataCache>>> {
        let mut guard = self.metadata_cache.lock();
        if let Some(cache) = guard.as_ref() {
            return Some(Arc::clone(cache));
        }
        match MetadataCache::open(&self.data_dir) {
            Ok(cache) => {
                let arc = Arc::new(Mutex::new(cache));
                *guard = Some(Arc::clone(&arc));
                Some(arc)
            }
            Err(e) => {
                error!("metadata cache open failed: {e:#}");
                None
            }
        }
    }

    /// Extract document metadata for the given paths off the request path,
    /// upsert it into the cache, and emit `file-metadata-updated` so the UI can
    /// fill in and re-sort. A file whose content matches a stale cache row
    /// (a rename) is re-keyed instead of re-extracted.
    fn spawn_metadata_fill(
        &self,
        paths: Vec<PathBuf>,
        settings: Settings,
        cache: Arc<Mutex<MetadataCache>>,
    ) {
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            let primary_source = metadata_source_preference(&settings.primary_metadata_source);
            // Pass 1: file-based extraction (blocking). Emits immediately so the
            // list gets publication dates fast, mirroring the on-open viewer's
            // fast first paint before the Zotero upgrade.
            let exts = settings.supported_extensions.clone();
            let cache1 = Arc::clone(&cache);
            let pass1 = tokio::task::spawn_blocking(move || {
                let registry = crate::commands::metadata::build_registry(exts);
                let mut eligible: Vec<(PathBuf, FileIdentity, DocumentMetadata)> = Vec::new();
                let mut updates: Vec<serde_json::Value> = Vec::new();
                for path in paths {
                    let Some(identity) = FileIdentity::for_path(&path) else {
                        continue;
                    };
                    match extract_or_rekey(&cache1, &registry, &path, identity) {
                        FillOutcome::Extracted(metadata) => {
                            updates.push(metadata_update_json_from_cache(
                                &cache1,
                                &path,
                                identity,
                                primary_source,
                                &metadata,
                            ));
                            eligible.push((path, identity, metadata));
                        }
                        // A rename hit already carries composed metadata (whose
                        // Zotero source, if any, was preserved by re-keying), so
                        // it is not eligible for a fresh Zotero resolve.
                        FillOutcome::Renamed(metadata) => {
                            updates.push(metadata_update_json_from_cache(
                                &cache1,
                                &path,
                                identity,
                                primary_source,
                                &metadata,
                            ));
                        }
                    }
                }
                (eligible, updates)
            })
            .await;

            let (eligible, updates) = match pass1 {
                Ok(v) => v,
                Err(e) => {
                    error!("metadata fill task failed: {e}");
                    return;
                }
            };
            if !updates.is_empty() {
                events.emit("file-metadata-updated", serde_json::json!(updates));
            }

            if eligible.is_empty() {
                return;
            }

            let mut composed = eligible;

            // Pass 2: authoritative Zotero override (async). Fetch the attachment
            // list once for the whole batch, then resolve each file locally.
            if settings.integrations.zotero.enabled {
                let client = ZoteroClient::from_settings(&settings.integrations.zotero);
                let attachments = match client.attachment_items().await {
                    Ok(a) => Some(a),
                    Err(e) => {
                        info!("metadata fill: zotero attachment fetch failed: {e:#}");
                        None
                    }
                };
                if let Some(attachments) = attachments {
                    let mut z_updates: Vec<serde_json::Value> = Vec::new();
                    for (path, identity, metadata) in composed.iter_mut() {
                        if let Some(z) =
                            zotero_override_for(&client, path, metadata, &attachments).await
                        {
                            if let Ok(guard) = cache.lock() {
                                if let Err(e) =
                                    guard.upsert(path, *identity, &z, MetadataSource::Zotero)
                                {
                                    error!(
                                        "metadata cache zotero upsert {}: {e:#}",
                                        path.display()
                                    );
                                }
                            }
                            *metadata = z;
                            z_updates.push(metadata_update_json_from_cache(
                                &cache,
                                path,
                                *identity,
                                primary_source,
                                metadata,
                            ));
                        }
                    }
                    if !z_updates.is_empty() {
                        events.emit("file-metadata-updated", serde_json::json!(z_updates));
                    }
                }
            }

            // Pass 3: Semantic Scholar citation enrichment. This writes into
            // the same file_metadata rows as publication_date, keyed by the
            // composed DOI for each file.
            if settings.integrations.semantic_scholar.enabled {
                let client =
                    SemanticScholarClient::from_settings(&settings.integrations.semantic_scholar);
                let mut s2_updates: Vec<serde_json::Value> = Vec::new();
                for (path, identity, metadata) in composed.iter_mut() {
                    if let Some(enriched) = semantic_scholar_enrichment_for(&client, metadata).await
                    {
                        metadata.semantic_scholar = Some(enriched);
                        if let Ok(guard) = cache.lock() {
                            if let Err(e) = guard.upsert(
                                path,
                                *identity,
                                metadata,
                                MetadataSource::SemanticScholar,
                            ) {
                                error!(
                                    "metadata cache semantic scholar upsert {}: {e:#}",
                                    path.display()
                                );
                            }
                        }
                        s2_updates.push(metadata_update_json_from_cache(
                            &cache,
                            path,
                            *identity,
                            primary_source,
                            metadata,
                        ));
                    }
                }
                if !s2_updates.is_empty() {
                    events.emit("file-metadata-updated", serde_json::json!(s2_updates));
                }
            }

            if settings.integrations.openalex.enabled {
                let client = OpenAlexClient::from_settings(&settings.integrations.openalex);
                let mut openalex_updates: Vec<serde_json::Value> = Vec::new();
                for (path, identity, metadata) in composed.iter_mut() {
                    if let Some(enriched) = openalex_enrichment_for(&client, metadata).await {
                        metadata.openalex = Some(enriched);
                        if let Ok(guard) = cache.lock() {
                            if let Err(e) =
                                guard.upsert(path, *identity, metadata, MetadataSource::OpenAlex)
                            {
                                error!("metadata cache openalex upsert {}: {e:#}", path.display());
                            }
                        }
                        // Populate the citation graph from the same enrichment
                        // pass. Edges are keyed by DOI, so this is independent
                        // of whether the referenced papers are in the library.
                        if let Some(doi) = metadata.doi.clone() {
                            match client.references(&doi).await {
                                Ok(targets) => {
                                    if let Ok(guard) = cache.lock() {
                                        if let Err(e) = guard.replace_citations(&doi, &targets) {
                                            error!(
                                                "metadata cache citations {}: {e:#}",
                                                path.display()
                                            );
                                        }
                                    }
                                }
                                Err(e) => info!("openalex references {doi}: {e:#}"),
                            }
                        }
                        openalex_updates.push(metadata_update_json_from_cache(
                            &cache,
                            path,
                            *identity,
                            primary_source,
                            metadata,
                        ));
                    }
                }
                if !openalex_updates.is_empty() {
                    events.emit("file-metadata-updated", serde_json::json!(openalex_updates));
                }
            }
        });
    }

    /// Opens a caller-authorized document path. Path confinement belongs to the
    /// transport boundary (for example MCP library roots or server uploads),
    /// because desktop library documents normally live outside `data_dir`.
    pub async fn open_file(&self, path: PathBuf) -> anyhow::Result<PreviewData> {
        let s = self.get_settings().await;
        crate::commands::files::open_file(path, s.supported_extensions).await
    }

    pub async fn rename_file(&self, path: PathBuf, new_name: String) -> anyhow::Result<PathBuf> {
        let old = path.clone();
        let new = crate::commands::files::rename_file(path, new_name).await?;
        self.rekey_research_path(&old, &new)?;
        self.rekey_index_path(&old, &new)?;
        Ok(new)
    }

    pub fn rekey_research_path(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .rekey_document(old, new)
    }

    /// Rewrites the semantic index keys for a renamed file or directory so its
    /// embeddings are preserved in place. A no-op when no index is loaded.
    pub fn rekey_index_path(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let index_arc = self.index.lock().clone();
        let mut guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(index) = guard.as_mut() {
            index.rename_file(old, new)?;
        }
        Ok(())
    }

    pub async fn import_files_into_current_root(
        &self,
        paths: Vec<PathBuf>,
        root: PathBuf,
        mode: crate::commands::files::FileImportMode,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let s = self.get_settings().await;
        let Some(current_root) = s.last_directory.clone() else {
            anyhow::bail!("Choose a directory before importing files");
        };
        let root_canonical = std::fs::canonicalize(&root).map_err(|err| {
            anyhow::anyhow!("Root directory not found: {} ({err})", root.display())
        })?;
        let current_root_canonical = std::fs::canonicalize(&current_root).map_err(|err| {
            anyhow::anyhow!(
                "Current root directory not found: {} ({err})",
                current_root.display()
            )
        })?;
        if root_canonical != current_root_canonical {
            anyhow::bail!(
                "Import target must be the current root: {}",
                current_root.display()
            );
        }

        let imported = crate::commands::files::import_files_into_root(
            paths,
            root,
            s.supported_extensions,
            mode,
        )
        .await?;
        self.emit_file_list_changed(&root_canonical);
        Ok(imported)
    }

    pub async fn get_file_metadata(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        let s = self.get_settings().await;
        crate::commands::metadata::get_file_metadata(path, s.supported_extensions).await
    }

    /// Richest available metadata for one document, in one authoritative order.
    /// Cache-first: when the file is cached and unchanged, return the composed
    /// record (title / author / DOI / publication date plus any Semantic
    /// Scholar or OpenAlex enrichment) exactly as the file list resolves it. On
    /// a cache miss fall back to [`Self::resolve_file_metadata`] (extraction +
    /// Zotero override), which yields the bibliographic basics for a file the
    /// cache has not processed yet. No third composition is introduced — this
    /// only chooses between the two the app already owns.
    pub async fn document_metadata_full(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        if let (Some(cache), Some(identity)) =
            (self.metadata_cache(), FileIdentity::for_path(&path))
        {
            let primary =
                metadata_source_preference(&self.get_settings().await.primary_metadata_source);
            let cached = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("metadata cache lock poisoned"))?
                .get_valid_with_primary(&path, identity, primary)?;
            if let Some(cached) = cached {
                return Ok(cached.metadata);
            }
        }
        self.resolve_file_metadata(path).await
    }

    pub async fn zotero_status(&self) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_status(s).await
    }

    pub async fn semantic_scholar_status(
        &self,
    ) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        let s = self.get_settings().await;
        crate::commands::integrations::semantic_scholar::semantic_scholar_status(s).await
    }

    pub async fn semantic_scholar_lookup(
        &self,
        doi: String,
    ) -> anyhow::Result<wilkes_core::types::SemanticScholarPaper> {
        let s = self.get_settings().await;
        crate::commands::integrations::semantic_scholar::semantic_scholar_lookup(
            s,
            self.metadata_cache(),
            doi,
        )
        .await
    }

    pub async fn openalex_status(&self) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        let s = self.get_settings().await;
        crate::commands::integrations::openalex::openalex_status(s).await
    }

    pub async fn openalex_lookup(
        &self,
        doi: String,
    ) -> anyhow::Result<wilkes_core::types::OpenAlexWork> {
        let s = self.get_settings().await;
        crate::commands::integrations::openalex::openalex_lookup(s, self.metadata_cache(), doi)
            .await
    }

    /// Authoritative document metadata: file-based extraction overridden by the
    /// Zotero library record when the file resolves to an item. This is the
    /// single owner of that composition — both the on-open viewer and the
    /// background tabulation resolve through it (the fill batches the Zotero
    /// attachment fetch, but composes identically).
    pub async fn resolve_file_metadata(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::resolve_file_metadata(s, path).await
    }

    /// Re-derive metadata for every cached file in place. Backs the manual
    /// "refresh metadata" action.
    ///
    /// Refresh never clears: clearing then repopulating loses any field the
    /// re-derivation fails to reproduce — a Zotero item that has since been
    /// removed from the library, or any field at all while Zotero is
    /// unreachable. Instead we re-run the same extraction + Zotero-override fill
    /// used on listing over the known files, writing through the merging
    /// [`MetadataCache::upsert`], which only overwrites a field when the new
    /// derivation actually produced a value and never lets file extraction
    /// clobber authoritative Zotero fields.
    ///
    /// Callers are responsible for confining explicit paths to the document
    /// roots they expose. The server does this at its HTTP boundary; desktop
    /// libraries normally live outside Wilkes' private application data
    /// directory.
    pub async fn refresh_file_metadata(&self, path: Option<PathBuf>) -> anyhow::Result<()> {
        let s = self.get_settings().await;
        let Some(cache) = self.metadata_cache() else {
            return Ok(());
        };
        let paths = if let Some(path) = path {
            vec![path]
        } else {
            match cache.lock() {
                Ok(guard) => guard.all_paths()?,
                Err(_) => return Ok(()),
            }
        };
        if !paths.is_empty() {
            self.spawn_metadata_fill(paths, s, cache);
        }
        Ok(())
    }

    pub async fn zotero_add_item(
        &self,
        path: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::AddOutcome> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_add_item(s, path).await
    }

    pub async fn zotero_generate_citation(
        &self,
        path: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::CitationResult> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_generate_citation(s, path).await
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    async fn settings(&self) -> Settings {
        self.get_settings().await
    }

    fn stop_directory_watcher(&self) {
        if let Some(mut w) = self.directory_watcher.lock().take() {
            w.stop();
        }
    }

    fn start_directory_watcher(self: &Arc<Self>, root: PathBuf) {
        self.stop_directory_watcher();
        if !root.exists() || !root.is_dir() {
            error!("directory watcher root is invalid: {}", root.display());
            return;
        }

        let ctx = Arc::clone(self);
        let runtime = tokio::runtime::Handle::current();
        match DirectoryWatcher::start(root.clone(), move |batch| {
            ctx.emit_file_list_changed(&batch.root);
            let ctx = Arc::clone(&ctx);
            runtime.spawn(async move {
                ctx.process_directory_change_for_semantic(batch).await;
            });
        }) {
            Ok(watcher) => *self.directory_watcher.lock() = Some(watcher),
            Err(err) => error!("directory watcher start failed: {err:#}"),
        }
    }

    fn on_directory_setting_maybe_changed(self: &Arc<Self>, before: &Settings, after: &Settings) {
        if before.last_directory == after.last_directory {
            return;
        }
        match after.last_directory.clone() {
            Some(root) => self.start_directory_watcher(root),
            None => self.stop_directory_watcher(),
        }
    }

    fn emit_file_list_changed(&self, root: &Path) {
        self.events.emit(
            "file-list-changed",
            serde_json::json!({ "root": root.display().to_string() }),
        );
    }

    async fn process_directory_change_for_semantic(self: Arc<Self>, batch: DirectoryChangeBatch) {
        let settings = self.get_settings().await;
        if !settings.search_prefer_semantic {
            return;
        }

        let Some(embedder) = self.embedder.lock().clone() else {
            return;
        };
        let index_arc = Arc::clone(&*self.index.lock());
        let has_index = index_arc
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        if !has_index {
            return;
        }
        // Invalidate before the updater can mutate rows. A concurrently
        // finishing tree build will observe the revision change and discard
        // its stale result instead of installing it.
        self.invalidate_topic_tree_cache();
        if let Ok(mut guard) = index_arc.lock() {
            if let Some(idx) = guard.as_mut() {
                if let Err(e) = idx.activate_root(&batch.root) {
                    error!(
                        "activate semantic update root {}: {e:#}",
                        batch.root.display()
                    );
                    return;
                }
            }
        }

        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));
        let registry = Arc::new(registry);
        let cache = self.metadata_cache();
        let config = Self::restore_state_indexing_config(&settings);
        let ev1 = Arc::clone(&self.events);
        let ev2 = Arc::clone(&self.events);

        let result = tokio::task::spawn_blocking(move || {
            process_directory_change(
                batch,
                &index_arc,
                &cache,
                &registry,
                &embedder,
                &config,
                &move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                &move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
            );
        })
        .await;

        if let Err(err) = result {
            error!("process_directory_change_for_semantic task panicked: {err}");
        }
    }

    fn embed_task_is_running(&self) -> bool {
        if self.embed_cancel_in_progress.load(Ordering::Acquire) {
            return true;
        }
        let guard = self.embed_task.lock();
        guard.as_ref().is_some_and(|task| !task.join.is_finished())
    }

    fn clear_embed_task(&self) {
        *self.embed_task.lock() = None;
        self.embed_cancel_in_progress
            .store(false, Ordering::Release);
    }

    async fn validate_index_root(
        &self,
        root: &Path,
        settings: &Settings,
        missing_dir_message: &str,
    ) -> Result<(), String> {
        let root_display = root.display().to_string();

        if root_display.trim().is_empty() {
            return Err(missing_dir_message.to_string());
        }
        if !root.exists() {
            return Err(format!("Index root not found: {}", root.display()));
        }
        if !root.is_dir() {
            return Err(format!("Index root is not a directory: {}", root.display()));
        }

        let files = crate::commands::files::list_files(
            root.to_path_buf(),
            settings.supported_extensions.clone(),
            settings.max_file_size,
        )
        .await
        .map_err(|err| format!("Failed to scan index root: {err}"))?;

        if files.files.is_empty() {
            return Err(format!(
                "No supported files found in selected directory: {}",
                root.display()
            ));
        }

        Ok(())
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    pub async fn update_semantic_settings<F>(&self, f: F)
    where
        F: FnOnce(SemanticSettings) -> SemanticSettings,
    {
        let _lock = self.settings_lock.lock().await;
        let current = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("update_semantic_settings: read: {e:#}");
                return;
            }
        };
        let semantic = f(current.semantic);
        if let Err(e) = update_settings(
            &self.settings_path,
            serde_json::json!({ "semantic": semantic }),
        )
        .await
        {
            error!("update_semantic_settings: write: {e:#}");
        }
    }

    /// Attach, detach or reload the generator when the generation settings
    /// change. Both the on and off transitions matter: leaving a generator
    /// attached after the feature is switched off would keep a multi-gigabyte
    /// process alive behind a toggle the user turned off.
    fn on_generation_settings_maybe_changed(self: &Arc<Self>, before: &Settings, after: &Settings) {
        let before = &before.generation;
        let after = &after.generation;
        let attachment_changed = before.enabled != after.enabled
            || before.engine != after.engine
            || before.model != after.model
            || (after.engine == GenerationEngine::Candle && before.device != after.device)
            || (after.engine == GenerationEngine::Ollama
                && (before.ollama_url != after.ollama_url
                    || before.context_tokens != after.context_tokens));
        if !attachment_changed {
            return;
        }
        let ctx = Arc::clone(self);
        let enabled = after.enabled;
        let identity_changed = before.engine != after.engine
            || before.model != after.model
            || before.device != after.device
            || before.ollama_url != after.ollama_url;
        if !enabled {
            info!("generation disabled; detaching the generator");
            self.unload_generator();
            return;
        }
        if identity_changed {
            // Detach synchronously with the settings transition. Until the new
            // backend is attached, the shared readiness predicate must not
            // accidentally bless the generator selected by the old settings.
            self.unload_generator();
        }
        tokio::spawn(async move {
            match ctx.load_generator().await {
                Ok(GeneratorLoad::Attached) => info!("generation model attached"),
                Ok(GeneratorLoad::NotConfigured) => {
                    info!("generation enabled but no model selected")
                }
                Ok(GeneratorLoad::Superseded) => {
                    info!("generation model load superseded by a newer settings change")
                }
                Err(e) => error!("Could not attach the generation model: {e:#}"),
            }
        });
    }

    pub async fn update_settings(
        self: &Arc<Self>,
        patch: serde_json::Value,
    ) -> anyhow::Result<wilkes_core::types::Settings> {
        let (before, updated) = {
            let _lock = self.settings_lock.lock().await;
            let before = get_settings(&self.settings_path).await.unwrap_or_default();
            let updated = update_settings(&self.settings_path, patch).await?;
            (before, updated)
        };
        self.on_directory_setting_maybe_changed(&before, &updated);
        self.on_zotero_settings_maybe_changed(&before, &updated);
        self.on_semantic_scholar_settings_maybe_changed(&before, &updated);
        self.on_openalex_settings_maybe_changed(&before, &updated);
        self.on_semantic_pref_maybe_changed(&before, &updated);
        self.on_generation_settings_maybe_changed(&before, &updated);
        Ok(updated)
    }

    /// React to the user toggling semantic search on or off. `search_prefer_semantic`
    /// is the single owner of whether the semantic subsystem is active: turning it
    /// off tears down the watcher so file changes stop triggering reindexes; turning
    /// it on reloads the index and watcher from the on-disk DB (no rebuild).
    fn on_semantic_pref_maybe_changed(self: &Arc<Self>, before: &Settings, after: &Settings) {
        if before.search_prefer_semantic == after.search_prefer_semantic {
            return;
        }
        if after.search_prefer_semantic {
            // Reload can install/probe the embedder, so do it off the settings
            // write path; the caller's build flow covers the not-yet-built case.
            let ctx = Arc::clone(self);
            tokio::spawn(async move {
                ctx.activate_semantic_from_disk().await;
            });
        } else {
            self.deactivate_semantic();
        }
    }

    /// Stop maintaining the semantic index: halt the watcher and release the
    /// resident embedder + index so filesystem changes no longer reindex. The
    /// on-disk DB is preserved so re-enabling is cheap.
    fn deactivate_semantic(&self) {
        self.invalidate_topic_tree_cache();
        *self.index.lock() = Arc::new(Mutex::new(None));
        *self.embedder.lock() = None;
    }

    /// Reload the embedder, index, and watcher from the on-disk DB when a usable
    /// one exists. No-op if semantic is already live or nothing is built yet.
    async fn activate_semantic_from_disk(self: &Arc<Self>) {
        if self.is_semantic_ready() {
            return;
        }
        let settings = self.get_settings().await;
        if let Some(loaded) = self.load_restore_state(settings).await {
            self.finish_restore_state(&loaded.plan, loaded.embedder, loaded.index)
                .await;
        }
    }

    /// React to a change in Zotero configuration by keeping the metadata cache
    /// coherent with the current integration state: drop now-stale Zotero rows,
    /// and, if Zotero is (still/now) enabled, re-resolve file-based rows into
    /// authoritative Zotero rows in the background.
    fn on_zotero_settings_maybe_changed(&self, before: &Settings, after: &Settings) {
        let z_before = &before.integrations.zotero;
        let z_after = &after.integrations.zotero;
        let relevant_changed =
            z_before.enabled != z_after.enabled || z_before.base_url != z_after.base_url;
        if !relevant_changed {
            return;
        }

        if let Some(cache) = self.metadata_cache() {
            if let Ok(guard) = cache.lock() {
                if let Err(e) = guard.invalidate_zotero() {
                    error!("metadata cache invalidate_zotero: {e:#}");
                }
            }
        }

        if z_after.enabled {
            self.spawn_zotero_backfill(after.clone());
        }
    }

    fn on_semantic_scholar_settings_maybe_changed(&self, before: &Settings, after: &Settings) {
        let s_before = &before.integrations.semantic_scholar;
        let s_after = &after.integrations.semantic_scholar;
        let relevant_changed = s_before.enabled != s_after.enabled
            || s_before.base_url != s_after.base_url
            || s_before.api_key != s_after.api_key;
        if !relevant_changed {
            return;
        }

        if let Some(cache) = self.metadata_cache() {
            if let Ok(guard) = cache.lock() {
                if let Err(e) = guard.invalidate_semantic_scholar() {
                    error!("metadata cache invalidate_semantic_scholar: {e:#}");
                }
            }
        }

        if s_after.enabled {
            self.spawn_semantic_scholar_backfill(after.clone());
        }
    }

    fn spawn_semantic_scholar_backfill(&self, settings: Settings) {
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let paths = match cache.lock() {
            Ok(guard) => guard.all_paths().unwrap_or_default(),
            Err(_) => return,
        };
        if !paths.is_empty() {
            self.spawn_metadata_fill(paths, settings, cache);
        }
    }

    fn on_openalex_settings_maybe_changed(&self, before: &Settings, after: &Settings) {
        let o_before = &before.integrations.openalex;
        let o_after = &after.integrations.openalex;
        let relevant_changed = o_before.enabled != o_after.enabled
            || o_before.base_url != o_after.base_url
            || o_before.email != o_after.email;
        if !relevant_changed {
            return;
        }

        if let Some(cache) = self.metadata_cache() {
            if let Ok(guard) = cache.lock() {
                if let Err(e) = guard.invalidate_openalex() {
                    error!("metadata cache invalidate_openalex: {e:#}");
                }
            }
        }

        if o_after.enabled {
            self.spawn_openalex_backfill(after.clone());
        }
    }

    fn spawn_openalex_backfill(&self, settings: Settings) {
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let paths = match cache.lock() {
            Ok(guard) => guard.all_paths().unwrap_or_default(),
            Err(_) => return,
        };
        if !paths.is_empty() {
            self.spawn_metadata_fill(paths, settings, cache);
        }
    }

    /// Re-resolve every file-sourced cache row against Zotero and upgrade the
    /// matches to authoritative Zotero rows. Runs after Zotero becomes usable so
    /// already-tabulated files gain library data without re-extraction.
    fn spawn_zotero_backfill(&self, settings: Settings) {
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            let rows = match cache.lock() {
                Ok(guard) => guard
                    .list_by_source(MetadataSource::File)
                    .unwrap_or_default(),
                Err(_) => return,
            };
            if rows.is_empty() {
                return;
            }
            let client = ZoteroClient::from_settings(&settings.integrations.zotero);
            let attachments = match client.attachment_items().await {
                Ok(a) => a,
                Err(e) => {
                    info!("zotero backfill: attachment fetch failed: {e:#}");
                    return;
                }
            };
            let mut updates: Vec<serde_json::Value> = Vec::new();
            let primary_source = metadata_source_preference(&settings.primary_metadata_source);
            for row in rows {
                if let Some(z) =
                    zotero_override_for(&client, &row.path, &row.metadata, &attachments).await
                {
                    if let Ok(guard) = cache.lock() {
                        if let Err(e) =
                            guard.upsert(&row.path, row.identity, &z, MetadataSource::Zotero)
                        {
                            error!("zotero backfill upsert {}: {e:#}", row.path.display());
                        }
                    }
                    updates.push(metadata_update_json_from_cache(
                        &cache,
                        &row.path,
                        row.identity,
                        primary_source,
                        &z,
                    ));
                }
            }
            if !updates.is_empty() {
                events.emit("file-metadata-updated", serde_json::json!(updates));
            }
        });
    }

    // ── Generation ────────────────────────────────────────────────────────────

    pub async fn generation_settings(&self) -> GenerationSettings {
        self.get_settings().await.generation
    }

    /// The single readiness predicate every LLM-dependent affordance is gated
    /// on. Not `settings.enabled` scattered across call sites — that is how a
    /// feature ends up half-gated, with a spinner spinning forever because the
    /// model was never installed.
    pub async fn is_generation_ready(&self) -> bool {
        let settings = self.generation_settings().await;
        settings.enabled && settings.model.is_some() && self.generator.lock().is_some()
    }

    /// Start the authoritative grounded-completion pipeline. Results are
    /// emitted on `completion://{id}` so callers can subscribe before starting
    /// and discard stale ids without a global event race.
    pub async fn request_completion(
        self: Arc<Self>,
        completion_id: String,
        request: CompletionRequest,
    ) -> Result<(), String> {
        if !self.is_generation_ready().await {
            return Err("Generation is not available".to_string());
        }
        if !self.is_semantic_ready() {
            return Err("Semantic index is not available".to_string());
        }
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "Embedding model is not available".to_string())?;
        let index = self.index.lock().clone();
        let generator = self
            .generator
            .lock()
            .clone()
            .ok_or_else(|| "Generation model is not available".to_string())?;
        let (roots, _) = library_roots(&self.get_settings().await);

        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.completion_operations.lock().insert(
            completion_id.clone(),
            CompletionOperation {
                cancel: Arc::clone(&cancel),
            },
        ) {
            previous.cancel.store(true, Ordering::Relaxed);
        }
        let ctx = Arc::clone(&self);
        tokio::task::spawn_blocking(move || {
            let dependencies = CompletionDependencies {
                embedder,
                index,
                generator,
                library_roots: roots,
            };
            let event_name = format!("completion://{completion_id}");
            let result = {
                let mut session = ctx.completion_session.lock();
                run_completion(
                    &completion_id,
                    &request,
                    &dependencies,
                    &mut session,
                    cancel.as_ref(),
                    &mut |event: CompletionEvent| {
                        ctx.events
                            .emit(&event_name, serde_json::to_value(event).unwrap_or_default());
                    },
                )
            };
            if let Err(error) = result {
                if !cancel.load(Ordering::Relaxed) {
                    error!(completion_id, "Grounded completion failed: {error:#}");
                    ctx.events.emit(
                        &event_name,
                        serde_json::to_value(CompletionEvent::Error {
                            message: format!("{error:#}"),
                        })
                        .unwrap_or_default(),
                    );
                }
            }
            let mut operations = ctx.completion_operations.lock();
            if operations
                .get(&completion_id)
                .is_some_and(|operation| Arc::ptr_eq(&operation.cancel, &cancel))
            {
                operations.remove(&completion_id);
            }
        });
        Ok(())
    }

    pub fn cancel_completion(&self, completion_id: &str) {
        if let Some(operation) = self.completion_operations.lock().remove(completion_id) {
            operation.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn cancel_all_completions(&self) {
        for (_, operation) in self.completion_operations.lock().drain() {
            operation.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub async fn completion_feedback(
        self: &Arc<Self>,
        completion_id: &str,
        feedback: CompletionFeedback,
    ) -> Result<(), String> {
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "Embedding model is not available".to_string())?;
        let ctx = Arc::clone(self);
        let completion_id = completion_id.to_string();
        let plan = tokio::task::spawn_blocking(move || {
            ctx.completion_session
                .lock()
                .prepare_feedback(&completion_id, feedback)
        })
        .await
        .map_err(|error| format!("Feedback preparation task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (plan, vectors) = tokio::task::spawn_blocking(move || {
            let vectors = plan.embed(embedder.as_ref())?;
            anyhow::Ok((plan, vectors))
        })
        .await
        .map_err(|error| format!("Feedback embedding task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let ctx = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            ctx.completion_session.lock().apply_feedback(plan, vectors)
        })
        .await
        .map_err(|error| format!("Feedback application task failed: {error}"))?
        .map_err(|error| error.to_string())
    }

    pub fn get_session_steering(&self) -> SessionSteering {
        self.completion_session.lock().steering()
    }

    pub fn reset_session_steering(&self) {
        self.completion_session.lock().reset();
    }

    /// Persist an editor buffer. The target must already be a non-PDF document
    /// inside the configured library; saving never becomes a path-creation or
    /// arbitrary-filesystem API.
    pub async fn save_document(&self, path: PathBuf, text: String) -> Result<(), String> {
        let settings = self.get_settings().await;
        let (roots, _) = library_roots(&settings);
        let path = std::fs::canonicalize(&path)
            .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
        Self::ensure_path_in_library(&path, &roots, "Editor document")?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("pdf") {
            return Err("PDF documents are not editable".to_string());
        }
        if !settings.supported_extensions.iter().any(|supported| {
            supported
                .trim_start_matches('.')
                .eq_ignore_ascii_case(extension)
        }) {
            return Err("Document type is not editable".to_string());
        }
        tokio::fs::write(&path, text)
            .await
            .map_err(|error| format!("Failed to save {}: {error}", path.display()))
    }

    pub async fn list_generation_models(&self) -> anyhow::Result<Vec<GeneratorDescriptor>> {
        let settings = self.generation_settings().await;
        generate_dispatch::list_models(settings.engine, &self.data_dir, &settings.ollama_url).await
    }

    pub async fn fetch_generation_model_size(&self, model_id: &str) -> anyhow::Result<u64> {
        let settings = self.generation_settings().await;
        let engine = settings.engine;
        let model_id = model_id.to_string();
        tokio::task::spawn_blocking(move || generate_dispatch::fetch_model_size(engine, &model_id))
            .await?
    }

    fn generation_device(settings: &GenerationSettings) -> String {
        settings
            .device
            .clone()
            .unwrap_or_else(|| "auto".to_string())
    }

    /// Download (if needed) and attach the configured generation model.
    ///
    /// Loads are serialised and epoch-stamped: every settings change spawns one,
    /// and a slow load for a model the user has since switched away from must not
    /// win the race and attach itself. The epoch is claimed before queueing, so
    /// the newest claim is the only one allowed to assign the generator.
    pub async fn load_generator(self: &Arc<Self>) -> anyhow::Result<GeneratorLoad> {
        let epoch = self.generator_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let _serialized = self.generator_load_lock.lock().await;
        if self.generator_epoch.load(Ordering::SeqCst) != epoch {
            info!("generation model load superseded before it started");
            return Ok(GeneratorLoad::Superseded);
        }

        // Read after taking the lock: the settings that matter are the current
        // ones, not the ones in force when this load was queued.
        let settings = self.generation_settings().await;
        if !settings.enabled {
            *self.generator.lock() = None;
            return Ok(GeneratorLoad::NotConfigured);
        }
        let Some(model) = settings.model.clone() else {
            *self.generator.lock() = None;
            return Ok(GeneratorLoad::NotConfigured);
        };

        // Generation download progress travels on its own event stream. The
        // `embed-*` events are owned by the semantic index lifecycle: borrowing
        // them here put the UI into "indexing" with no terminal event to leave it.
        let (progress_tx, progress_rx) = mpsc::channel::<EmbedProgress>(64);
        let forward = tokio::spawn(Self::forward_generation_progress(
            Arc::clone(&self.events),
            progress_rx,
        ));
        let attach = generate_dispatch::attach_generator(
            settings.engine,
            model.clone(),
            self.generate_manager.clone(),
            Self::generation_device(&settings),
            &self.data_dir,
            &settings.ollama_url,
            settings.context_tokens,
            progress_tx,
        )
        .await;
        let _ = forward.await;
        let generator = match attach {
            Ok(generator) => generator,
            Err(e) => {
                self.emit_generation_error(format!("{e:#}"));
                return Err(e);
            }
        };

        // Last check before the assignment: a newer load claimed the epoch while
        // this one was downloading, and its model is the one the user wants.
        if self.generator_epoch.load(Ordering::SeqCst) != epoch {
            info!(
                "generation model '{}' finished loading but was superseded; discarding it",
                model.model_id()
            );
            self.events.emit(
                "generation-done",
                serde_json::json!({ "model": model.model_id() }),
            );
            return Ok(GeneratorLoad::Superseded);
        }

        *self.generator.lock() = Some(generator);

        // Generation and embedding share the worker lifecycle default. Do not
        // read a second persisted timeout here: older builds wrote 60 seconds
        // into generation settings, causing a reload every minute even though
        // the common worker residency is five minutes.
        if settings.engine == GenerationEngine::Candle {
            let _ = self
                .generate_manager
                .send(ManagerCommand::SetTimeout(
                    wilkes_core::worker::DEFAULT_IDLE_TIMEOUT_SECS,
                ))
                .await;
        }
        self.events.emit(
            "generation-done",
            serde_json::json!({ "model": model.model_id() }),
        );
        Ok(GeneratorLoad::Attached)
    }

    fn emit_generation_error(&self, message: impl Into<String>) {
        let message = message.into();
        error!("Generation model load failed: {message}");
        self.events.emit(
            "generation-error",
            serde_json::json!({ "message": message }),
        );
    }

    async fn forward_generation_progress(
        events: Arc<dyn EventEmitter>,
        mut progress_rx: mpsc::Receiver<EmbedProgress>,
    ) {
        while let Some(progress) = progress_rx.recv().await {
            events.emit(
                "generation-progress",
                serde_json::to_value(&progress).unwrap_or_default(),
            );
        }
    }

    /// Detach the generator and reap its process. Called when the feature is
    /// switched off or the model changes.
    pub fn unload_generator(&self) {
        // Supersede an install/probe already in flight. Merely clearing the Arc
        // would let that older load attach itself again after disable returned.
        self.generator_epoch.fetch_add(1, Ordering::SeqCst);
        self.cancel_all_completions();
        *self.generator.lock() = None;
        self.kill_generation_worker();
    }

    pub fn is_semantic_ready(&self) -> bool {
        self.embedder.lock().is_some()
            && self
                .index
                .lock()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some()
    }

    // ── Search ────────────────────────────────────────────────────────────────

    fn canonicalize_search_root(root: &Path) -> Result<PathBuf, String> {
        std::fs::canonicalize(root).map_err(|err| {
            format!(
                "Search root does not exist or cannot be accessed: {} ({err})",
                root.display()
            )
        })
    }

    fn canonicalize_supported_file(
        root: &Path,
        path: &Path,
        supported_extensions: &[String],
        label: &str,
    ) -> Result<(PathBuf, FileType), String> {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let file = std::fs::canonicalize(&requested).map_err(|err| {
            format!(
                "{label} file does not exist or cannot be accessed: {} ({err})",
                requested.display()
            )
        })?;
        if !file.is_file() {
            return Err(format!("{label} file is not a file: {}", file.display()));
        }
        let file_type = FileType::detect(&file, supported_extensions)
            .ok_or_else(|| format!("{label} file type is not supported: {}", file.display()))?;
        Ok((file, file_type))
    }

    fn prepare_search_query(
        mut query: SearchQuery,
        supported_extensions: Vec<String>,
        library_roots: &[PathBuf],
    ) -> Result<SearchQuery, String> {
        query.supported_extensions = supported_extensions;
        if query.scope != SearchScope::All {
            query.root = Self::canonicalize_search_root(&query.root)?;
            Self::ensure_path_in_library(&query.root, library_roots, "Search root")?;
        }

        if let SearchScope::File { path } = &query.scope {
            let (file, _) = Self::canonicalize_supported_file(
                &query.root,
                path,
                &query.supported_extensions,
                "Search",
            )?;
            Self::ensure_path_in_library(&file, library_roots, "Search file")?;
            query.scope = SearchScope::File { path: file };
        }

        Ok(query)
    }

    fn ensure_path_in_library(
        path: &Path,
        library_roots: &[PathBuf],
        label: &str,
    ) -> Result<(), String> {
        if library_roots.iter().any(|root| path.starts_with(root)) {
            return Ok(());
        }
        Err(format!("{label} is not in the library: {}", path.display()))
    }

    fn ensure_no_active_embed_task(&self, message: &str) -> Result<(), String> {
        if self.embed_cancel_in_progress.load(Ordering::Acquire) {
            return Err(message.to_string());
        }
        let mut guard = self.embed_task.lock();
        if let Some(task) = guard.as_ref() {
            if !task.join.is_finished() {
                return Err(message.to_string());
            }
            *guard = None;
        }
        Ok(())
    }

    async fn prepare_semantic_runtime(
        self: &Arc<Self>,
        root: &Path,
        settings: &Settings,
    ) -> Result<SemanticRuntime, String> {
        self.ensure_no_active_embed_task("Semantic index is currently being built. Please wait.")?;

        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
        let index_arc = self.index.lock().clone();
        let query_root_canonical =
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let root_ready = {
            let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_ref() {
                Some(idx) => {
                    let status = idx.status_for_root(Some(&query_root_canonical));
                    status.indexed_files > 0 && status.total_chunks > 0
                }
                None => false,
            }
        };

        if !root_ready {
            self.request_semantic_reindex_for_root(
                &index_arc,
                Arc::clone(&embedder),
                &query_root_canonical,
            );
            return Err(format!(
                "Semantic index is not ready for search root {}. Indexing has been requested; please try again when indexing finishes.",
                root.display()
            ));
        }

        Ok(SemanticRuntime {
            embedder,
            index: index_arc,
            indexing: IndexingConfig {
                chunk_size: settings.semantic.chunk_size,
                chunk_overlap: settings.semantic.chunk_overlap,
                supported_extensions: settings.supported_extensions.clone(),
            },
        })
    }

    async fn prepare_global_semantic_runtime(
        self: &Arc<Self>,
        settings: &Settings,
    ) -> Result<SemanticRuntime, String> {
        self.ensure_no_active_embed_task("Semantic index is currently being built. Please wait.")?;
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
        let index = self.index.lock().clone();
        let ready = index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|idx| {
                let status = idx.status();
                status.indexed_files > 0 && status.total_chunks > 0
            });
        if !ready {
            return Err("The global semantic index has no searchable documents.".to_string());
        }
        Ok(SemanticRuntime {
            embedder,
            index,
            indexing: IndexingConfig {
                chunk_size: settings.semantic.chunk_size,
                chunk_overlap: settings.semantic.chunk_overlap,
                supported_extensions: settings.supported_extensions.clone(),
            },
        })
    }

    fn request_semantic_reindex_for_root(
        self: &Arc<Self>,
        index_arc: &Arc<Mutex<Option<SemanticIndex>>>,
        embedder: Arc<dyn Embedder>,
        root: &Path,
    ) {
        let already_building = {
            let guard = self.embed_task.lock();
            guard.as_ref().is_some_and(|t| !t.join.is_finished())
                || self.embed_cancel_in_progress.load(Ordering::Acquire)
        };
        if already_building {
            info!("semantic read: root changed but reindex already in progress, skipping");
            return;
        }

        info!("semantic read: root changed, triggering background reindex");
        let engine = {
            let guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .as_ref()
                .map(|idx| idx.status().engine)
                .unwrap_or_default()
        };
        let selected = SelectedEmbedder {
            engine,
            model: EmbedderModel(embedder.model_id().to_string()),
            dimension: embedder.dimension(),
        };
        let ctx = Arc::clone(self);
        let root_str = root.to_string_lossy().to_string();
        tokio::spawn(async move {
            if let Err(e) = ctx.start_build_index(root_str, selected).await {
                error!("background reindex failed: {e}");
            }
        });
    }

    /// Resolve semantic state (if needed) and start the search. Handles both
    /// Grep and Semantic modes; callers do not branch on mode.
    pub async fn start_search(self: Arc<Self>, query: SearchQuery) -> Result<SearchHandle, String> {
        self.start_search_as(query, "app").await
    }

    pub async fn start_search_as(
        self: Arc<Self>,
        mut query: SearchQuery,
        initiated_by: &str,
    ) -> Result<SearchHandle, String> {
        let settings = self.settings().await;
        let (resolved_library_roots, library_root_errors) = library_roots(&settings);
        query = Self::prepare_search_query(
            query,
            settings.supported_extensions.clone(),
            &resolved_library_roots,
        )?;

        let eligibility_roots = match &query.scope {
            SearchScope::All => resolved_library_roots.clone(),
            SearchScope::File { path } => resolved_library_roots
                .iter()
                .filter(|root| path.starts_with(root))
                .max_by_key(|root| root.components().count())
                .cloned()
                .map(|root| vec![root])
                .unwrap_or_else(|| vec![query.root.clone()]),
            SearchScope::Corpus => vec![query.root.clone()],
        };
        let eligible_paths = if query.collection_id.is_some() || !query.tag_ids.is_empty() {
            Some(
                self.eligible_paths_for_filters(
                    query.collection_id.as_deref(),
                    &query.tag_ids,
                    &eligibility_roots,
                )
                .await?,
            )
        } else {
            None
        };

        let mut semantic_indexing = None;
        let (all_roots, all_root_errors) = if query.scope == SearchScope::All {
            (resolved_library_roots, library_root_errors)
        } else {
            (Vec::new(), Vec::new())
        };
        let (embedder, index) = if query.mode == SearchMode::Semantic {
            let runtime = if query.scope == SearchScope::All {
                self.prepare_global_semantic_runtime(&settings).await?
            } else {
                self.prepare_semantic_runtime(&query.root, &settings)
                    .await?
            };
            semantic_indexing = Some(runtime.indexing);
            (Some(runtime.embedder), Some(runtime.index))
        } else {
            (None, None)
        };

        let log = {
            let store = self.research_store().map_err(|e| e.to_string())?;
            let id = store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .start_search_log(&query, initiated_by)
                .map_err(|e| e.to_string())?;
            Some(SearchLogTracker::new(store, id))
        };

        // Query-vector enhancement is a semantic-only concern. HyDE additionally
        // needs the loaded generator; hand it over only when HyDE is on so the
        // provider does not hold a generator reference it will never use.
        let retrieval = settings.retrieval.clone();
        let generator = if query.mode == SearchMode::Semantic && retrieval.hyde.enabled {
            self.generator.lock().clone()
        } else {
            None
        };

        let primary_metadata_source = metadata_source_preference(&settings.primary_metadata_source);
        Ok(start_search(
            query,
            all_roots,
            all_root_errors,
            embedder,
            index,
            semantic_indexing,
            eligible_paths,
            log,
            retrieval,
            generator,
            settings.grep_use_index,
        )
        .with_metadata(self.metadata_cache(), primary_metadata_source))
    }

    async fn eligible_paths_for_collection(
        &self,
        collection_id: &str,
        roots: &[PathBuf],
    ) -> Result<std::collections::HashSet<PathBuf>, String> {
        self.eligible_paths_for_filters(Some(collection_id), &[], roots)
            .await
    }

    async fn eligible_paths_for_filters(
        &self,
        collection_id: Option<&str>,
        tag_ids: &[String],
        roots: &[PathBuf],
    ) -> Result<std::collections::HashSet<PathBuf>, String> {
        let mut all = std::collections::HashSet::new();
        for root in roots {
            let listed = self
                .list_files(root.clone())
                .await
                .map_err(|e| e.to_string())?;
            let mut eligible = listed
                .files
                .iter()
                .filter(|entry| {
                    tag_ids
                        .iter()
                        .all(|id| entry.tags.iter().any(|tag| tag.id == *id))
                })
                .map(|entry| entry.path.clone())
                .collect::<std::collections::HashSet<_>>();
            if let Some(collection_id) = collection_id {
                let store = self.research_store().map_err(|e| e.to_string())?;
                let collection_paths = store
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .eligible_paths(collection_id, root, &listed.files)
                    .map_err(|e| e.to_string())?;
                eligible.retain(|path| collection_paths.contains(path));
            }
            all.extend(eligible);
        }
        Ok(all)
    }

    pub async fn related_documents(
        self: Arc<Self>,
        mut query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String> {
        const DEFAULT_LIMIT: usize = 8;
        const MAX_LIMIT: usize = 25;

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        query.root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&query.root, &library_roots, "Related documents root")?;
        let (source_path, _) = Self::canonicalize_supported_file(
            &query.root,
            &query.path,
            &settings.supported_extensions,
            "Related documents",
        )?;
        Self::ensure_path_in_library(&source_path, &library_roots, "Related documents file")?;
        let runtime = self
            .prepare_semantic_runtime(&query.root, &settings)
            .await?;
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let supported_extensions = settings.supported_extensions.clone();

        let root = query.root.clone();
        let listing_roots = if query.scope == SearchScope::All {
            library_roots
        } else {
            vec![root.clone()]
        };
        let eligible_paths = match query.collection_id.as_deref() {
            Some(collection_id) => Some(
                self.eligible_paths_for_collection(collection_id, &listing_roots)
                    .await?,
            ),
            None => None,
        };
        let mut related = tokio::task::spawn_blocking(move || {
            let guard = runtime.index.lock().unwrap_or_else(|p| p.into_inner());
            let idx = guard
                .as_ref()
                .ok_or_else(|| "No semantic index found. Build the index first.".to_string())?;
            idx.related_documents_filtered(
                &query.root,
                &source_path,
                limit,
                &supported_extensions,
                query.scope == SearchScope::All,
                eligible_paths.as_ref(),
            )
            .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("related documents task panicked: {e}"))??;

        // Use the canonical file-list metadata pipeline so every document list
        // renders and sorts the same record shape.
        let mut entries = std::collections::HashMap::new();
        for listing_root in listing_roots {
            let listed = self
                .list_files(listing_root)
                .await
                .map_err(|e| e.to_string())?;
            entries.extend(
                listed
                    .files
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry)),
            );
        }
        for document in &mut related {
            if let Some(entry) = entries.get(&document.entry.path) {
                document.entry = entry.clone();
            }
        }
        Ok(related)
    }

    /// Citation neighbours of a document that are present in the library,
    /// resolved by DOI: the documents it references and the documents that
    /// cite it. Unlike `related_documents` this needs no semantic index — it
    /// reads the DOI-keyed citation graph from the metadata cache. Returns
    /// empty when the anchor has no known DOI or no cached edges.
    pub async fn citation_links(
        self: Arc<Self>,
        query: CitationLinksQuery,
    ) -> Result<CitationLinks, String> {
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Citation links root")?;
        let (source_path, _) = Self::canonicalize_supported_file(
            &root,
            &query.path,
            &settings.supported_extensions,
            "Citation links",
        )?;
        Self::ensure_path_in_library(&source_path, &library_roots, "Citation links file")?;

        let cache = self
            .metadata_cache()
            .ok_or_else(|| "Metadata cache is unavailable".to_string())?;
        let (paths, reference_dois) = {
            let guard = cache
                .lock()
                .map_err(|_| "Metadata cache lock failed".to_string())?;
            let Some(doi) = guard
                .doi_for_path(&source_path)
                .map_err(|e| e.to_string())?
            else {
                return Ok(CitationLinks::default());
            };
            (
                guard.citation_links(&doi).map_err(|e| e.to_string())?,
                guard.citation_references(&doi).map_err(|e| e.to_string())?,
            )
        };
        if paths.references.is_empty() && paths.cited_by.is_empty() && reference_dois.is_empty() {
            return Ok(CitationLinks::default());
        }

        let reference_text = if reference_dois.is_empty() {
            None
        } else {
            self.citation_reference_text(&source_path).await
        };
        let all_references = enrich_citation_references(reference_dois, reference_text.as_deref());

        // Resolve edge paths to the same metadata-enriched record shape as every
        // other document list. Citation edges span the whole library, so list
        // across all roots rather than a single scope.
        let mut entries = std::collections::HashMap::new();
        for listing_root in library_roots {
            let listed = self
                .list_files(listing_root)
                .await
                .map_err(|e| e.to_string())?;
            entries.extend(
                listed
                    .files
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry)),
            );
        }
        let resolve = |paths: Vec<PathBuf>| -> Vec<FileEntry> {
            paths
                .into_iter()
                .filter(|path| path != &source_path)
                .filter_map(|path| entries.get(&path).cloned())
                .collect()
        };
        Ok(CitationLinks {
            references: resolve(paths.references),
            cited_by: resolve(paths.cited_by),
            all_references,
        })
    }

    /// One lifecycle and event contract for every user-facing token stream.
    /// Task closures own input preparation, prompts, and verification.
    async fn run_generation_stream<F>(
        self: Arc<Self>,
        request_id: String,
        task: GenerationTask,
        generate: F,
    ) -> Result<(), String>
    where
        F: FnOnce(
                Arc<Self>,
                Arc<dyn Generator>,
                &mut dyn FnMut(&str) -> std::ops::ControlFlow<()>,
            ) -> anyhow::Result<Generated>
            + Send
            + 'static,
    {
        let result: Result<String, String> = async {
            if request_id.trim().is_empty() || request_id.len() > 128 {
                return Err("Invalid generation request id".to_string());
            }
            if !self.is_generation_ready().await {
                return Err("Generation is not available".to_string());
            }
            let generator = self
                .generator
                .lock()
                .clone()
                .ok_or_else(|| "Generation model unavailable".to_string())?;

            let events = Arc::clone(&self.events);
            let delta_request_id = request_id.clone();
            let ctx = Arc::clone(&self);
            let generated = tokio::task::spawn_blocking(move || {
                generate(ctx, generator, &mut |delta| {
                    events.emit(
                        "generation-stream",
                        serde_json::to_value(GenerationStreamEvent::Delta {
                            request_id: delta_request_id.clone(),
                            task,
                            delta: delta.to_string(),
                        })
                        .unwrap_or_default(),
                    );
                    std::ops::ControlFlow::Continue(())
                })
            })
            .await
            .map_err(|e| format!("Generation task panicked: {e}"))?
            .map_err(|e| format!("{e:#}"))?;
            Ok(generated.text)
        }
        .await;

        match result {
            Ok(text) => {
                self.events.emit(
                    "generation-stream",
                    serde_json::to_value(GenerationStreamEvent::Completed {
                        request_id,
                        task,
                        text,
                    })
                    .unwrap_or_default(),
                );
                Ok(())
            }
            Err(error) => {
                warn!("Generation stream {request_id} failed: {error}");
                self.events.emit(
                    "generation-stream",
                    serde_json::to_value(GenerationStreamEvent::Failed {
                        request_id,
                        task,
                        error: error.clone(),
                    })
                    .unwrap_or_default(),
                );
                Err(error)
            }
        }
    }

    /// Stream a one-sentence explanation of why `related_path` is related to
    /// `anchor_path`.
    pub async fn explain_related_document(
        self: Arc<Self>,
        request_id: String,
        anchor_path: PathBuf,
        related_path: PathBuf,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::RelationExplanation,
            move |ctx, generator, sink| {
                let anchor = ctx
                    .document_summary(&anchor_path)
                    .map_err(anyhow::Error::msg)?;
                let related = ctx
                    .document_summary(&related_path)
                    .map_err(anyhow::Error::msg)?;
                explain_relation(generator.as_ref(), &anchor, &related, sink)
            },
        )
        .await
    }

    /// Stream a concise summary of the document currently shown in the viewer.
    /// Extraction is on demand and independent of the semantic index.
    pub async fn summarize_document(
        self: Arc<Self>,
        request_id: String,
        path: PathBuf,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::DocumentSummary,
            move |ctx, generator, sink| {
                let input = ctx
                    .document_summary_input(&path)
                    .map_err(anyhow::Error::msg)?;
                generate_document_summary(generator.as_ref(), &input, sink)
            },
        )
        .await
    }

    /// Synthesize a cited answer from cleaned, rank-preserving search passages.
    pub async fn summarize_search_results(
        self: Arc<Self>,
        request_id: String,
        input: SearchResultsSummaryInput,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::SearchResultsSummary,
            move |_ctx, generator, sink| {
                generate_search_results_summary(generator.as_ref(), &input, sink)
            },
        )
        .await
    }

    /// Title plus a leading excerpt, both from caches. Returns `Err` when there
    /// is no cached text: no fallback extraction.
    fn document_summary(&self, path: &Path) -> Result<DocumentSummary, String> {
        let index = self.index.lock();
        let guard = index.lock().unwrap_or_else(|p| p.into_inner());
        let idx = guard
            .as_ref()
            .ok_or_else(|| "No semantic index found".to_string())?;
        let excerpt = idx
            .cached_document_excerpt(path, MAX_EXCERPT_CHARS)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("No cached text for {}", path.display()))?;

        Ok(DocumentSummary {
            title: self.document_title(path),
            excerpt,
        })
    }

    fn document_summary_input(&self, path: &Path) -> Result<DocumentSummaryInput, String> {
        Ok(DocumentSummaryInput {
            title: self.document_title(path),
            text: Self::read_document_text(path)?,
        })
    }

    fn read_document_text(path: &Path) -> Result<String, String> {
        let text = if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        {
            PdfExtractor::new()
                .extract(path)
                .map_err(|e| format!("Could not extract {}: {e:#}", path.display()))?
                .text
        } else {
            std::fs::read_to_string(path)
                .map_err(|e| format!("Could not read {}: {e}", path.display()))?
        };
        Ok(text)
    }

    /// Prefer the semantic index's current extracted text. If this file has no
    /// usable stored full text (including legacy empty rows), extract it once
    /// off the async runtime so citation labels still work without an index
    /// rebuild.
    async fn citation_reference_text(&self, path: &Path) -> Option<String> {
        let indexed = {
            let index = self.index.lock();
            let guard = index.lock().unwrap_or_else(|p| p.into_inner());
            guard.as_ref().and_then(|idx| {
                idx.indexed_document_for_path(path)
                    .map_err(|error| {
                        info!(path = %path.display(), %error, "citation text index read failed");
                        error
                    })
                    .ok()
                    .flatten()
                    .map(|(text, _)| text)
                    .filter(|text| !text.trim().is_empty())
            })
        };
        if indexed.is_some() {
            return indexed;
        }

        let path = path.to_path_buf();
        match tokio::task::spawn_blocking(move || Self::read_document_text(&path)).await {
            Ok(Ok(text)) if !text.trim().is_empty() => Some(text),
            Ok(Ok(_)) => None,
            Ok(Err(error)) => {
                info!(%error, "citation text extraction skipped");
                None
            }
            Err(error) => {
                info!(%error, "citation text extraction task failed");
                None
            }
        }
    }

    fn document_title(&self, path: &Path) -> String {
        FileIdentity::for_path(path)
            .zip(self.metadata_cache())
            .and_then(|(identity, cache)| {
                let cache = cache.lock().unwrap_or_else(|p| p.into_inner());
                cache.get_valid(path, identity).ok().flatten()
            })
            .and_then(|metadata| metadata.title.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    }

    // ── Build index ───────────────────────────────────────────────────────────

    /// Start an index build in the background. Progress, completion, and errors
    /// are emitted through the `EventEmitter` as `embed-progress`, `embed-done`,
    /// and `embed-error` events. Returns as soon as the task is spawned.
    pub async fn start_build_index(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        info!(
            "AppContext::start_build_index: root={}, engine={}, model={}",
            root,
            selected.engine.as_str(),
            selected.model.model_id()
        );
        let plan = match self.prepare_build_index(&root, &selected).await {
            Ok(plan) => plan,
            Err(err) => {
                info!("AppContext::start_build_index: prepare failed: {err}");
                self.emit_embed_error("Build", err.clone());
                return Err(err);
            }
        };

        self.events
            .emit("manager-event", serde_json::json!("Reindexing"));

        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let join = Arc::clone(&self).spawn_build_index_task(
            plan,
            selected,
            cancel.clone(),
            Arc::clone(&cancel_flag),
        );

        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag,
            join,
        });
        info!("AppContext::start_build_index: build task registered");
        Ok(())
    }

    // ── Download model ────────────────────────────────────────────────────────

    /// Download a model in the background and load it into state on success.
    pub async fn start_download_model(
        self: Arc<Self>,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        info!(
            "AppContext::start_download_model: engine={}, model={}",
            selected.engine.as_str(),
            selected.model.model_id()
        );
        let plan = match self.prepare_download_model(&selected).await {
            Ok(plan) => plan,
            Err(err) => {
                info!("AppContext::start_download_model: prepare failed: {err}");
                self.emit_embed_error("Download", err.clone());
                return Err(err);
            }
        };
        let join = Arc::clone(&self).spawn_download_model_task(plan, selected);

        let cancel = CancellationToken::new();
        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Download,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });
        info!("AppContext::start_download_model: download task registered");
        Ok(())
    }

    async fn prepare_build_index(
        &self,
        root: &str,
        selected: &SelectedEmbedder,
    ) -> Result<BuildIndexPlan, String> {
        if self.embed_task_is_running() {
            return Err("A build is already in progress.".into());
        }

        let root_path = PathBuf::from(root);
        let settings = self.settings().await;
        self.validate_index_root(
            &root_path,
            &settings,
            "Choose a directory before building an index.",
        )
        .await?;

        Ok(BuildIndexPlan {
            root_path,
            device: settings.semantic.device_for(selected.engine).to_string(),
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
        })
    }

    async fn prepare_download_model(
        &self,
        selected: &SelectedEmbedder,
    ) -> Result<DownloadModelPlan, String> {
        if self.embed_task_is_running() {
            return Err("A build is already in progress.".into());
        }

        let settings = self.settings().await;
        let Some(root_path) = settings.last_directory.clone() else {
            return Err(
                "Choose a directory before downloading a model and building an index.".to_string(),
            );
        };
        self.validate_index_root(
            &root_path,
            &settings,
            "Choose a directory before downloading a model and building an index.",
        )
        .await?;

        Ok(DownloadModelPlan {
            device: settings.semantic.device_for(selected.engine).to_string(),
        })
    }

    fn build_index_options(
        manager: WorkerManager,
        data_dir: PathBuf,
        plan: &BuildIndexPlan,
        progress_tx: tokio::sync::mpsc::Sender<EmbedProgress>,
        cancel_flag: Arc<AtomicBool>,
    ) -> crate::commands::embed::BuildIndexOptions {
        crate::commands::embed::BuildIndexOptions {
            manager: Some(manager),
            device: Some(plan.device.clone()),
            data_dir,
            tx: progress_tx,
            cancel_flag,
            chunk_size: plan.chunk_size,
            chunk_overlap: plan.chunk_overlap,
            supported_extensions: plan.supported_extensions.clone(),
        }
    }

    fn cleanup_partial_index_files(data_dir: &Path) {
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
    }

    fn emit_progress_event(&self, progress: &EmbedProgress) {
        self.events.emit(
            "embed-progress",
            serde_json::to_value(progress).unwrap_or_default(),
        );
    }

    async fn open_built_index(
        &self,
        data_dir: PathBuf,
        model_id: String,
        dim: usize,
    ) -> Result<SemanticIndex, String> {
        match tokio::task::spawn_blocking(move || SemanticIndex::open(&data_dir, &model_id, dim))
            .await
        {
            Ok(Ok(index)) => Ok(index),
            Ok(Err(err)) => Err(err.to_string()),
            Err(err) => Err(err.to_string()),
        }
    }

    fn start_build_watcher(
        self: &Arc<Self>,
        root_path: PathBuf,
        _index_arc: Arc<Mutex<Option<SemanticIndex>>>,
        _embedder: Arc<dyn Embedder>,
        _indexing: IndexingConfig,
    ) {
        self.start_directory_watcher(root_path);
    }

    async fn finish_build_index(
        self: &Arc<Self>,
        plan: &BuildIndexPlan,
        selected: &SelectedEmbedder,
        data_dir: &Path,
        embedder: Arc<dyn Embedder>,
    ) -> Result<(), String> {
        let dim = embedder.dimension();
        let model_id = selected.model.model_id().to_string();
        let mut index = self
            .open_built_index(data_dir.to_path_buf(), model_id, dim)
            .await?;
        index
            .activate_root(&plan.root_path)
            .map_err(|e| e.to_string())?;
        let actual_dim = index.status().dimension;
        let index_arc = Arc::new(Mutex::new(Some(index)));

        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = Some(Arc::clone(&embedder));
        *self.index.lock() = Arc::clone(&index_arc);

        self.start_build_watcher(
            plan.root_path.clone(),
            index_arc,
            embedder,
            IndexingConfig {
                chunk_size: plan.chunk_size,
                chunk_overlap: plan.chunk_overlap,
                supported_extensions: plan.supported_extensions.clone(),
            },
        );

        self.update_semantic_settings(|s| SemanticSettings {
            index_path: Some(data_dir.join("semantic_index.db")),
            selected: SelectedEmbedder {
                dimension: actual_dim,
                ..selected.clone()
            },
            enabled: true,
            ..s
        })
        .await;

        self.events
            .emit("embed-done", serde_json::json!({ "operation": "Build" }));
        Ok(())
    }

    async fn forward_embed_progress(
        events: Arc<dyn EventEmitter>,
        mut progress_rx: mpsc::Receiver<EmbedProgress>,
    ) {
        while let Some(progress) = progress_rx.recv().await {
            events.emit(
                "embed-progress",
                serde_json::to_value(&progress).unwrap_or_default(),
            );
        }
    }

    fn spawn_build_index_task(
        self: Arc<Self>,
        plan: BuildIndexPlan,
        selected: SelectedEmbedder,
        cancel: CancellationToken,
        cancel_flag: Arc<AtomicBool>,
    ) -> JoinHandle<anyhow::Result<()>> {
        let manager = self.worker_manager.clone();
        let data_dir = self.data_dir.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<EmbedProgress>(128);
        let cancel_for_task = cancel.clone();

        tokio::spawn(async move {
            info!(
                "AppContext::spawn_build_index_task: entered for root={}",
                plan.root_path.display()
            );
            enum TerminalEvent {
                Done,
                Cancelled,
            }

            struct TerminalEventGuard {
                events: Arc<dyn EventEmitter>,
                terminal: Option<TerminalEvent>,
            }

            impl Drop for TerminalEventGuard {
                fn drop(&mut self) {
                    let Some(event) = self.terminal.take() else {
                        return;
                    };
                    let name = match event {
                        TerminalEvent::Done => "ReindexingDone",
                        TerminalEvent::Cancelled => "ReindexingCancelled",
                    };
                    self.events.emit("manager-event", serde_json::json!(name));
                }
            }

            let mut terminal_event = TerminalEventGuard {
                events: Arc::clone(&ctx.events),
                terminal: None,
            };

            let options = Self::build_index_options(
                manager.clone(),
                data_dir.clone(),
                &plan,
                progress_tx,
                Arc::clone(&cancel_flag),
            );
            let build_fut = crate::commands::embed::build_index(
                plan.root_path.clone(),
                selected.clone(),
                options,
            );
            tokio::pin!(build_fut);
            let mut progress_rx = progress_rx;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel_for_task.cancelled(), if !cancel_flag.load(Ordering::Relaxed) => {
                        info!("AppContext::spawn_build_index_task: cancellation observed");
                        cancel_flag.store(true, Ordering::Relaxed);
                        ctx.worker_manager.request_shutdown();
                    }

                    res = &mut build_fut => {
                        info!("AppContext::spawn_build_index_task: build future completed");
                        match res {
                            Ok(embedder) => {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    Self::cleanup_partial_index_files(&data_dir);
                                    terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                    ctx.emit_embed_error("Build", "");
                                } else if let Err(err) = ctx
                                    .finish_build_index(&plan, &selected, &data_dir, embedder)
                                    .await
                                {
                                    terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                    ctx.emit_embed_error("Build", err);
                                } else {
                                    terminal_event.terminal = Some(TerminalEvent::Done);
                                }
                            }
                            Err(e) => {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    Self::cleanup_partial_index_files(&data_dir);
                                    terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                    ctx.emit_embed_error("Build", "");
                                } else {
                                    terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                    ctx.emit_embed_error("Build", format!("{e:#}"));
                                }
                            }
                        }
                        break;
                    }

                    Some(p) = progress_rx.recv() => {
                        if !cancel_flag.load(Ordering::Relaxed) {
                            ctx.emit_progress_event(&p);
                        }
                    }
                }
            }
            ctx.clear_embed_task();
            Ok(())
        })
    }

    fn spawn_download_model_task(
        self: Arc<Self>,
        plan: DownloadModelPlan,
        selected: SelectedEmbedder,
    ) -> JoinHandle<anyhow::Result<()>> {
        let data_dir = self.data_dir.clone();
        let manager = self.worker_manager.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = mpsc::channel::<EmbedProgress>(64);

        tokio::spawn(async move {
            let forward = tokio::spawn(Self::forward_embed_progress(
                Arc::clone(&ctx.events),
                progress_rx,
            ));

            let result = crate::commands::embed::download_model(
                selected.clone(),
                manager.clone(),
                plan.device.clone(),
                data_dir.clone(),
                progress_tx,
            )
            .await;

            let _ = forward.await;

            match result {
                Ok(()) => {
                    if let Err(e) = ctx
                        .probe_and_load_downloaded_model(
                            selected.clone(),
                            manager,
                            plan.device.clone(),
                        )
                        .await
                    {
                        ctx.emit_embed_error("Download", e);
                    } else {
                        ctx.events
                            .emit("embed-done", serde_json::json!({ "operation": "Download" }));
                    }
                }
                Err(e) => {
                    ctx.emit_embed_error("Download", format!("{e:#}"));
                }
            }

            ctx.clear_embed_task();
            Ok(())
        })
    }

    async fn probe_and_load_downloaded_model(
        self: &Arc<Self>,
        selected: SelectedEmbedder,
        manager: WorkerManager,
        device: String,
    ) -> Result<(), String> {
        let installer =
            dispatch::get_installer(selected.engine, selected.model.clone(), manager, device);
        self.probe_and_load_downloaded_model_with(installer).await
    }

    async fn probe_and_load_downloaded_model_with(
        self: &Arc<Self>,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Result<(), String> {
        // Probe model dimensions by running install again (no-op if cached).
        let (probe_tx, _) = mpsc::channel(1);
        if let Err(e) = installer.install(&self.data_dir, probe_tx).await {
            return Err(format!("Failed to probe model dimensions: {e:#}"));
        }

        match installer.build(&self.data_dir) {
            Ok(embedder) => {
                *self.embedder.lock() = Some(embedder);
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    // ── Embed lifecycle ───────────────────────────────────────────────────────

    pub async fn cancel_embed(&self) {
        info!("AppContext::cancel_embed: requested");
        info!("AppContext::cancel_embed: requesting worker shutdown immediately");
        self.worker_manager.request_shutdown();
        let Some(task) = self.embed_task.lock().take() else {
            info!("AppContext::cancel_embed: no active task");
            return;
        };

        self.embed_cancel_in_progress.store(true, Ordering::Release);
        task.cancel_flag.store(true, Ordering::Relaxed);
        task.cancel.cancel();

        match task.operation {
            EmbedOperation::Build => {
                info!(
                    "AppContext::cancel_embed: worker shutdown requested; awaiting build task cleanup"
                );
                let _ = task.join.await;
            }
            EmbedOperation::Download => {
                info!("AppContext::cancel_embed: aborting download task");
                self.emit_embed_error(task.operation.as_str(), "");
                task.join.abort();
                let _ = task.join.await;
            }
        }

        self.clear_embed_task();
        info!("AppContext::cancel_embed: completed");
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop_directory_watcher();
        self.cancel_all_completions();
        self.cancel_embed().await;
        if let Some(task) = self.cluster_label_task.lock().take() {
            task.abort();
        }
        for (_, operation) in self.topic_operations.lock().drain() {
            operation.cancel_flag.store(true, Ordering::Relaxed);
            operation.cancel.cancel();
            if let Some(task) = operation.label_task {
                task.abort();
            }
        }
        self.kill_all_workers();
    }

    pub async fn delete_index(&self, root: Option<PathBuf>) -> anyhow::Result<()> {
        self.cancel_all_completions();
        self.invalidate_topic_tree_cache();
        if let Some(root) = root {
            crate::commands::embed::delete_index(&self.data_dir, Some(root.clone())).await?;
            let index_arc = self.index.lock().clone();
            if let Ok(mut guard) = index_arc.lock() {
                if let Some(idx) = guard.as_mut() {
                    let _ = idx.delete_root(&root);
                }
            };
        } else {
            *self.index.lock() = Arc::new(Mutex::new(None));
            *self.embedder.lock() = None;
            crate::commands::embed::delete_index(&self.data_dir, None).await?;
            self.update_semantic_settings(|s| SemanticSettings {
                index_path: None,
                ..s
            })
            .await;
        }
        Ok(())
    }

    pub async fn get_index_status(&self, root: Option<PathBuf>) -> anyhow::Result<IndexStatus> {
        crate::commands::embed::get_index_status(&self.data_dir, root).await
    }

    // ── Worker management ─────────────────────────────────────────────────────

    /// Status of every worker. Two processes can die independently, so a single
    /// status would misreport a dead generation worker as healthy.
    pub fn get_worker_statuses(&self) -> Vec<WorkerStatus> {
        vec![self.worker_manager.status(), self.generate_manager.status()]
    }

    /// The embedding worker's status. Kept for callers that only care about
    /// indexing; new callers should prefer `get_worker_statuses`.
    pub fn get_worker_status(&self) -> WorkerStatus {
        self.worker_manager.status()
    }

    pub fn kill_worker(&self) {
        self.worker_manager.request_shutdown();
    }

    pub fn kill_generation_worker(&self) {
        self.generate_manager.request_shutdown();
    }

    /// Shut down every worker. A missed one leaks a multi-gigabyte process past
    /// app exit.
    pub fn kill_all_workers(&self) {
        self.kill_worker();
        self.kill_generation_worker();
    }

    pub async fn set_worker_timeout(&self, secs: u64) -> anyhow::Result<()> {
        self.worker_manager
            .send(ManagerCommand::SetTimeout(secs))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    // ── Startup restore ───────────────────────────────────────────────────────

    fn restore_state_needs_reset(settings: &Settings, db_status: Option<&IndexStatus>) -> bool {
        match db_status {
            None => settings.semantic.enabled || settings.semantic.index_path.is_some(),
            Some(db_status) => {
                let selected = &settings.semantic.selected;
                db_status.engine != selected.engine
                    || db_status.model_id != selected.model.model_id()
            }
        }
    }

    fn restore_state_indexing_config(settings: &Settings) -> IndexingConfig {
        IndexingConfig {
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
        }
    }

    fn restore_state_enabled_settings(
        current: SemanticSettings,
        db_path: PathBuf,
        selected: SelectedEmbedder,
        dim: usize,
    ) -> SemanticSettings {
        SemanticSettings {
            enabled: true,
            index_path: Some(db_path),
            selected: SelectedEmbedder {
                dimension: dim,
                ..selected
            },
            ..current
        }
    }

    fn open_semantic_index_with<F>(
        data_dir: &PathBuf,
        model_id: &str,
        expected_dim: usize,
        open: F,
    ) -> anyhow::Result<SemanticIndex>
    where
        F: FnOnce(&PathBuf, &str, usize) -> anyhow::Result<SemanticIndex>,
    {
        open(data_dir, model_id, expected_dim)
    }

    fn prepare_restore_state_plan(
        settings: Settings,
        db_status: IndexStatus,
    ) -> RestoreStatePreparation {
        let selected = settings.semantic.selected.clone();
        if Self::restore_state_needs_reset(&settings, Some(&db_status)) {
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            }
        } else {
            RestoreStatePreparation::Ready(RestoreStatePlan {
                device: settings.semantic.device_for(selected.engine).to_string(),
                db_status,
                selected,
            })
        }
    }

    async fn clear_restore_state_settings(&self) {
        self.update_semantic_settings(|s| SemanticSettings {
            enabled: false,
            index_path: None,
            ..s
        })
        .await;
    }

    async fn load_restore_db_status(&self, settings: &Settings) -> Option<IndexStatus> {
        match tokio::task::spawn_blocking({
            let d = self.data_dir.clone();
            move || SemanticIndex::read_status_from_path(&d)
        })
        .await
        {
            Ok(Ok(status)) => Some(status),
            Ok(Err(err)) => {
                info!("restore_state: no index DB ({err:#}), nothing to restore");
                if Self::restore_state_needs_reset(settings, None) {
                    self.clear_restore_state_settings().await;
                }
                None
            }
            Err(err) => {
                error!("restore_state: spawn_blocking panicked: {err}");
                None
            }
        }
    }

    async fn load_restore_state(
        self: &Arc<Self>,
        settings: Settings,
    ) -> Option<RestoreLoadedState> {
        let db_status = self.load_restore_db_status(&settings).await?;
        let plan = match Self::prepare_restore_state_plan(settings, db_status) {
            RestoreStatePreparation::Ready(plan) => plan,
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                info!(
                    "restore_state: index selection '{:?}/{}' != settings selection '{:?}/{}', clearing stale index reference",
                    db_status.engine, db_status.model_id, selected.engine, selected.model.model_id()
                );
                self.clear_restore_state_settings().await;
                return None;
            }
        };

        let embedder = self
            .restore_embedder(&plan.selected, plan.device.clone())
            .await?;
        let index = self
            .restore_index(&plan.selected, embedder.dimension())
            .await?;

        Some(RestoreLoadedState {
            plan,
            embedder,
            index,
        })
    }

    async fn restore_embedder(
        &self,
        selected: &SelectedEmbedder,
        device: String,
    ) -> Option<Arc<dyn Embedder>> {
        let installer = dispatch::get_installer(
            selected.engine,
            selected.model.clone(),
            self.worker_manager.clone(),
            device,
        );
        self.restore_embedder_with(installer).await
    }

    async fn restore_embedder_with(
        &self,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Option<Arc<dyn Embedder>> {
        let (probe_tx, _) = tokio::sync::mpsc::channel(1);
        if let Err(err) = installer.install(&self.data_dir, probe_tx).await {
            error!("restore_state: install probe failed: {err:#}");
            return None;
        }
        if !installer.is_available(&self.data_dir) {
            info!("restore_state: model files absent, skipping");
            return None;
        }

        let data_dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || installer.build(&data_dir)).await {
            Ok(Ok(embedder)) => Some(embedder),
            Ok(Err(err)) => {
                error!("restore_state: build embedder: {err:#}");
                None
            }
            Err(err) => {
                error!("restore_state: build embedder panicked: {err}");
                None
            }
        }
    }

    async fn restore_index(
        &self,
        selected: &SelectedEmbedder,
        expected_dim: usize,
    ) -> Option<SemanticIndex> {
        let data_dir = self.data_dir.clone();
        let model_id = selected.model.model_id().to_string();
        match tokio::task::spawn_blocking(move || {
            Self::open_semantic_index_with(&data_dir, &model_id, expected_dim, |dir, model, dim| {
                SemanticIndex::open(dir, model, dim)
            })
        })
        .await
        {
            Ok(Ok(index)) => Some(index),
            Ok(Err(err)) => {
                error!("restore_state: open index: {err:#}");
                None
            }
            Err(err) => {
                error!("restore_state: open index panicked: {err}");
                None
            }
        }
    }

    fn restore_store_loaded_state(
        &self,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) -> Arc<Mutex<Option<SemanticIndex>>> {
        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = Some(Arc::clone(&embedder));
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);
        index_arc
    }

    async fn finish_restore_state(
        &self,
        plan: &RestoreStatePlan,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) {
        self.restore_store_loaded_state(Arc::clone(&embedder), index);

        let db_path = self.data_dir.join("semantic_index.db");
        let dim = plan.db_status.dimension;
        self.update_semantic_settings(|s| {
            Self::restore_state_enabled_settings(s, db_path.clone(), plan.selected.clone(), dim)
        })
        .await;

        info!("restore_state: embedder and index restored");
    }

    /// Reload the embedder and index from disk if they were previously built,
    /// and restart the filesystem watcher. Run this once after `new`.
    pub async fn restore_state(self: Arc<Self>) {
        let settings = match get_settings(&self.settings_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("restore_state: read settings: {e:#}");
                return;
            }
        };
        if let Some(root) = settings.last_directory.clone() {
            self.start_directory_watcher(root);
        }
        // Independent of the semantic toggle below: generation has its own
        // enable flag and must restore whether or not semantic search is on.
        if settings.generation.enabled && settings.generation.model.is_some() {
            let ctx = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = ctx.load_generator().await {
                    error!("restore_state: could not attach the generation model: {e:#}");
                }
            });
        }
        // `search_prefer_semantic` is the single owner of whether the semantic
        // subsystem is active. A leftover index DB on disk must not resurrect the
        // embedder behind a toggle the user turned off. The directory watcher is
        // independent and remains active for file-list invalidation.
        if !settings.search_prefer_semantic {
            info!("restore_state: semantic search disabled by preference, skipping restore");
            return;
        }
        let Some(loaded) = self.load_restore_state(settings).await else {
            return;
        };

        self.finish_restore_state(&loaded.plan, loaded.embedder, loaded.index)
            .await;
    }
}

/// Content-derived cluster identity.
///
/// Same members and same text yields the same key, even across a granularity
/// change that happens to reproduce the group; any edit, addition or removal
/// yields a different one. That is exactly the invalidation wanted, with no
/// invalidation code — and it is why the key uses each member's `input_hash`
/// rather than its `updated_at`, which changes on edits that do not affect the
/// clustered text.
fn cluster_key(members: &[(String, String)]) -> String {
    let mut parts: Vec<String> = members
        .iter()
        .map(|(id, input_hash)| format!("{id}:{input_hash}"))
        .collect();
    parts.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"v1\n");
    hasher.update(parts.join("\n").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Distribute a hard chunk budget across documents before Ward sees the input.
///
/// The round-robin quota makes every document contribute one chunk whenever
/// that is mathematically possible, then fills short documents completely
/// before repeatedly increasing larger ones. Within each document, evenly
/// spaced windows avoid turning the cap into a "document beginnings" sampler.
fn cap_topic_chunks(chunks: Vec<TopicChunkData>, cap: usize) -> Vec<TopicChunkData> {
    if cap == 0 {
        return Vec::new();
    }
    if chunks.len() <= cap {
        return chunks;
    }
    let mut by_file: std::collections::BTreeMap<i64, Vec<TopicChunkData>> =
        std::collections::BTreeMap::new();
    for chunk in chunks {
        by_file.entry(chunk.file_id).or_default().push(chunk);
    }
    let groups: Vec<Vec<TopicChunkData>> = by_file.into_values().collect();

    // More documents than slots is the only case where "every document"
    // cannot coexist with a hard ceiling. Choose documents evenly across the
    // stable file ordering rather than silently preferring the first paths.
    if groups.len() > cap {
        return (0..cap)
            .map(|slot| {
                let group_index = slot.saturating_mul(groups.len()) / cap;
                let group = &groups[group_index];
                group[group.len() / 2].clone()
            })
            .collect();
    }

    let mut quotas = vec![1usize; groups.len()];
    let mut assigned = groups.len();
    while assigned < cap {
        let mut progressed = false;
        for (quota, group) in quotas.iter_mut().zip(&groups) {
            if assigned == cap {
                break;
            }
            if *quota < group.len() {
                *quota += 1;
                assigned += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    let mut sampled = Vec::with_capacity(assigned);
    for (group, quota) in groups.into_iter().zip(quotas) {
        for slot in 0..quota {
            // Midpoints of `quota` equal-width ranges, characteristically
            // selecting first/middle/last coverage as the quota grows.
            let index = ((slot.saturating_mul(2) + 1).saturating_mul(group.len()))
                / quota.saturating_mul(2);
            sampled.push(group[index.min(group.len() - 1)].clone());
        }
    }
    sampled
}

fn topic_member(chunk: &TopicChunkData) -> ChunkTopicMember {
    ChunkTopicMember {
        chunk_id: chunk.chunk_id,
        file_path: chunk.file_path.clone(),
        chunk_text: chunk.chunk_text.clone(),
        extraction_byte_range: chunk.extraction_byte_range.clone(),
        origin: chunk.origin.clone(),
    }
}

fn mean_normalized_embeddings(
    embeddings: &[Vec<f32>],
    indices: &[usize],
) -> Result<Vec<f32>, String> {
    let Some(&first_index) = indices.first() else {
        return Err("Cannot calculate coverage for an empty topic".to_string());
    };
    let dimension = embeddings
        .get(first_index)
        .ok_or_else(|| "Topic member embedding index is out of bounds".to_string())?
        .len();
    if dimension == 0 {
        return Err("Cannot calculate coverage from zero-dimensional embeddings".to_string());
    }
    let mut mean = vec![0.0f32; dimension];
    for &index in indices {
        let embedding = embeddings
            .get(index)
            .ok_or_else(|| "Topic member embedding index is out of bounds".to_string())?;
        if embedding.len() != dimension {
            return Err("Topic member embedding dimensions do not match".to_string());
        }
        if !embedding.iter().all(|value| value.is_finite()) {
            return Err("Topic member embedding contains non-finite values".to_string());
        }
        let norm = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if norm > f32::EPSILON {
            for (total, value) in mean.iter_mut().zip(embedding) {
                *total += value / norm;
            }
        }
    }
    let count = indices.len() as f32;
    for value in &mut mean {
        *value /= count;
    }
    Ok(mean)
}

fn chunk_cluster_key(chunk_ids: impl IntoIterator<Item = i64>) -> String {
    let mut ids: Vec<i64> = chunk_ids.into_iter().collect();
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"chunk-cluster-v1\n");
    for id in ids {
        hasher.update(id.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn chunk_label_inputs(
    topic: &ChunkTopic,
    text_by_id: &std::collections::HashMap<i64, &str>,
) -> Vec<String> {
    let mut remaining: Vec<i64> = topic
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .filter(|id| *id != topic.representative_chunk_id)
        .collect();
    remaining.sort_unstable();
    std::iter::once(topic.representative_chunk_id)
        .chain(remaining)
        .filter_map(|id| text_by_id.get(&id).map(|text| (*text).to_string()))
        .take(wilkes_core::generate::tasks::cluster_label::MAX_MEMBERS)
        .collect()
}

/// Prompt input for one cluster: the representative first, then the remaining
/// members in a deterministic order. Non-deterministic selection would make the
/// cached label depend on iteration order.
fn label_inputs(
    cluster: &BookmarkCluster,
    inputs: &std::collections::HashMap<&str, &str>,
) -> Vec<String> {
    let mut ordered: Vec<&String> = cluster
        .bookmark_ids
        .iter()
        .filter(|id| **id != cluster.representative_bookmark_id)
        .collect();
    ordered.sort();

    std::iter::once(&cluster.representative_bookmark_id)
        .chain(ordered.into_iter())
        .filter_map(|id| inputs.get(id.as_str()).map(|text| text.to_string()))
        .take(wilkes_core::generate::tasks::cluster_label::MAX_MEMBERS)
        .collect()
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

fn bookmark_embedding_input(bookmark: &Bookmark) -> String {
    let quote = bookmark.quote.trim();
    let note = bookmark.note.as_deref().map(str::trim).unwrap_or_default();
    match (quote.is_empty(), note.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("Selected passage:\n{quote}"),
        (true, false) => format!("Research note:\n{note}"),
        (false, false) => {
            format!("Selected passage:\n{quote}\n\nResearch note:\n{note}")
        }
    }
}

/// Outcome of filling one file's metadata: either freshly extracted (and thus
/// eligible for a Zotero upgrade), or recovered by re-keying a renamed file's
/// existing row (already composed, so not re-resolved).
enum FillOutcome {
    Extracted(DocumentMetadata),
    Renamed(DocumentMetadata),
}

fn enrich_citation_references(
    reference_dois: Vec<String>,
    document_text: Option<&str>,
) -> Vec<CitationReference> {
    let wanted = reference_dois.iter().cloned().collect::<HashSet<_>>();
    let mut lines_by_doi = HashMap::new();
    if let Some(text) = document_text {
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            for found in find_dois(line) {
                if wanted.contains(&found) {
                    lines_by_doi
                        .entry(found)
                        .or_insert_with(|| line.to_string());
                }
            }
        }
    }

    reference_dois
        .into_iter()
        .map(|doi| CitationReference {
            citation_line: lines_by_doi.remove(&doi),
            doi,
        })
        .collect()
}

/// JSON payload entry for the `file-metadata-updated` event.
fn metadata_update_json(
    path: &Path,
    metadata: &DocumentMetadata,
    metadata_conflicts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "path": path.to_string_lossy(),
        "title": metadata.title,
        "author": metadata.author,
        "doi": metadata.doi,
        "publication_date": metadata.created_at,
        "citation_count": provider_citation_count(metadata),
        "metadata_conflicts": metadata_conflicts,
    })
}

fn provider_citation_count(metadata: &DocumentMetadata) -> Option<i64> {
    metadata
        .semantic_scholar
        .as_ref()
        .map(|paper| paper.citation_count)
        .or_else(|| metadata.openalex.as_ref().map(|work| work.citation_count))
}

fn metadata_update_json_from_cache(
    cache: &Arc<Mutex<MetadataCache>>,
    path: &Path,
    identity: FileIdentity,
    primary_source: MetadataSource,
    fallback: &DocumentMetadata,
) -> serde_json::Value {
    match cache
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .get_valid_with_primary(path, identity, primary_source)
                .ok()
        })
        .flatten()
    {
        Some(cached) => metadata_update_json(
            path,
            &cached.metadata,
            serde_json::to_value(cached.conflicts).unwrap_or_else(|_| serde_json::json!({})),
        ),
        None => metadata_update_json(path, fallback, serde_json::json!({})),
    }
}

/// Fill one file's file-based metadata. Prefers a cheap re-key when the same
/// content already exists in the cache under a stale (now-missing) path — a
/// rename. Otherwise extract fresh and upsert as a `File`-sourced row.
fn extract_or_rekey(
    cache: &Arc<Mutex<MetadataCache>>,
    registry: &wilkes_core::metadata::MetadataExtractorRegistry,
    path: &Path,
    identity: FileIdentity,
) -> FillOutcome {
    if let Ok(guard) = cache.lock() {
        if let Ok(Some(old_path)) = guard.find_rename_source(path, identity) {
            if let Err(e) = guard.rename(&old_path, path) {
                error!(
                    "metadata cache rename {} -> {}: {e:#}",
                    old_path.display(),
                    path.display()
                );
            } else if let Ok(Some(meta)) = guard.get_valid(path, identity) {
                return FillOutcome::Renamed(meta);
            }
        }
    }

    let metadata = registry.extract_for(path, None).unwrap_or_default();
    if let Ok(guard) = cache.lock() {
        if let Err(e) = guard.upsert(path, identity, &metadata, MetadataSource::File) {
            error!("metadata cache upsert {}: {e:#}", path.display());
        }
    }
    FillOutcome::Extracted(metadata)
}

/// Best-effort Zotero override for one file against a pre-fetched attachment
/// list. Returns `None` when the file does not resolve or the lookup errors.
async fn zotero_override_for(
    client: &ZoteroClient,
    path: &Path,
    file_based: &DocumentMetadata,
    attachments: &[ZoteroItem],
) -> Option<DocumentMetadata> {
    match crate::commands::integrations::zotero::resolve_override(
        client,
        path,
        file_based,
        attachments,
    )
    .await
    {
        Ok(opt) => opt,
        Err(e) => {
            info!("zotero override {}: {e:#}", path.display());
            None
        }
    }
}

async fn semantic_scholar_enrichment_for(
    client: &SemanticScholarClient,
    metadata: &DocumentMetadata,
) -> Option<wilkes_core::types::SemanticScholarPaper> {
    let doi = metadata.doi.as_deref()?;
    match client.lookup_by_doi(doi).await {
        Ok(paper) => Some(paper),
        Err(e) => {
            info!("semantic scholar lookup {doi}: {e:#}");
            None
        }
    }
}

async fn openalex_enrichment_for(
    client: &OpenAlexClient,
    metadata: &DocumentMetadata,
) -> Option<wilkes_core::types::OpenAlexWork> {
    let doi = metadata.doi.as_deref()?;
    match client.lookup_by_doi(doi).await {
        Ok(work) => Some(work),
        Err(e) => {
            info!("openalex lookup {doi}: {e:#}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tracing::subscriber;
    use tracing_subscriber::prelude::*;
    use wilkes_core::embed::MockEmbedder;
    use wilkes_core::generate::mock::MockGenerator;
    use wilkes_core::types::EmbeddingEngine;
    use wilkes_core::types::{
        BookmarkDock, ByteRange, EmbedderModel, IndexStatus, SearchMode, SelectedEmbedder,
        SemanticSettings, Settings, SourceOrigin, Theme,
    };

    fn topic_chunk(file_id: i64, chunk_id: i64) -> TopicChunkData {
        TopicChunkData {
            chunk_id,
            file_id,
            file_path: PathBuf::from(format!("/library/{file_id}.txt")),
            chunk_text: format!("chunk {chunk_id}"),
            extraction_byte_range: ByteRange {
                start: chunk_id as usize,
                end: chunk_id as usize + 1,
            },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
            embedding: vec![chunk_id as f32, 1.0],
        }
    }

    #[test]
    fn topic_cap_is_hard_and_distributed_per_document() {
        let chunks = vec![
            topic_chunk(1, 1),
            topic_chunk(2, 2),
            topic_chunk(2, 3),
            topic_chunk(2, 4),
            topic_chunk(3, 5),
            topic_chunk(3, 6),
            topic_chunk(3, 7),
            topic_chunk(3, 8),
            topic_chunk(3, 9),
            topic_chunk(3, 10),
        ];
        let sampled = cap_topic_chunks(chunks, 6);
        assert_eq!(sampled.len(), 6);
        let counts = sampled.iter().fold(
            std::collections::BTreeMap::<i64, usize>::new(),
            |mut counts, chunk| {
                *counts.entry(chunk.file_id).or_default() += 1;
                counts
            },
        );
        assert_eq!(counts, [(1, 1), (2, 3), (3, 2)].into_iter().collect());
        assert_eq!(
            sampled
                .iter()
                .filter(|chunk| chunk.file_id == 3)
                .map(|chunk| chunk.chunk_id)
                .collect::<Vec<_>>(),
            vec![6, 9],
            "within-document sampling should cover the passage range"
        );
    }

    #[test]
    fn topic_cap_samples_documents_evenly_when_documents_exceed_budget() {
        let chunks = (0..10)
            .map(|index| topic_chunk(index, index))
            .collect::<Vec<_>>();
        let sampled = cap_topic_chunks(chunks, 3);
        assert_eq!(sampled.len(), 3);
        assert_eq!(
            sampled
                .iter()
                .map(|chunk| chunk.file_id)
                .collect::<Vec<_>>(),
            vec![0, 3, 6]
        );
    }

    #[test]
    fn topic_cap_allows_the_complete_data_set() {
        let chunks = (0..10)
            .map(|index| topic_chunk(index, index))
            .collect::<Vec<_>>();
        let sampled = cap_topic_chunks(chunks, usize::MAX);
        assert_eq!(sampled.len(), 10);
    }

    #[tokio::test]
    async fn topic_granularity_recuts_one_cached_tree_and_cap_rebuilds_it() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("topic-root");
        std::fs::create_dir_all(&root).unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let mut index = SemanticIndex::create(
            dir.path(),
            "topic-cache-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        let vectors = [
            [1.0, 0.0],
            [0.99, 0.01],
            [0.0, 1.0],
            [0.01, 0.99],
            [-1.0, 0.0],
            [-0.99, 0.01],
        ];
        for (index_number, vector) in vectors.into_iter().enumerate() {
            let path = root.join(format!("{index_number}.txt"));
            let text = format!("topic passage {index_number}");
            std::fs::write(&path, &text).unwrap();
            index
                .write_file(wilkes_core::embed::index::db::PreparedFile {
                    full_text: String::new(),
                    path: path.clone(),
                    chunks: vec![(
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: path,
                            text: text.clone(),
                            byte_range: ByteRange {
                                start: 0,
                                end: text.len(),
                            },
                            origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        },
                        vector.to_vec(),
                    )],
                })
                .unwrap();
        }
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let fewer = Arc::clone(&ctx)
            .chunk_topics(
                "root-fewer".to_string(),
                ChunkTopicsQuery {
                    root: root.clone(),
                    path: None,
                    granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
                },
            )
            .await
            .unwrap();
        let first_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
        assert!(first_tree
            .sampled
            .iter()
            .all(|chunk| chunk.embedding.is_empty()));

        let more = Arc::clone(&ctx)
            .chunk_topics(
                "root-more".to_string(),
                ChunkTopicsQuery {
                    root: root.clone(),
                    path: None,
                    granularity: wilkes_core::types::BookmarkClusterGranularity::MuchMore,
                },
            )
            .await
            .unwrap();
        let recut_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
        assert!(Arc::ptr_eq(&first_tree, &recut_tree));
        assert_ne!(fewer.topics.len(), more.topics.len());

        ctx.update_semantic_settings(|settings| SemanticSettings {
            topic_cloud_input_cap: 3,
            ..settings
        })
        .await;
        let capped = Arc::clone(&ctx)
            .chunk_topics(
                "root-capped".to_string(),
                ChunkTopicsQuery {
                    root,
                    path: None,
                    granularity: wilkes_core::types::BookmarkClusterGranularity::Balanced,
                },
            )
            .await
            .unwrap();
        let rebuilt_tree = ctx.topic_tree_caches.lock().root.as_ref().unwrap().clone();
        assert!(!Arc::ptr_eq(&first_tree, &rebuilt_tree));
        assert_eq!(capped.sampled_chunk_count, 3);

        let revision = ctx.semantic_index_revision.load(Ordering::Acquire);
        ctx.deactivate_semantic();
        assert!(ctx.topic_tree_caches.lock().root.is_none());
        assert!(ctx.topic_tree_caches.lock().document.is_none());
        assert!(ctx.semantic_index_revision.load(Ordering::Acquire) > revision);
    }

    #[tokio::test]
    async fn document_topics_are_file_scoped_and_keep_the_root_tree_cached() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("document-topic-root");
        std::fs::create_dir_all(&root).unwrap();
        let document = root.join("paper.txt");
        let other = root.join("other.txt");
        std::fs::write(&document, "document passages").unwrap();
        std::fs::write(&other, "other passage").unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        let mut index = SemanticIndex::create(
            dir.path(),
            "document-topic-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        let vectors = [
            [1.0, 0.0],
            [0.99, 0.01],
            [0.0, 1.0],
            [0.01, 0.99],
            [-1.0, 0.0],
            [-0.99, 0.01],
        ];
        let chunks = vectors
            .into_iter()
            .enumerate()
            .map(|(number, vector)| {
                let text = format!("document topic passage {number}");
                (
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: document.clone(),
                        text: text.clone(),
                        byte_range: ByteRange {
                            start: 0,
                            end: text.len(),
                        },
                        origin: SourceOrigin::TextFile {
                            line: (number + 1) as u32,
                            col: 1,
                        },
                    },
                    vector.to_vec(),
                )
            })
            .collect();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: document.clone(),
                chunks,
            })
            .unwrap();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: other.clone(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: other,
                        text: "other passage".to_string(),
                        byte_range: ByteRange { start: 0, end: 13 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![0.5, 0.5],
                )],
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let root_result = Arc::clone(&ctx)
            .chunk_topics(
                "root-cloud".to_string(),
                ChunkTopicsQuery {
                    root: root.clone(),
                    path: None,
                    granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
                },
            )
            .await
            .unwrap();
        assert!(root_result
            .topics
            .iter()
            .all(|topic| topic.library_coverage.is_none()));
        let document_result = Arc::clone(&ctx)
            .chunk_topics(
                "document-cloud".to_string(),
                ChunkTopicsQuery {
                    root,
                    path: Some(document.clone()),
                    granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
                },
            )
            .await
            .unwrap();

        assert_eq!(document_result.total_document_count, 1);
        assert_eq!(document_result.total_chunk_count, 6);
        let canonical_document = document.canonicalize().unwrap();
        assert!(document_result
            .topics
            .iter()
            .flat_map(|topic| &topic.chunks)
            .all(|chunk| chunk.file_path == canonical_document));
        let caches = ctx.topic_tree_caches.lock();
        assert!(caches.root.is_some());
        assert!(caches.document.is_some());
    }

    #[tokio::test]
    async fn document_topics_report_full_library_coverage_without_changing_membership() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("document-coverage-root");
        let other_root = dir.path().join("document-coverage-other-root");
        let stale_root = dir.path().join("document-coverage-stale-root");
        let unindexed_root = dir.path().join("document-coverage-unindexed-root");
        for path in [&root, &other_root, &stale_root, &unindexed_root] {
            std::fs::create_dir_all(path).unwrap();
        }
        let source = root.join("source.txt");
        let matching = other_root.join("matching.txt");
        let unrelated = other_root.join("unrelated.txt");
        let stale_match = stale_root.join("stale-match.txt");
        for path in [&source, &matching, &unrelated, &stale_match] {
            std::fs::write(path, "indexed passage").unwrap();
        }
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        settings.favorites = vec![other_root.clone(), unindexed_root];
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        let mut index = SemanticIndex::create(
            dir.path(),
            "document-coverage-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        let source_chunks = [
            [1.0, 0.0],
            [1.0, 0.0],
            [1.0, 0.0],
            [0.0, 1.0],
            [0.0, 1.0],
            [0.0, 1.0],
        ]
        .into_iter()
        .enumerate()
        .map(|(number, embedding)| {
            let text = format!("source passage {number}");
            (
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: source.clone(),
                    text: text.clone(),
                    byte_range: ByteRange {
                        start: 0,
                        end: text.len(),
                    },
                    origin: SourceOrigin::TextFile {
                        line: (number + 1) as u32,
                        col: 1,
                    },
                },
                embedding.to_vec(),
            )
        })
        .collect();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: source.clone(),
                chunks: source_chunks,
            })
            .unwrap();
        index.activate_root(&other_root).unwrap();
        for (path, embedding) in [
            (matching.clone(), vec![1.0, 0.0]),
            (unrelated.clone(), vec![-1.0, 0.0]),
        ] {
            index
                .write_file(wilkes_core::embed::index::db::PreparedFile {
                    full_text: String::new(),
                    path: path.clone(),
                    chunks: vec![(
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: path,
                            text: "other passage".to_string(),
                            byte_range: ByteRange { start: 0, end: 13 },
                            origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        },
                        embedding,
                    )],
                })
                .unwrap();
        }
        index.activate_root(&stale_root).unwrap();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: stale_match.clone(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: stale_match,
                        text: "stale matching passage".to_string(),
                        byte_range: ByteRange { start: 0, end: 22 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0],
                )],
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let result = Arc::clone(&ctx)
            .chunk_topics(
                "document-coverage".to_string(),
                ChunkTopicsQuery {
                    root,
                    path: Some(source.clone()),
                    granularity: wilkes_core::types::BookmarkClusterGranularity::MuchFewer,
                },
            )
            .await
            .unwrap();

        assert_eq!(result.topics.len(), 2);
        let canonical_source = source.canonicalize().unwrap();
        assert!(result
            .topics
            .iter()
            .flat_map(|topic| &topic.chunks)
            .all(|chunk| chunk.file_path == canonical_source));
        let mut related_counts = result
            .topics
            .iter()
            .map(|topic| {
                let coverage = topic.library_coverage.as_ref().unwrap();
                assert_eq!(coverage.eligible_document_count, 2);
                (
                    coverage.related_document_count,
                    coverage
                        .chunks
                        .iter()
                        .map(|chunk| chunk.file_path.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        related_counts.sort_by_key(|(count, _)| *count);
        assert_eq!(related_counts[0], (0, Vec::new()));
        assert_eq!(related_counts[1].0, 1);
        assert_eq!(related_counts[1].1, vec![matching.canonicalize().unwrap()]);
    }

    #[test]
    fn cancelling_one_topic_request_does_not_cancel_another() {
        let (_dir, ctx) = test_ctx();
        let (_, first_flag) = ctx.start_topic_operation("first").unwrap();
        let (_, second_flag) = ctx.start_topic_operation("second").unwrap();

        ctx.cancel_chunk_topics("first");

        assert!(first_flag.load(Ordering::Relaxed));
        assert!(!second_flag.load(Ordering::Relaxed));
        assert!(ctx.topic_operations.lock().contains_key("second"));
    }

    #[test]
    fn chunk_cluster_identity_depends_only_on_the_member_set() {
        assert_eq!(chunk_cluster_key([3, 1, 2]), chunk_cluster_key([2, 3, 1]));
        assert_ne!(chunk_cluster_key([1, 2]), chunk_cluster_key([1, 3]));
    }

    #[test]
    fn library_roots_are_canonical_deduplicated_and_collapse_nested_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        let nested = root.join("nested");
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let missing = dir.path().join("missing");
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        settings.favorites = vec![nested, sibling.clone()];
        settings.recent_dirs = vec![root.clone(), missing.clone()];

        let (roots, errors) = library_roots(&settings);

        assert_eq!(
            roots,
            vec![
                root.canonicalize().unwrap(),
                sibling.canonicalize().unwrap()
            ]
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains(&missing.display().to_string()));
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct MockEmitter {
        events: Arc<Mutex<Vec<(String, Value)>>>,
    }
    impl EventEmitter for MockEmitter {
        fn emit(&self, name: &str, payload: Value) {
            self.events
                .lock()
                .unwrap()
                .push((name.to_string(), payload));
        }
    }

    struct FakeInstaller {
        install_calls: Arc<AtomicUsize>,
        build_calls: Arc<AtomicUsize>,
        available: bool,
        install_should_fail: bool,
        build_should_fail: bool,
    }

    struct TopicEmbedder {
        calls: Arc<AtomicUsize>,
    }

    impl Embedder for TopicEmbedder {
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(texts
                .iter()
                .map(|text| {
                    if text.contains("cat") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }

        fn model_id(&self) -> &str {
            "topic-test"
        }

        fn dimension(&self) -> usize {
            2
        }

        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Fastembed
        }
    }

    #[async_trait::async_trait]
    impl EmbedderInstaller for FakeInstaller {
        fn is_available(&self, _data_dir: &Path) -> bool {
            self.available
        }

        async fn install(
            &self,
            _data_dir: &Path,
            _tx: mpsc::Sender<EmbedProgress>,
        ) -> anyhow::Result<()> {
            self.install_calls.fetch_add(1, Ordering::Relaxed);
            if self.install_should_fail {
                Err(anyhow::anyhow!("install failed"))
            } else {
                Ok(())
            }
        }

        fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
            Ok(())
        }

        fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
            self.build_calls.fetch_add(1, Ordering::Relaxed);
            if self.build_should_fail {
                Err(anyhow::anyhow!("build failed"))
            } else {
                Ok(Arc::new(MockEmbedder::default()))
            }
        }
    }

    fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (ctx, _rx, _loop) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        (dir, ctx)
    }

    #[tokio::test]
    async fn test_zotero_disable_invalidates_cached_zotero_rows() {
        let (_dir, ctx) = test_ctx();
        let cache = ctx.metadata_cache().expect("cache opens");
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        {
            let guard = cache.lock().unwrap();
            guard
                .upsert(
                    Path::new("/f.pdf"),
                    id,
                    &DocumentMetadata::default(),
                    MetadataSource::File,
                )
                .unwrap();
            guard
                .upsert(
                    Path::new("/z.pdf"),
                    id,
                    &DocumentMetadata::default(),
                    MetadataSource::Zotero,
                )
                .unwrap();
        }

        let mut before = Settings::default();
        before.integrations.zotero.enabled = true;
        let after = Settings::default(); // Zotero disabled.

        ctx.on_zotero_settings_maybe_changed(&before, &after);

        let guard = cache.lock().unwrap();
        assert!(
            guard.get_valid(Path::new("/z.pdf"), id).unwrap().is_none(),
            "zotero-sourced row should be dropped on disable"
        );
        assert!(
            guard.get_valid(Path::new("/f.pdf"), id).unwrap().is_some(),
            "file-sourced row should survive"
        );
    }

    #[tokio::test]
    async fn test_bookmark_methods_round_trip() {
        let (_dir, ctx) = test_ctx();

        let bookmark = ctx
            .add_bookmark(wilkes_core::types::NewBookmark {
                path: "/tmp/example.pdf".into(),
                origin: SourceOrigin::PdfPage {
                    page: 2,
                    bbox: None,
                },
                text_range: None,
                quote: "important".to_string(),
                note: None,
                rects: Vec::new(),
            })
            .await
            .unwrap();

        assert_eq!(ctx.list_bookmarks().await.unwrap().len(), 1);
        assert!(bookmark.note.is_none());

        let noted = ctx
            .update_bookmark_note(&bookmark.id, Some("a note".to_string()))
            .await
            .unwrap();
        assert_eq!(noted.note.as_deref(), Some("a note"));
        assert_eq!(
            ctx.list_bookmarks().await.unwrap()[0].note.as_deref(),
            Some("a note")
        );

        ctx.remove_bookmark(&bookmark.id).await.unwrap();
        assert!(ctx.list_bookmarks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_cluster_bookmarks_embeds_passages_and_reuses_cache() {
        let (_dir, ctx) = test_ctx();
        let calls = Arc::new(AtomicUsize::new(0));
        *ctx.embedder.lock() = Some(Arc::new(TopicEmbedder {
            calls: Arc::clone(&calls),
        }));

        let mut bookmark_ids = Vec::new();
        for quote in [
            "cat behavior",
            "cat nutrition",
            "cat genetics",
            "quantum fields",
            "particle physics",
            "wave functions",
        ] {
            let bookmark = ctx
                .add_bookmark(NewBookmark {
                    path: PathBuf::from("/tmp/paper.pdf"),
                    origin: SourceOrigin::PdfPage {
                        page: 1,
                        bbox: None,
                    },
                    text_range: None,
                    quote: quote.into(),
                    note: None,
                    rects: Vec::new(),
                })
                .await
                .unwrap();
            bookmark_ids.push(bookmark.id);
        }
        let query = BookmarkClustersQuery {
            bookmark_ids: bookmark_ids.clone(),
            granularity: wilkes_core::types::BookmarkClusterGranularity::Balanced,
        };

        let first = ctx.clone().cluster_bookmarks(query.clone()).await.unwrap();
        assert_eq!(first.clusters.len(), 2);
        assert_eq!(first.clusters[0].bookmark_ids, bookmark_ids[..3]);
        assert_eq!(first.clusters[1].bookmark_ids, bookmark_ids[3..]);
        assert!(first.unclustered_bookmark_ids.is_empty());
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        let second = ctx.clone().cluster_bookmarks(query.clone()).await.unwrap();
        assert_eq!(second.clusters.len(), 2);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "the second request should use persisted bookmark embeddings"
        );

        let mut more_granular_query = query.clone();
        more_granular_query.granularity = wilkes_core::types::BookmarkClusterGranularity::More;
        ctx.clone()
            .cluster_bookmarks(more_granular_query)
            .await
            .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "changing granularity should reuse persisted bookmark embeddings"
        );

        ctx.update_bookmark_note(&bookmark_ids[0], Some("updated".into()))
            .await
            .unwrap();
        ctx.cluster_bookmarks(query).await.unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "editing one note should embed only that cache miss"
        );
    }

    fn generation_ctx(
        dir: &Path,
        events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    ) -> Arc<AppContext> {
        let emitter = Arc::new(MockEmitter { events });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.to_path_buf(),
            dir.join("settings.json"),
            WorkerPaths::resolve(dir),
            emitter,
        );
        ctx
    }

    async fn enable_generation(ctx: &Arc<AppContext>, model: &str) {
        update_settings(
            &ctx.settings_path,
            serde_json::json!({
                "generation": {
                    "enabled": true,
                    "model": model,
                    "device": "cpu",
                    "sampling_overrides": {},
                }
            }),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn document_summary_uses_the_shared_delta_and_terminal_event_contract() {
        let dir = tempdir().unwrap();
        // Desktop documents normally live outside the app-support data dir.
        // Filesystem confinement belongs to the HTTP uploads boundary.
        let documents = tempdir().unwrap();
        let path = documents.path().join("paper.txt");
        std::fs::write(&path, "A source document with a result.").unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), Arc::clone(&events));
        enable_generation(&ctx, "mock-generator").await;
        *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted(["A complete summary."])));

        Arc::clone(&ctx)
            .summarize_document("summary-request".to_string(), path)
            .await
            .unwrap();

        let events = events.lock().unwrap();
        let streamed = events
            .iter()
            .filter(|(name, _)| name == "generation-stream")
            .map(|(_, payload)| {
                serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            streamed,
            vec![
                GenerationStreamEvent::Delta {
                    request_id: "summary-request".to_string(),
                    task: GenerationTask::DocumentSummary,
                    delta: "A ".to_string(),
                },
                GenerationStreamEvent::Delta {
                    request_id: "summary-request".to_string(),
                    task: GenerationTask::DocumentSummary,
                    delta: "complete ".to_string(),
                },
                GenerationStreamEvent::Delta {
                    request_id: "summary-request".to_string(),
                    task: GenerationTask::DocumentSummary,
                    delta: "summary.".to_string(),
                },
                GenerationStreamEvent::Completed {
                    request_id: "summary-request".to_string(),
                    task: GenerationTask::DocumentSummary,
                    text: "A complete summary.".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn search_results_summary_uses_its_own_correlated_task() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), Arc::clone(&events));
        enable_generation(&ctx, "mock-generator").await;
        *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted([
            "The leading studies agree on the measured outcome [1].",
        ])));
        let input = SearchResultsSummaryInput {
            query: "agreement".to_string(),
            sources: vec![
                wilkes_core::generate::tasks::search_results_summary::SearchResultsSummarySource {
                    title: "paper.pdf".to_string(),
                },
            ],
            passages: vec![
                wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryPassage {
                    text: "The leading studies agree on the measured outcome.".to_string(),
                    source_index: 0,
                },
            ],
        };

        Arc::clone(&ctx)
            .summarize_search_results("results-request".to_string(), input)
            .await
            .unwrap();

        let events = events.lock().unwrap();
        assert!(events.iter().any(|(name, payload)| {
            name == "generation-stream"
                && matches!(
                    serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap(),
                    GenerationStreamEvent::Completed {
                        request_id,
                        task: GenerationTask::SearchResultsSummary,
                        text,
                    } if request_id == "results-request"
                        && text == "The leading studies agree on the measured outcome [1]."
                )
        }));
    }

    #[tokio::test]
    async fn generation_task_failure_emits_one_correlated_failed_terminal_event() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "  ").unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), Arc::clone(&events));
        enable_generation(&ctx, "mock-generator").await;
        *ctx.generator.lock() = Some(Arc::new(MockGenerator::scripted(["unused"])));

        let error = Arc::clone(&ctx)
            .summarize_document("failed-request".to_string(), path)
            .await
            .unwrap_err();
        assert!(error.contains("document has no extractable text"));

        let events = events.lock().unwrap();
        let streamed = events
            .iter()
            .filter(|(name, _)| name == "generation-stream")
            .map(|(_, payload)| {
                serde_json::from_value::<GenerationStreamEvent>(payload.clone()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(streamed.len(), 1);
        assert!(matches!(
            &streamed[0],
            GenerationStreamEvent::Failed {
                request_id,
                task: GenerationTask::DocumentSummary,
                error,
            } if request_id == "failed-request"
                && error.contains("document has no extractable text")
        ));
    }

    /// The generation install must not borrow the embed event stream: that
    /// stream globally sets "indexing", and this path emits no `embed-done` to
    /// ever clear it. Failure or success, generation reports on its own events.
    #[tokio::test]
    async fn generation_load_never_touches_the_embed_event_stream() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), Arc::clone(&events));
        enable_generation(&ctx, "not/a-real-model").await;

        let err = ctx.load_generator().await.unwrap_err();
        assert!(
            err.to_string().contains("not a known generation model"),
            "{err:#}"
        );

        let events = events.lock().unwrap();
        assert!(
            !events.iter().any(|(name, _)| name.starts_with("embed-")),
            "generation emitted an embed event: {events:?}"
        );
        assert!(events
            .iter()
            .any(|(name, payload)| name == "generation-error"
                && payload["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("not a known generation model"))));
    }

    /// Two settings changes in quick succession each spawn a load. The one that
    /// is no longer current must leave the generator alone, whatever order the
    /// two finish in — readiness only checks that *some* generator is attached,
    /// so an overwrite here silently runs the wrong model.
    #[tokio::test]
    async fn a_superseded_load_does_not_attach_its_generator() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), Arc::clone(&events));
        enable_generation(&ctx, "not/a-real-model").await;

        // Hold the load lock so the load below queues, then claim a newer epoch
        // on its behalf — exactly what a second `load_generator` call does.
        let held = ctx.generator_load_lock.lock().await;
        let load = {
            let ctx = Arc::clone(&ctx);
            tokio::spawn(async move { ctx.load_generator().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        ctx.generator_epoch.fetch_add(1, Ordering::SeqCst);
        drop(held);

        assert_eq!(load.await.unwrap().unwrap(), GeneratorLoad::Superseded);
        assert!(ctx.generator.lock().is_none());
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|(name, _)| name == "generation-error"),
            "a superseded load must not report a failure"
        );
    }

    #[tokio::test]
    async fn unloading_supersedes_a_generation_load_already_in_flight() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = generation_ctx(dir.path(), events);
        enable_generation(&ctx, "not/a-real-model").await;

        let held = ctx.generator_load_lock.lock().await;
        let load = {
            let ctx = Arc::clone(&ctx);
            tokio::spawn(async move { ctx.load_generator().await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        ctx.unload_generator();
        drop(held);

        assert_eq!(load.await.unwrap().unwrap(), GeneratorLoad::Superseded);
        assert!(ctx.generator.lock().is_none());
    }

    #[test]
    fn test_emit_embed_error_logs_and_emits() {
        wilkes_core::logging::clear_logs();

        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let subscriber = tracing_subscriber::registry().with(wilkes_core::logging::BufferLayer);
        subscriber::with_default(subscriber, || {
            ctx.emit_embed_error("Build", "Worker error");
        });

        let logs = wilkes_core::logging::get_logs();
        assert!(logs
            .iter()
            .any(|line| line.contains("Build failed: Worker error")));

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"] == "Worker error"
        }));
    }

    #[test]
    fn test_emit_embed_error_preserves_error_chain() {
        wilkes_core::logging::clear_logs();

        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let err = anyhow::anyhow!("inner worker failure")
            .context("Fatal embedder error while indexing /tmp/example.md");

        let subscriber = tracing_subscriber::registry().with(wilkes_core::logging::BufferLayer);
        subscriber::with_default(subscriber, || {
            ctx.emit_embed_error("Build", format!("{err:#}"));
        });

        let logs = wilkes_core::logging::get_logs();
        assert!(logs.iter().any(|line| {
            line.contains("Build failed: Fatal embedder error while indexing /tmp/example.md")
        }));
        assert!(logs
            .iter()
            .any(|line| line.contains("inner worker failure")));

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"].as_str().is_some_and(|msg| {
                    msg.contains("Fatal embedder error while indexing /tmp/example.md")
                        && msg.contains("inner worker failure")
                })
        }));
    }

    #[test]
    fn test_embed_operation_as_str() {
        assert_eq!(EmbedOperation::Download.as_str(), "Download");
        assert_eq!(EmbedOperation::Build.as_str(), "Build");
    }

    #[test]
    fn test_restore_state_needs_reset_on_missing_db() {
        let settings = Settings {
            favorites: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder::default_for(EmbeddingEngine::Candle),
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            integrations: Default::default(),
            supported_extensions: vec![],
            max_results: 0,
            bookmarks_dock: BookmarkDock::default(),
            ..Settings::default()
        };

        assert!(AppContext::restore_state_needs_reset(&settings, None));
    }

    #[test]
    fn test_restore_state_needs_reset_on_mismatch() {
        let settings = Settings {
            favorites: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            integrations: Default::default(),
            supported_extensions: vec![],
            max_results: 0,
            bookmarks_dock: BookmarkDock::default(),
            ..Settings::default()
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Candle,
            model_id: "model-b".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        assert!(AppContext::restore_state_needs_reset(
            &settings,
            Some(&db_status)
        ));
    }

    #[test]
    fn test_restore_state_needs_reset_false_when_matching() {
        let settings = Settings {
            favorites: vec![],
            recent_dirs: vec![],
            last_directory: None,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            theme: Theme::default(),
            search_prefer_semantic: false,
            semantic: SemanticSettings {
                enabled: true,
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            integrations: Default::default(),
            supported_extensions: vec![],
            max_results: 0,
            bookmarks_dock: BookmarkDock::default(),
            ..Settings::default()
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Fastembed,
            model_id: "model-a".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        assert!(!AppContext::restore_state_needs_reset(
            &settings,
            Some(&db_status)
        ));
    }

    #[test]
    fn test_restore_state_indexing_config() {
        let settings = Settings {
            supported_extensions: vec!["txt".to_string(), "md".to_string()],
            semantic: SemanticSettings {
                chunk_size: 128,
                chunk_overlap: 32,
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };

        let indexing = AppContext::restore_state_indexing_config(&settings);
        assert_eq!(indexing.chunk_size, 128);
        assert_eq!(indexing.chunk_overlap, 32);
        assert_eq!(indexing.supported_extensions, vec!["txt", "md"]);
    }

    #[test]
    fn test_restore_state_enabled_settings() {
        let current = SemanticSettings {
            enabled: false,
            index_path: None,
            ..SemanticSettings::default()
        };
        let selected = SelectedEmbedder {
            engine: EmbeddingEngine::Fastembed,
            model: EmbedderModel("model-a".to_string()),
            dimension: 384,
        };
        let updated = AppContext::restore_state_enabled_settings(
            current,
            PathBuf::from("semantic_index.db"),
            selected,
            768,
        );

        assert!(updated.enabled);
        assert_eq!(updated.index_path, Some(PathBuf::from("semantic_index.db")));
        assert_eq!(updated.selected.dimension, 768);
    }

    #[test]
    fn test_open_semantic_index_with_error() {
        let dir = tempdir().unwrap();
        let result = AppContext::open_semantic_index_with(
            &dir.path().to_path_buf(),
            "model-a",
            384,
            |_dir, _model_id, _dim| Err(anyhow::anyhow!("open failed")),
        );

        match result {
            Ok(_) => panic!("expected open error"),
            Err(err) => assert!(err.to_string().contains("open failed")),
        }
    }

    #[tokio::test]
    async fn test_start_directory_watcher_invalid_root_leaves_no_watcher() {
        let dir = tempdir().unwrap();
        let (_tmp, ctx) = test_ctx();
        ctx.start_directory_watcher(dir.path().join("missing"));
        assert!(ctx.directory_watcher.lock().is_none());
    }

    fn running_embed_task() -> EmbedTaskHandle {
        EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel: CancellationToken::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join: tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                Ok(())
            }),
        }
    }

    #[tokio::test]
    async fn test_embed_task_helpers_track_state() {
        let (_dir, ctx) = test_ctx();
        assert!(!ctx.embed_task_is_running());

        *ctx.embed_task.lock() = Some(running_embed_task());
        assert!(ctx.embed_task_is_running());

        ctx.clear_embed_task();
        assert!(!ctx.embed_task_is_running());
        assert!(ctx.embed_task.lock().is_none());
    }

    #[tokio::test]
    async fn test_prepare_build_index_happy_path() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "hello").unwrap();

        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let plan = ctx
            .prepare_build_index(&root.to_string_lossy(), &selected)
            .await
            .unwrap();

        assert_eq!(plan.root_path, root);
        assert_eq!(
            plan.chunk_size,
            ctx.get_settings().await.semantic.chunk_size
        );
        assert_eq!(
            plan.chunk_overlap,
            ctx.get_settings().await.semantic.chunk_overlap
        );
        assert_eq!(
            plan.supported_extensions,
            ctx.get_settings().await.supported_extensions
        );
        assert_eq!(
            plan.device,
            ctx.get_settings()
                .await
                .semantic
                .device_for(EmbeddingEngine::Candle)
                .to_string()
        );
    }

    #[tokio::test]
    async fn test_prepare_build_index_rejects_running_task() {
        let (_dir, ctx) = test_ctx();
        *ctx.embed_task.lock() = Some(running_embed_task());

        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let err = ctx
            .prepare_build_index("/tmp", &selected)
            .await
            .unwrap_err();

        assert!(err.contains("already in progress"));
    }

    #[tokio::test]
    async fn test_prepare_build_index_validates_root_path() {
        let (_dir, ctx) = test_ctx();
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);

        let missing = ctx
            .prepare_build_index("/definitely/missing/path", &selected)
            .await
            .unwrap_err();
        assert!(missing.contains("Index root not found"));

        let file_dir = tempdir().unwrap();
        let file_path = file_dir.path().join("not_a_dir");
        std::fs::write(&file_path, "hello").unwrap();
        let not_dir = ctx
            .prepare_build_index(&file_path.to_string_lossy(), &selected)
            .await
            .unwrap_err();
        assert!(not_dir.contains("not a directory"));

        let empty_dir = tempdir().unwrap();
        let empty = ctx
            .prepare_build_index(&empty_dir.path().to_string_lossy(), &selected)
            .await
            .unwrap_err();
        assert!(empty.contains("No supported files found"));
    }

    #[tokio::test]
    async fn test_prepare_download_model_happy_path_and_running_guard() {
        let (dir, ctx) = test_ctx();
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "hello").unwrap();
        ctx.update_settings(serde_json::json!({ "last_directory": root }))
            .await
            .unwrap();

        let plan = ctx.prepare_download_model(&selected).await.unwrap();
        assert_eq!(
            plan.device,
            ctx.get_settings()
                .await
                .semantic
                .device_for(EmbeddingEngine::Candle)
                .to_string()
        );

        *ctx.embed_task.lock() = Some(running_embed_task());
        let err = ctx.prepare_download_model(&selected).await.unwrap_err();
        assert!(err.contains("already in progress"));
    }

    #[test]
    fn test_build_index_options_and_cleanup_partial_files() {
        let dir = tempdir().unwrap();
        let plan = BuildIndexPlan {
            root_path: dir.path().join("root"),
            device: "cpu".to_string(),
            chunk_size: 123,
            chunk_overlap: 45,
            supported_extensions: vec!["rs".to_string(), "txt".to_string()],
        };
        let (tx, _rx) = mpsc::channel(1);
        let options = AppContext::build_index_options(
            WorkerManager::new(WorkerPaths {
                python_path: PathBuf::from("python"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("reqs.txt"),
                venv_dir: PathBuf::from("venv"),
                worker_bin: PathBuf::from("worker"),
                data_dir: dir.path().to_path_buf(),
            })
            .0,
            dir.path().to_path_buf(),
            &plan,
            tx,
            Arc::new(AtomicBool::new(false)),
        );

        assert_eq!(options.device.as_deref(), Some("cpu"));
        assert_eq!(options.chunk_size, 123);
        assert_eq!(options.chunk_overlap, 45);
        assert_eq!(options.supported_extensions, vec!["rs", "txt"]);

        for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
            std::fs::write(dir.path().join(format!("semantic_index.db{suffix}")), "x").unwrap();
        }
        AppContext::cleanup_partial_index_files(dir.path());
        for suffix in [".tmp", ".tmp-wal", ".tmp-shm"] {
            assert!(!dir
                .path()
                .join(format!("semantic_index.db{suffix}"))
                .exists());
        }
    }

    #[tokio::test]
    async fn test_emit_progress_helpers() {
        let dir = tempdir().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: Arc::clone(&captured),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );
        let progress = EmbedProgress::Build(wilkes_core::models::progress::IndexBuildProgress {
            files_processed: 1,
            total_files: 2,
            message: "building".to_string(),
            done: false,
        });
        ctx.emit_progress_event(&progress);

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "embed-progress");
    }

    #[tokio::test]
    async fn test_forward_embed_progress_emits_events() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let emitter: Arc<dyn EventEmitter> = Arc::new(MockEmitter {
            events: Arc::clone(&captured),
        });
        let (tx, rx) = mpsc::channel(2);

        let forward = tokio::spawn(AppContext::forward_embed_progress(Arc::clone(&emitter), rx));
        tx.send(EmbedProgress::Download(
            wilkes_core::models::progress::DownloadProgress {
                bytes_received: 3,
                total_bytes: 9,
                done: false,
            },
        ))
        .await
        .unwrap();
        drop(tx);
        forward.await.unwrap();

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "embed-progress");
    }

    #[tokio::test]
    async fn test_open_built_index_error_and_store_loaded_state() {
        let (dir, ctx) = test_ctx();
        let err = match ctx
            .open_built_index(dir.path().to_path_buf(), "missing-model".to_string(), 384)
            .await
        {
            Ok(_) => panic!("expected open_built_index to fail"),
            Err(err) => err,
        };
        assert!(!err.is_empty());

        let data_dir = ctx.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir,
            "mock-model",
            384,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let index_arc = ctx.restore_store_loaded_state(Arc::clone(&embedder), index);

        assert!(ctx.embedder.lock().is_some());
        assert!(ctx.index.lock().lock().unwrap().is_some());
        assert!(index_arc.lock().unwrap().is_some());
    }

    #[test]
    fn test_prepare_restore_state_plan_variants() {
        let matching = Settings {
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("model-a".to_string()),
                    dimension: 384,
                },
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        let db_status = IndexStatus {
            indexed_files: 1,
            total_chunks: 1,
            built_at: None,
            build_duration_ms: None,
            engine: EmbeddingEngine::Candle,
            model_id: "model-a".to_string(),
            dimension: 384,
            root_path: None,
            db_size_bytes: None,
        };

        match AppContext::prepare_restore_state_plan(matching.clone(), db_status.clone()) {
            RestoreStatePreparation::Ready(plan) => {
                assert_eq!(plan.selected.model.model_id(), "model-a");
                assert_eq!(plan.db_status.model_id, "model-a");
            }
            RestoreStatePreparation::ResetStaleSelection { .. } => panic!("expected ready plan"),
        }

        let mismatched = Settings {
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Fastembed,
                    model: EmbedderModel("model-b".to_string()),
                    dimension: 384,
                },
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        match AppContext::prepare_restore_state_plan(mismatched, db_status) {
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                assert_eq!(db_status.model_id, "model-a");
                assert_eq!(selected.model.model_id(), "model-b");
            }
            RestoreStatePreparation::Ready(_) => panic!("expected stale-selection reset"),
        }
    }

    #[tokio::test]
    async fn test_restore_embedder_with_installer_branches() {
        let (_dir, ctx) = test_ctx();

        let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: true,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(install_fail).await.is_none());

        let unavailable: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: false,
            install_should_fail: false,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(unavailable).await.is_none());

        let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: true,
        });
        assert!(ctx.restore_embedder_with(build_fail).await.is_none());

        let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: false,
        });
        assert!(ctx.restore_embedder_with(ok).await.is_some());
    }

    #[tokio::test]
    async fn test_probe_and_load_downloaded_model_with_installer_branches() {
        let (_dir, ctx) = test_ctx();

        let install_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: true,
            build_should_fail: false,
        });
        let err = ctx
            .probe_and_load_downloaded_model_with(install_fail)
            .await
            .unwrap_err();
        assert!(err.contains("Failed to probe model dimensions"));

        let build_fail: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: true,
        });
        let err = ctx
            .probe_and_load_downloaded_model_with(build_fail)
            .await
            .unwrap_err();
        assert!(err.contains("build failed"));

        let ok: Arc<dyn EmbedderInstaller> = Arc::new(FakeInstaller {
            install_calls: Arc::new(AtomicUsize::new(0)),
            build_calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            install_should_fail: false,
            build_should_fail: false,
        });
        ctx.probe_and_load_downloaded_model_with(ok).await.unwrap();
        assert!(ctx.embedder.lock().is_some());
    }

    #[tokio::test]
    async fn test_finish_restore_state_persists_semantic_state() {
        let (_dir2, ctx2) = test_ctx();
        let data_dir2 = ctx2.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir2,
            "restore-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        let plan = RestoreStatePlan {
            db_status: IndexStatus {
                indexed_files: 1,
                total_chunks: 1,
                built_at: None,
                build_duration_ms: None,
                engine: EmbeddingEngine::Candle,
                model_id: "restore-model".to_string(),
                dimension: 384,
                root_path: None,
                db_size_bytes: None,
            },
            selected: SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("restore-model".to_string()),
                dimension: 1,
            },
            device: "cpu".to_string(),
        };
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        ctx2.finish_restore_state(&plan, embedder, index).await;

        let settings = ctx2.get_settings().await;
        assert!(settings.semantic.enabled);
        assert_eq!(settings.semantic.selected.dimension, 384);
    }

    #[tokio::test]
    async fn test_restore_state_skips_when_semantic_pref_off() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        // A fully restorable index DB sits on disk with the default selection,
        // so the only thing that can prevent restore is the user's toggle.
        let selected = SelectedEmbedder::default();
        SemanticIndex::create(
            &ctx.data_dir,
            selected.model.model_id(),
            384,
            selected.engine,
            None,
        )
        .unwrap();

        // Persist settings with the semantic toggle OFF while the built index is
        // still marked enabled and its path present (the exact state a user is in
        // after building and then unchecking semantic search).
        let disabled = Settings {
            search_prefer_semantic: false,
            last_directory: Some(root),
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(ctx.data_dir.join("semantic_index.db")),
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&disabled).unwrap(),
        )
        .unwrap();

        // Positive control: the same settings with the toggle ON would be deemed
        // restorable (selection matches the DB), proving `search_prefer_semantic`
        // is the decisive gate rather than a stale-selection reset.
        let enabled = Settings {
            search_prefer_semantic: true,
            ..disabled.clone()
        };
        let db_status = ctx
            .load_restore_db_status(&enabled)
            .await
            .expect("db status present");
        assert!(!AppContext::restore_state_needs_reset(
            &enabled,
            Some(&db_status)
        ));

        Arc::clone(&ctx).restore_state().await;

        // Directory watching is independent of semantic restore, so it starts
        // for file-list invalidation even when semantic search is disabled.
        assert!(ctx.directory_watcher.lock().is_some());
        assert!(ctx.embedder.lock().is_none());
        assert!(!ctx.get_settings().await.search_prefer_semantic);
    }

    #[tokio::test]
    async fn test_update_settings_last_directory_restarts_directory_watcher() {
        let (dir, ctx) = test_ctx();
        let root1 = dir.path().join("root1");
        let root2 = dir.path().join("root2");
        std::fs::create_dir_all(&root1).unwrap();
        std::fs::create_dir_all(&root2).unwrap();

        ctx.update_settings(serde_json::json!({ "last_directory": root1 }))
            .await
            .unwrap();
        assert!(ctx.directory_watcher.lock().is_some());

        ctx.update_settings(serde_json::json!({ "last_directory": root2 }))
            .await
            .unwrap();
        assert!(ctx.directory_watcher.lock().is_some());

        ctx.update_settings(serde_json::json!({ "last_directory": null }))
            .await
            .unwrap();
        assert!(ctx.directory_watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_update_settings_pref_off_keeps_directory_watcher_and_tears_down_semantic() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        // Seed persisted settings with the toggle already ON so the update below
        // is a pure ON->OFF transition (no stray activate spawn to race with).
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&Settings {
                search_prefer_semantic: true,
                last_directory: Some(root.clone()),
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();

        // Stand up a resident index + watcher as if semantic were active.
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "teardown-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
        *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
        ctx.start_directory_watcher(root);
        assert!(ctx.directory_watcher.lock().is_some());

        ctx.update_settings(serde_json::json!({ "search_prefer_semantic": false }))
            .await
            .unwrap();

        // Turning the toggle off must leave file-list watching active while
        // releasing resident semantic state so file changes no longer reindex.
        assert!(ctx.directory_watcher.lock().is_some());
        assert!(ctx.embedder.lock().is_none());
        assert!(ctx
            .index
            .lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none());
    }

    #[tokio::test]
    async fn test_load_restore_db_status_clears_stale_settings_when_missing_db() {
        let (_dir, ctx) = test_ctx();
        let settings = Settings {
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        ctx.update_settings(serde_json::json!({
            "semantic": {
                "enabled": true,
                "index_path": "semantic_index.db"
            }
        }))
        .await
        .unwrap();

        let db_status = ctx.load_restore_db_status(&settings).await;
        assert!(db_status.is_none());

        let updated = ctx.get_settings().await;
        assert!(!updated.semantic.enabled);
        assert!(updated.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_load_restore_db_status_reads_existing_db() {
        let (dir, ctx) = test_ctx();
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "restore-model",
            384,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        let settings = Settings {
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(PathBuf::from("semantic_index.db")),
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };

        let db_status = ctx.load_restore_db_status(&settings).await;
        let db_status = db_status.expect("expected restore db status");
        assert_eq!(db_status.model_id, "restore-model");
        assert_eq!(db_status.dimension, 384);

        drop(index);
    }

    #[tokio::test]
    async fn test_finish_build_index_starts_directory_watcher_and_persists_state() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let data_dir = ctx.data_dir.clone();
        let index = SemanticIndex::create(
            &data_dir,
            "build-model",
            384,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        let plan = BuildIndexPlan {
            root_path: root.clone(),
            device: "cpu".to_string(),
            chunk_size: 64,
            chunk_overlap: 8,
            supported_extensions: vec!["txt".to_string()],
        };
        let selected = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("build-model".to_string()),
            dimension: 384,
        };
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        ctx.finish_build_index(&plan, &selected, &data_dir, embedder)
            .await
            .unwrap();

        assert!(ctx.embedder.lock().is_some());
        assert!(ctx.index.lock().lock().unwrap().is_some());
        assert!(ctx.directory_watcher.lock().is_some());

        let settings = ctx.get_settings().await;
        assert!(settings.semantic.enabled);
        assert_eq!(
            settings.semantic.index_path,
            Some(data_dir.join("semantic_index.db"))
        );

        ctx.stop_directory_watcher();
        assert!(ctx.directory_watcher.lock().is_none());
        drop(index);
    }

    #[tokio::test]
    async fn test_start_build_watcher_failure_leaves_no_watcher() {
        let (_dir, ctx) = test_ctx();
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "watch-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        let index_arc = Arc::new(Mutex::new(Some(index)));
        let missing_root = PathBuf::from("/definitely/missing/watcher/root");
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        ctx.start_build_watcher(
            missing_root,
            index_arc,
            embedder,
            IndexingConfig {
                chunk_size: 64,
                chunk_overlap: 8,
                supported_extensions: vec!["txt".to_string()],
            },
        );

        assert!(ctx.directory_watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_spawn_build_index_task_cancellation_cleans_up_without_emitting_done() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: Arc::clone(&events),
        });
        let (ctx, _rx, loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: dir.path().join("missing-worker"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );
        let _loop_handle = tokio::spawn(loop_fut);
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let stale = ctx.data_dir.join("semantic_index.db.tmp");
        std::fs::write(&stale, "stale").unwrap();

        let plan = BuildIndexPlan {
            root_path: root,
            device: "cpu".to_string(),
            chunk_size: 64,
            chunk_overlap: 8,
            supported_extensions: vec!["txt".to_string()],
        };
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let handle = Arc::clone(&ctx).spawn_build_index_task(
            plan,
            selected,
            cancel,
            Arc::clone(&cancel_flag),
        );

        handle.await.unwrap().unwrap();

        assert!(cancel_flag.load(Ordering::Relaxed));
        assert!(!stale.exists());

        let events = events.lock().unwrap();
        assert!(events.iter().any(|(name, payload)| {
            name == "embed-error" && payload["operation"] == "Build" && payload["message"] == ""
        }));
        assert!(events.iter().any(|(name, payload)| {
            name == "manager-event" && payload == &serde_json::json!("ReindexingCancelled")
        }));
        assert!(events.iter().all(|(name, payload)| name != "manager-event"
            || payload != &serde_json::json!("ReindexingDone")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_spawn_build_index_task_failure_emits_cancelled_without_done() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: Arc::clone(&events),
        });
        let worker_bin = dir.path().join("worker.sh");
        write_executable(
            &worker_bin,
            r#"#!/bin/sh
read req
echo '{"Error":"Model '\''DefinitelyNotARealFastembedModel'\'' is not supported by fastembed"}'
exit 0
"#,
        );
        let (ctx, _rx, loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin,
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );
        let _loop_handle = tokio::spawn(loop_fut);
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let plan = BuildIndexPlan {
            root_path: root,
            device: "cpu".to_string(),
            chunk_size: 64,
            chunk_overlap: 8,
            supported_extensions: vec!["txt".to_string()],
        };
        let selected = SelectedEmbedder {
            engine: EmbeddingEngine::Fastembed,
            model: EmbedderModel("DefinitelyNotARealFastembedModel".to_string()),
            dimension: 384,
        };
        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));

        let handle = Arc::clone(&ctx).spawn_build_index_task(
            plan,
            selected,
            cancel,
            Arc::clone(&cancel_flag),
        );

        handle.await.unwrap().unwrap();

        assert!(!cancel_flag.load(Ordering::Relaxed));

        let events = events.lock().unwrap();
        assert!(events.iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"]
                    .as_str()
                    .is_some_and(|msg| msg.contains("is not supported by fastembed"))
        }));
        assert!(events.iter().any(|(name, payload)| {
            name == "manager-event" && payload == &serde_json::json!("ReindexingCancelled")
        }));
        assert!(events.iter().all(|(name, payload)| {
            name != "manager-event" || payload != &serde_json::json!("ReindexingDone")
        }));
    }

    #[tokio::test]
    async fn test_app_context_new() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("py_pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (ctx, _event_rx, _loop_fut) =
            AppContext::new(data_dir, settings_path.clone(), paths, emitter);

        ctx.update_semantic_settings(|s| SemanticSettings {
            enabled: true,
            chunk_size: 1234,
            ..s
        })
        .await;

        let updated = get_settings(&settings_path).await.unwrap();
        assert_eq!(updated.semantic.enabled, true);
        assert_eq!(updated.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_event_forwarder() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter.clone(),
        );

        let (tx, rx) = mpsc::channel(1);
        let forwarder = tokio::spawn(ctx.run_event_forwarder(rx));

        tx.send(ManagerEvent::WorkerStarting).await.unwrap();
        tx.send(ManagerEvent::ReindexingDone).await.unwrap();
        drop(tx);
        forwarder.await.unwrap();

        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 2);
        assert_eq!(events_guard[0].0, "manager-event");
        assert_eq!(events_guard[0].1, serde_json::json!("WorkerStarting"));
        assert_eq!(events_guard[1].1, serde_json::json!("ReindexingDone"));
    }

    #[tokio::test]
    async fn test_stop_directory_watcher() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.start_directory_watcher(dir.path().to_path_buf());
        assert!(ctx.directory_watcher.lock().is_some());
        ctx.stop_directory_watcher();
        assert!(ctx.directory_watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_is_semantic_ready() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        assert!(!ctx.is_semantic_ready());

        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        *ctx.embedder.lock() = Some(embedder);
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "semantic-ready",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
        assert!(ctx.is_semantic_ready());
    }

    #[tokio::test]
    async fn test_cancel_embed() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let cancel = CancellationToken::new();
        let join = tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Download,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        ctx.cancel_embed().await; // Should not panic
        assert!(ctx.embed_task.lock().is_none());

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, payload)| {
            name == "embed-error" && payload["operation"] == "Download" && payload["message"] == ""
        }));
    }

    #[tokio::test]
    async fn test_shutdown() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.start_directory_watcher(dir.path().to_path_buf());

        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let join = tokio::spawn(async move {
            cancel_for_task.cancelled().await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        ctx.shutdown().await;

        assert!(ctx.directory_watcher.lock().is_none());
        assert!(ctx.embed_task.lock().is_none());
    }

    #[tokio::test]
    async fn test_prepare_build_index_rejects_retry_while_cancel_in_progress() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );

        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let selected = SelectedEmbedder::default_for(EmbeddingEngine::Candle);
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let join = tokio::spawn(async move {
            cancel_for_task.cancelled().await;
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let cancel_ctx = Arc::clone(&ctx);
        let cancel_task = tokio::spawn(async move {
            cancel_ctx.cancel_embed().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let err = ctx
            .prepare_build_index(&root.to_string_lossy(), &selected)
            .await
            .unwrap_err();
        assert_eq!(err, "A build is already in progress.");

        cancel_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_delete_index() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
        let res = ctx.delete_index(None).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_index_status() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let res = ctx.get_index_status(None).await;
        assert!(res.is_err()); // No index exists
    }

    #[tokio::test]
    async fn test_kill_worker() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.kill_worker(); // Should not panic
    }

    #[tokio::test]
    async fn test_set_worker_timeout() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let res = ctx.set_worker_timeout(100).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_settings_operations() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let initial = ctx.get_settings().await;
        assert_eq!(initial.context_lines, 2);

        let patch = serde_json::json!({ "context_lines": 5 });
        let updated = ctx.update_settings(patch).await.unwrap();
        assert_eq!(updated.context_lines, 5);

        let _updated_semantic = ctx
            .update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
            .await;

        // Settings should have been saved to disk
        let disk_content = tokio::fs::read_to_string(&settings_path).await.unwrap();
        assert!(disk_content.contains("\"context_lines\": 5"));
        assert!(disk_content.contains("\"enabled\": true"));
    }

    #[tokio::test]
    async fn test_file_operations() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        tokio::fs::write(root.join("test.txt"), "hello")
            .await
            .unwrap();
        tokio::fs::write(root.join("test.pdf"), "fake pdf")
            .await
            .unwrap();
        tokio::fs::create_dir(root.join("subdir")).await.unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let files = ctx.list_files(root.to_path_buf()).await.unwrap();
        assert!(files.files.len() >= 2);

        let preview = ctx.open_file(root.join("test.txt")).await.unwrap();
        match preview {
            PreviewData::Text { content, .. } => assert!(content.contains("hello")),
            _ => panic!("Expected Text preview"),
        }
    }

    #[tokio::test]
    async fn test_start_search_grep() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": dir.path() }),
        )
        .await
        .unwrap();

        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let handle = ctx.clone().start_search(query).await.unwrap();
        // SearchHandle only has rx field (mpsc::Receiver)
        drop(handle);
    }

    #[test]
    fn test_prepare_search_query_normalizes_file_scope() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("paper.txt"), "hello").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: SearchScope::File {
                path: PathBuf::from("paper.txt"),
            },
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let prepared = AppContext::prepare_search_query(
            query,
            vec!["txt".to_string()],
            &[dir.path().canonicalize().unwrap()],
        )
        .unwrap();
        assert_eq!(prepared.root, std::fs::canonicalize(dir.path()).unwrap());
        assert_eq!(
            prepared.scope,
            SearchScope::File {
                path: std::fs::canonicalize(dir.path().join("paper.txt")).unwrap()
            }
        );
    }

    #[test]
    fn test_prepare_search_query_accepts_file_in_another_library_root() {
        let root = tempdir().unwrap();
        let other_root = tempdir().unwrap();
        let other_file = other_root.path().join("paper.txt");
        std::fs::write(&other_file, "hello").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: SearchScope::File {
                path: other_file.clone(),
            },
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let prepared = AppContext::prepare_search_query(
            query,
            vec!["txt".to_string()],
            &[
                root.path().canonicalize().unwrap(),
                other_root.path().canonicalize().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(
            prepared.scope,
            SearchScope::File {
                path: other_file.canonicalize().unwrap()
            }
        );
    }

    #[test]
    fn test_prepare_search_query_rejects_file_outside_library() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("paper.txt");
        std::fs::write(&outside_file, "hello").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: SearchScope::File { path: outside_file },
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let err = AppContext::prepare_search_query(
            query,
            vec!["txt".to_string()],
            &[root.path().canonicalize().unwrap()],
        )
        .unwrap_err();
        assert!(err.contains("Search file is not in the library"));
    }

    #[test]
    fn test_prepare_search_query_rejects_root_outside_library() {
        let library = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let query = SearchQuery {
            pattern: "secret".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: outside.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let err = AppContext::prepare_search_query(
            query,
            vec!["txt".to_string()],
            &[library.path().canonicalize().unwrap()],
        )
        .unwrap_err();
        assert!(err.contains("not in the library"));
    }

    #[test]
    fn test_prepare_search_query_allows_nested_root_inside_library() {
        let library = tempdir().unwrap();
        let nested = library.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let query = SearchQuery {
            pattern: "allowed".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: nested.clone(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Grep,
            scope: SearchScope::Corpus,
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let prepared = AppContext::prepare_search_query(
            query,
            vec!["txt".to_string()],
            &[library.path().canonicalize().unwrap()],
        )
        .unwrap();
        assert_eq!(prepared.root, nested.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn test_start_search_semantic_missing() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": dir.path() }),
        )
        .await
        .unwrap();

        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let res = ctx.clone().start_search(query).await;
        match res {
            Err(e) => assert!(e.contains("No semantic index found")),
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_get_worker_status() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        let status = ctx.get_worker_status();
        assert_eq!(status.active, false);
    }

    #[tokio::test]
    async fn test_spawn_background_tasks() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, rx, loop_fut) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.spawn_background_tasks(rx, loop_fut);
        // Just verify it doesn't panic and tasks are spawned
    }

    #[tokio::test]
    async fn test_restore_state_no_index() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // No index on disk, no settings
        ctx.clone().restore_state().await;
        assert!(!ctx.is_semantic_ready());
    }

    #[tokio::test]
    async fn test_restore_state_invalid_settings_json_returns_early() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        std::fs::write(&settings_path, "{not valid json").unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir,
            },
            emitter,
        );

        ctx.clone().restore_state().await;

        assert!(ctx.embedder.lock().is_none());
        assert!(ctx.index.lock().lock().unwrap().is_none());
        assert!(ctx.directory_watcher.lock().is_none());
    }

    #[tokio::test]
    async fn test_start_build_index_already_in_progress() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let res = ctx
            .start_build_index(
                "root".to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_start_build_index_root_not_found() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let res = ctx
            .start_build_index(
                "/non/existent/path/for/sure/12345".to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Index root not found"));
        assert!(events.lock().unwrap().iter().any(|(name, payload)| {
            name == "embed-error"
                && payload["operation"] == "Build"
                && payload["message"]
                    .as_str()
                    .is_some_and(|msg| msg.contains("Index root not found"))
        }));
    }

    #[tokio::test]
    async fn test_start_search_semantic_build_in_progress() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": dir.path() }),
        )
        .await
        .unwrap();

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let query = SearchQuery {
            mode: SearchMode::Semantic,
            root: dir.path().to_path_buf(),
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let res = ctx.start_search(query).await;
        match res {
            Err(e) => assert!(e.contains("Semantic index is currently being built")),
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_related_documents_requires_current_semantic_root() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(&data_dir),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        let embedder = Arc::new(MockEmbedder {
            dimension: 2,
            ..MockEmbedder::default()
        });
        let model_id = embedder.model_id().to_string();
        *ctx.embedder.lock() = Some(embedder);
        let indexed_root = dir.path().join("indexed");
        let requested_root = dir.path().join("requested");
        std::fs::create_dir_all(&indexed_root).unwrap();
        std::fs::create_dir_all(&requested_root).unwrap();
        let requested_file = requested_root.join("source.txt");
        std::fs::write(&requested_file, "source").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": requested_root.clone() }),
        )
        .await
        .unwrap();
        let idx = SemanticIndex::create(
            &data_dir,
            &model_id,
            2,
            EmbeddingEngine::Candle,
            Some(&indexed_root),
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

        let err = ctx
            .clone()
            .related_documents(RelatedDocumentsQuery {
                root: requested_root,
                path: requested_file,
                scope: SearchScope::Corpus,
                limit: Some(8),
                collection_id: None,
            })
            .await
            .unwrap_err();

        assert!(err.contains("Semantic index is not ready"));
    }

    #[tokio::test]
    async fn test_related_documents_rejects_root_outside_library() {
        let (dir, ctx) = test_ctx();
        let library = dir.path().join("library");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let source = outside.join("source.txt");
        std::fs::write(&source, "source").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": library }),
        )
        .await
        .unwrap();

        let err = ctx
            .related_documents(RelatedDocumentsQuery {
                root: outside,
                path: source,
                scope: SearchScope::Corpus,
                limit: None,
                collection_id: None,
            })
            .await
            .unwrap_err();

        assert!(err.contains("not in the library"));
    }

    #[tokio::test]
    async fn test_citation_links_rejects_root_outside_library() {
        let (dir, ctx) = test_ctx();
        let library = dir.path().join("library");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let source = outside.join("source.txt");
        std::fs::write(&source, "source").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": library }),
        )
        .await
        .unwrap();

        let err = ctx
            .citation_links(CitationLinksQuery {
                root: outside,
                path: source,
            })
            .await
            .unwrap_err();

        assert!(err.contains("not in the library"));
    }

    #[tokio::test]
    async fn test_citation_links_empty_when_anchor_has_no_doi() {
        let (dir, ctx) = test_ctx();
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        let source = library.join("source.txt");
        std::fs::write(&source, "source").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": library }),
        )
        .await
        .unwrap();

        let links = ctx
            .clone()
            .citation_links(CitationLinksQuery {
                root: library,
                path: source,
            })
            .await
            .unwrap();

        assert!(links.references.is_empty());
        assert!(links.cited_by.is_empty());
        assert!(links.all_references.is_empty());
    }

    #[tokio::test]
    async fn test_citation_links_resolves_library_edges() {
        let (dir, ctx) = test_ctx();
        let library = std::fs::canonicalize(dir.path()).unwrap().join("library");
        std::fs::create_dir_all(&library).unwrap();
        let source = library.join("source.txt");
        let cited = library.join("cited.txt");
        std::fs::write(
            &source,
            "References\nSmith (2024). Exact cited work. https://doi.org/10.1000/CITED.LONG.\n",
        )
        .unwrap();
        std::fs::write(&cited, "cited").unwrap();

        let cache = ctx.metadata_cache().expect("cache opens");
        let id = FileIdentity {
            size_bytes: 1,
            modified_at_ms: 1,
        };
        {
            let guard = cache.lock().unwrap();
            guard
                .upsert(
                    &source,
                    id,
                    &DocumentMetadata {
                        doi: Some("10.1000/source".into()),
                        ..DocumentMetadata::default()
                    },
                    wilkes_core::metadata::cache::MetadataSource::File,
                )
                .unwrap();
            guard
                .upsert(
                    &cited,
                    id,
                    &DocumentMetadata {
                        doi: Some("10.1000/cited.long".into()),
                        ..DocumentMetadata::default()
                    },
                    wilkes_core::metadata::cache::MetadataSource::File,
                )
                .unwrap();
            guard
                .replace_citations(
                    "10.1000/source",
                    &[
                        "10.1000/cited".into(),
                        "10.1000/cited.long".into(),
                        "10.1000/missing".into(),
                    ],
                )
                .unwrap();
        }

        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": library }),
        )
        .await
        .unwrap();

        let links = ctx
            .clone()
            .citation_links(CitationLinksQuery {
                root: library.clone(),
                path: source.clone(),
            })
            .await
            .unwrap();

        assert_eq!(
            links
                .references
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            vec![cited.clone()]
        );
        assert!(links.cited_by.is_empty());
        assert_eq!(
            links
                .all_references
                .iter()
                .map(|reference| (reference.doi.as_str(), reference.citation_line.as_deref(),))
                .collect::<Vec<_>>(),
            vec![
                ("10.1000/cited", None),
                (
                    "10.1000/cited.long",
                    Some("Smith (2024). Exact cited work. https://doi.org/10.1000/CITED.LONG."),
                ),
                ("10.1000/missing", None),
            ]
        );

        // The reverse direction resolves from the same stored edge.
        let reverse = ctx
            .citation_links(CitationLinksQuery {
                root: library,
                path: cited,
            })
            .await
            .unwrap();
        assert_eq!(
            reverse
                .cited_by
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>(),
            vec![source]
        );
        assert!(reverse.references.is_empty());
        assert!(reverse.all_references.is_empty());
    }

    #[tokio::test]
    async fn test_related_documents_returns_index_results() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.txt");
        let related = root.join("related.txt");
        std::fs::write(&source, "source").unwrap();
        std::fs::write(&related, "related").unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(&data_dir),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        let embedder = Arc::new(MockEmbedder {
            dimension: 2,
            ..MockEmbedder::default()
        });
        let model_id = embedder.model_id().to_string();
        *ctx.embedder.lock() = Some(embedder);
        let mut idx = SemanticIndex::create(
            &data_dir,
            &model_id,
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        let chunk = |path: &Path, text: &str| wilkes_core::embed::index::chunk::Chunk {
            file_path: path.to_path_buf(),
            text: text.to_string(),
            byte_range: ByteRange {
                start: 0,
                end: text.len(),
            },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
        };
        idx.write_file(wilkes_core::embed::index::db::PreparedFile {
            full_text: String::new(),
            path: source.clone(),
            chunks: vec![(chunk(&source, "source"), vec![1.0, 0.0])],
        })
        .unwrap();
        idx.write_file(wilkes_core::embed::index::db::PreparedFile {
            full_text: String::new(),
            path: related.clone(),
            chunks: vec![(chunk(&related, "related"), vec![0.9, 0.1])],
        })
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": root.clone() }),
        )
        .await
        .unwrap();

        let docs = ctx
            .related_documents(RelatedDocumentsQuery {
                root,
                path: source,
                scope: SearchScope::Corpus,
                limit: Some(8),
                collection_id: None,
            })
            .await
            .unwrap();

        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].entry.path, std::fs::canonicalize(related).unwrap());
    }

    #[tokio::test]
    async fn test_start_download_model_already_in_progress() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        // Mock a task in progress
        let cancel = CancellationToken::new();
        let join = tokio::spawn(async { Ok(()) });
        *ctx.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join,
        });

        let res = ctx
            .start_download_model(SelectedEmbedder {
                engine: EmbeddingEngine::Candle,
                model: EmbedderModel("m".to_string()),
                dimension: 384,
            })
            .await;

        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "A build is already in progress.");
    }

    #[tokio::test]
    async fn test_document_operations_allow_files_outside_data_dir() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();

        let preview = ctx.open_file(outside.clone()).await.unwrap();
        match preview {
            PreviewData::Text { content, .. } => assert_eq!(content, "secret"),
            _ => panic!("Expected text preview"),
        }

        assert_eq!(
            ctx.get_file_metadata(outside.clone()).await.unwrap(),
            DocumentMetadata::default()
        );
        assert_eq!(
            wilkes_agent::search::SearchService::document_metadata(
                Arc::clone(&ctx),
                outside.clone(),
            )
            .await
            .unwrap(),
            DocumentMetadata::default()
        );
        assert_eq!(
            ctx.resolve_file_metadata(outside).await.unwrap(),
            DocumentMetadata::default()
        );
    }

    #[tokio::test]
    async fn test_start_search_semantic_root_mismatch() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let embedder = Arc::new(MockEmbedder::default());
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();

        // Mock an embedder and index
        *ctx.embedder.lock() = Some(embedder);

        // Create an index on disk so we can open it
        let root1 = dir.path().join("root1");
        std::fs::create_dir_all(&root1).unwrap();
        let idx = SemanticIndex::create(
            &data_dir,
            &model_id,
            dimension,
            EmbeddingEngine::Candle,
            Some(&root1),
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

        // Search in a different root
        let root2 = dir.path().join("root2");
        std::fs::create_dir_all(&root2).unwrap();
        std::fs::write(root2.join("file.txt"), "hello").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": root2.clone() }),
        )
        .await
        .unwrap();
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root2.clone(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        // This should trigger a background reindex because root2 != root1
        let err = match ctx.clone().start_search(query).await {
            Err(err) => err,
            Ok(_) => panic!("expected semantic root mismatch"),
        };
        assert!(err.contains("Semantic index is not ready"));

        let mut saw_reindex = false;
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
            let events_guard = events.lock().unwrap();
            if events_guard
                .iter()
                .any(|e| e.0 == "manager-event" && e.1 == serde_json::json!("Reindexing"))
            {
                saw_reindex = true;
                break;
            }
        }
        assert!(saw_reindex);
    }

    #[tokio::test]
    async fn test_start_search_semantic_root_mismatch_without_indexed_root_reindexes() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let embedder = Arc::new(MockEmbedder::default());
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();
        *ctx.embedder.lock() = Some(embedder);

        let idx = SemanticIndex::create(
            &data_dir,
            &model_id,
            dimension,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(idx)));

        let root2 = dir.path().join("root2");
        std::fs::create_dir_all(&root2).unwrap();
        std::fs::write(root2.join("file.txt"), "hello").unwrap();
        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": root2.clone() }),
        )
        .await
        .unwrap();
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root2.clone(),
            max_results: 0,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 2,
            mode: SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let err = match ctx.clone().start_search(query).await {
            Err(err) => err,
            Ok(_) => panic!("expected semantic root mismatch"),
        };
        assert!(err.contains("Semantic index is not ready"));

        let mut saw_reindex = false;
        for _ in 0..20 {
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
            let events_guard = events.lock().unwrap();
            if events_guard
                .iter()
                .any(|e| e.0 == "manager-event" && e.1 == serde_json::json!("Reindexing"))
            {
                saw_reindex = true;
                break;
            }
        }
        assert!(saw_reindex);

        let events_guard = events.lock().unwrap();
        assert!(
            events_guard
                .iter()
                .filter(|e| e.0 == "manager-event" && e.1 == serde_json::json!("Reindexing"))
                .count()
                >= 1
        );
    }

    #[tokio::test]
    async fn test_restore_state_model_mismatch() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Write settings with model A
        let settings = Settings {
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    model: EmbedderModel("model-A".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        tokio::fs::write(&settings_path, serde_json::to_string(&settings).unwrap())
            .await
            .unwrap();

        // Write index status with model B
        SemanticIndex::create(&data_dir, "model-B", 1, EmbeddingEngine::Candle, None).unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path.clone(),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        ctx.clone().restore_state().await;

        // Should have cleared the stale index reference in settings
        let updated_settings = ctx.get_settings().await;
        assert_eq!(updated_settings.semantic.enabled, false);
        assert!(updated_settings.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_update_semantic_settings_error() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("invalid.json");
        std::fs::write(&settings_path, "{ broken }").unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: PathBuf::from("data"),
            },
            emitter,
        );

        ctx.update_semantic_settings(|s| s).await;
    }

    #[tokio::test]
    async fn test_restore_state_model_mismatch_clears_settings() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");

        let settings = Settings {
            search_prefer_semantic: true,
            semantic: SemanticSettings {
                selected: SelectedEmbedder {
                    model: EmbedderModel("model-A".to_string()),
                    ..Default::default()
                },
                enabled: true,
                index_path: Some(data_dir.join("semantic_index.db")),
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        wilkes_core::embed::index::SemanticIndex::create(
            &data_dir,
            "model-B",
            1,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        ctx.clone().restore_state().await;
        let updated = ctx.get_settings().await;
        assert_eq!(updated.semantic.enabled, false);
        assert!(updated.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_worker_operations() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        let status = ctx.get_worker_status();
        assert!(!status.active);

        ctx.kill_worker();

        // set_worker_timeout sends to the manager loop, which is running, so it should succeed.
        let res = ctx.set_worker_timeout(10).await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_delete_index_operation() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: data_dir.clone(),
            },
            emitter,
        );

        // Create a fake index
        std::fs::write(data_dir.join("semantic_index.db"), "fake db").unwrap();
        // Note: delete_index currently only removes the .db file.
        std::fs::write(
            data_dir.join("semantic_index.status.json"),
            r#"{"model_id": "m", "dimension": 1, "engine": "Candle"}"#,
        )
        .unwrap();

        ctx.delete_index(None).await.unwrap();
        assert!(!data_dir.join("semantic_index.db").exists());

        let settings = ctx.get_settings().await;
        assert!(settings.semantic.index_path.is_none());
    }

    #[tokio::test]
    async fn test_get_index_status_not_found() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        let res = ctx.get_index_status(None).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_update_settings_patch() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        let patch = serde_json::json!({
            "supported_extensions": ["rs", "txt"]
        });
        ctx.update_settings(patch).await.unwrap();

        let settings = ctx.get_settings().await;
        assert_eq!(settings.supported_extensions, vec!["rs", "txt"]);
    }

    #[tokio::test]
    async fn test_update_semantic_settings_patch() {
        let dir = tempdir().unwrap();
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        ctx.update_semantic_settings(|mut s| {
            s.chunk_size = 1234;
            s
        })
        .await;

        let settings = ctx.get_settings().await;
        assert_eq!(settings.semantic.chunk_size, 1234);
    }

    #[tokio::test]
    async fn test_cancel_embed_operation() {
        let dir = tempdir().unwrap();
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );
        std::fs::write(dir.path().join("file.txt"), "hello").unwrap();

        // Start a fake build index
        ctx.clone()
            .start_build_index(
                dir.path().to_string_lossy().to_string(),
                SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
            )
            .await
            .unwrap();

        // Immediately cancel
        ctx.cancel_embed().await;

        assert!(ctx.embed_task.lock().is_none());
        assert!(!ctx.get_worker_status().active);
    }

    #[tokio::test]
    async fn test_start_download_model_error() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths {
                python_path: PathBuf::from("p"),
                python_package_dir: PathBuf::from("pkg"),
                requirements_path: PathBuf::from("r"),
                venv_dir: PathBuf::from("v"),
                worker_bin: PathBuf::from("w"),
                data_dir: dir.path().to_path_buf(),
            },
            emitter,
        );
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "hello").unwrap();
        ctx.update_settings(serde_json::json!({ "last_directory": root }))
            .await
            .unwrap();

        // Requesting download of non-existent model should eventually emit error
        ctx.clone()
            .start_download_model(SelectedEmbedder {
                engine: EmbeddingEngine::Fastembed,
                model: EmbedderModel("invalid-model".to_string()),
                dimension: 384,
            })
            .await
            .unwrap();

        // Wait for task to fail
        let mut found = false;
        for _ in 0..10 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let events_guard = events.lock().unwrap();
            if events_guard.iter().any(|e| e.0 == "embed-error") {
                found = true;
                break;
            }
        }
        assert!(found);
    }

    #[tokio::test]
    async fn test_restore_state_complex() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Seed settings.json with a different model
        let mut initial_settings = wilkes_core::types::Settings::default();
        initial_settings.semantic.selected.model = EmbedderModel("m1".to_string());
        std::fs::write(
            &settings_path,
            serde_json::to_string(&initial_settings).unwrap(),
        )
        .unwrap();

        // Create an index status file matching that model
        let index_dir = dir.path().join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let status = wilkes_core::types::IndexStatus {
            model_id: "m1".to_string(),
            engine: EmbeddingEngine::Candle,
            dimension: 128,
            indexed_files: 1,
            total_chunks: 10,
            built_at: Some(12345678),
            build_duration_ms: Some(1000),
            root_path: Some(dir.path().to_path_buf()),
            db_size_bytes: Some(1024),
        };
        std::fs::write(
            index_dir.join("status.json"),
            serde_json::to_string(&status).unwrap(),
        )
        .unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        // AppContext::new might have already called restore_state internally,
        // but let's be explicit and check if it sticks.
        ctx.clone().restore_state().await;

        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.selected.model.0, "m1");
    }

    #[tokio::test]
    async fn test_restore_state_open_index_fail() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a corrupted index path (a directory where a file should be)
        let index_path = data_dir.join("semantic_index.db");
        std::fs::create_dir(&index_path).unwrap();

        let settings = Settings {
            search_prefer_semantic: true,
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(index_path),
                selected: SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("m".to_string()),
                    dimension: 384,
                },
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        ctx.clone().restore_state().await;

        let updated = ctx.get_settings().await;
        assert_eq!(updated.semantic.enabled, false);
    }

    #[tokio::test]
    async fn test_update_semantic_settings_success() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("s.json");
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            settings_path.clone(),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        ctx.update_semantic_settings(|s| SemanticSettings { enabled: true, ..s })
            .await;

        let s = ctx.get_settings().await;
        assert_eq!(s.semantic.enabled, true);
        assert!(settings_path.exists());
    }

    #[tokio::test]
    async fn test_worker_status_timeout_and_delete_index_wrapper() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });

        let (ctx, _event_rx, loop_fut) = AppContext::new(
            data_dir.clone(),
            settings_path,
            WorkerPaths::resolve(dir.path()),
            emitter,
        );
        let _loop_handle = tokio::spawn(loop_fut);

        let status = ctx.get_worker_status();
        assert!(!status.active);
        assert_eq!(status.timeout_secs, 300);

        ctx.set_worker_timeout(123).await.unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let status = ctx.get_worker_status();
        assert_eq!(status.timeout_secs, 123);

        let index = SemanticIndex::create(
            &data_dir,
            "test-model",
            3,
            EmbeddingEngine::Candle,
            Some(dir.path()),
        )
        .unwrap();
        drop(index);

        let index_path = data_dir.join("semantic_index.db");
        assert!(index_path.exists());

        let status = ctx.get_index_status(None).await.unwrap();
        assert_eq!(status.model_id, "test-model");

        ctx.delete_index(None).await.unwrap();
        assert!(!index_path.exists());
    }

    #[tokio::test]
    async fn test_prepare_build_index_rejects_file_root() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("file.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("settings.json"),
            WorkerPaths::resolve(dir.path()),
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );

        let err = ctx
            .prepare_build_index(
                file_path.to_str().unwrap(),
                &SelectedEmbedder::default_for(EmbeddingEngine::Candle),
            )
            .await
            .unwrap_err();
        assert!(err.contains("is not a directory"));
    }

    #[test]
    fn test_restore_state_needs_reset_on_none_db() {
        let mut settings = Settings::default();
        settings.semantic.enabled = true;
        assert!(AppContext::restore_state_needs_reset(&settings, None));

        settings.semantic.enabled = false;
        settings.semantic.index_path = Some(PathBuf::from("any"));
        assert!(AppContext::restore_state_needs_reset(&settings, None));

        settings.semantic.index_path = None;
        assert!(!AppContext::restore_state_needs_reset(&settings, None));
    }

    #[tokio::test]
    async fn test_shutdown_twice() {
        let (_dir, ctx) = test_ctx();
        ctx.shutdown().await;
        ctx.shutdown().await; // Should return early
    }

    #[tokio::test]
    async fn test_spawn_download_model_error() {
        let dir = tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = Arc::new(MockEmitter {
            events: events.clone(),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            dir.path().to_path_buf(),
            dir.path().join("s.json"),
            WorkerPaths::resolve(dir.path()),
            emitter,
        );

        let plan = DownloadModelPlan {
            device: "cpu".to_string(),
        };
        let selected = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("invalid".to_string()),
            dimension: 0,
        };

        let join = ctx.spawn_download_model_task(plan, selected);
        let _ = join.await;

        let events_guard = events.lock().unwrap();
        assert!(events_guard.iter().any(|(name, _)| name == "embed-error"));
    }

    #[tokio::test]
    async fn test_clear_restore_state_settings_direct() {
        let (_dir, ctx) = test_ctx();
        ctx.update_settings(serde_json::json!({ "semantic": { "enabled": true } }))
            .await
            .unwrap();
        assert!(ctx.settings().await.semantic.enabled);

        ctx.clear_restore_state_settings().await;
        assert!(!ctx.settings().await.semantic.enabled);
    }
}
