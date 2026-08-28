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
use wilkes_core::embed::index::db::{
    ChunkAccumulation, ManagedChunkData, ManagedDocumentData, TopicChunkData,
    TopicCoveragePrototype,
};
use wilkes_core::embed::index::semantic_updater::process_directory_change;
use wilkes_core::embed::index::SemanticIndex;
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
    MetadataSourcePreference, NewBookmark, NewSmartCollection, NewTag, OmittedFileReason,
    PreviewData, RelatedDocument, RelatedDocumentsQuery, SearchDocument, SearchLogEntry,
    SearchMode, SearchQuery, SearchScope, SelectedEmbedder, SemanticSettings, Settings,
    SmartCollection, Tag, TopicLibraryCoverage, UpdateSmartCollection, UpdateTag,
};
use wilkes_core::types::{
    GenerationSettings, GenerationStreamEvent, GenerationTask, GeneratorDescriptor,
};
use wilkes_core::worker::manager::{
    ManagerCommand, ManagerEvent, WorkerManager, WorkerPaths, WorkerStatus,
};

use crate::commands::search::{start_search, SearchHandle};
use crate::commands::settings::{get_scoped_settings, update_scoped_settings};
#[cfg(test)]
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

    async fn max_search_file_size(self: Arc<Self>) -> u64 {
        self.get_settings().await.max_file_size
    }

    fn is_read_only(&self) -> bool {
        AppContext::is_read_only(self)
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

struct PendingManagedOperation<'a>(&'a AtomicU64);

impl<'a> PendingManagedOperation<'a> {
    fn new(counter: &'a AtomicU64) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for PendingManagedOperation<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Which side of a retrieval pair a text is.
///
/// An asymmetric model — E5, BGE, arctic-embed — is trained with a prefix on
/// one side or both, and applying the wrong one is not a small loss: measured
/// on a 6,600-record corpus, the same model placed the right answer at rank 52
/// with the query prefix and rank 1792 without it, while every similarity rose
/// (Underdog, ACQUISITION §12i). The role is therefore never a request field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmbedRole {
    Query,
    Passage,
}

/// One probe of a managed chunk search: a vector the caller already holds, or
/// the text it wants searched for.
///
/// Text is the better form where the caller has a choice. It is one round trip
/// rather than two, and it is the only form under which the query role can
/// reach the embedder at all.
#[derive(Clone, Debug)]
pub enum ManagedSearchProbeInput {
    Vector(Vec<f32>),
    Text(String),
}

/// Result of `embed_texts`: vectors from the same model the index uses,
/// with the identity consumers pin against.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EmbeddedTexts {
    pub engine: String,
    pub model_id: String,
    pub dimension: usize,
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedEmbeddedTexts {
    pub embedding_space_id: String,
    pub engine: String,
    pub model_id: String,
    pub dimension: usize,
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkExport {
    pub chunk_ref: ChunkRef,
    pub ordinal: usize,
    pub text: String,
    pub text_sha256: String,
    pub byte_range: wilkes_core::types::ByteRange,
    pub origin: wilkes_core::types::SourceOrigin,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedEmbeddingWork {
    pub chunks: usize,
    pub reused: usize,
    pub computed: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedDocumentExport {
    pub corpus_id: String,
    pub source_sha256: String,
    pub source_byte_len: u64,
    pub media_type: String,
    pub snapshot_id: String,
    pub rendition_id: String,
    pub extraction_recipe_id: String,
    pub extracted_content_sha256: String,
    pub chunk_count: usize,
    pub embedding_space_id: String,
    pub engine: String,
    pub model_id: String,
    pub dimension: usize,
    pub passage_input_recipe: String,
    pub outline: Vec<ExportedOutlineEntry>,
    /// What this document's extraction had to decide for itself — which pages
    /// clustered into a body column and which were too ambiguous to reorder,
    /// what was removed as furniture, how the wrap hyphens resolved.
    pub extraction: wilkes_core::types::ExtractionDiagnostics,
    pub chunks: Vec<ManagedChunkExport>,
    pub embedding_work: ManagedEmbeddingWork,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagedCorpusBackup {
    pub format: String,
    pub corpus_id: String,
    pub embedding_space_id: String,
    pub path: String,
    pub file_count: usize,
    pub byte_len: u64,
    pub files: Vec<ManagedBackupFile>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ManagedBackupFile {
    pub path: String,
    pub byte_len: u64,
    pub sha256: String,
}

pub(crate) fn backup_managed_directory(
    data_dir: &Path,
    corpus_id: &str,
    expected_embedding_space_id: &str,
    destination: &Path,
) -> anyhow::Result<ManagedCorpusBackup> {
    backup_managed_directory_parts(
        data_dir,
        data_dir,
        corpus_id,
        expected_embedding_space_id,
        destination,
    )
}

fn backup_managed_directory_parts(
    canonical_data_dir: &Path,
    projection_data_dir: &Path,
    corpus_id: &str,
    expected_embedding_space_id: &str,
    destination: &Path,
) -> anyhow::Result<ManagedCorpusBackup> {
    anyhow::ensure!(!destination.exists(), "backup destination already exists");
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("backup destination has no parent"))?;
    std::fs::create_dir_all(parent)?;

    let index = SemanticIndex::open_for_maintenance(projection_data_dir)?;
    let actual_space = index.embedding_space_identity()?.id().0;
    anyhow::ensure!(
        actual_space == expected_embedding_space_id,
        "EMBEDDING_SPACE_MISMATCH: index={actual_space}, request={expected_embedding_space_id}"
    );
    drop(index);

    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("managed-corpus");
    let staging = parent.join(format!(".{leaf}.partial-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)?;
    let result = (|| -> anyhow::Result<ManagedCorpusBackup> {
        copy_managed_tree(
            &canonical_data_dir.join("managed_sources"),
            &staging.join("managed_sources"),
        )?;
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(canonical_data_dir.join("workspace.json"))?)?;
        if projection_data_dir != canonical_data_dir {
            let projection: serde_json::Value = serde_json::from_slice(&std::fs::read(
                projection_data_dir.join("workspace.json"),
            )?)?;
            manifest["semantic"] = projection["semantic"].clone();
        }
        std::fs::write(
            staging.join("workspace.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        let live_db = projection_data_dir.join("semantic_index.db");
        anyhow::ensure!(live_db.is_file(), "managed semantic index is absent");
        let snapshot_db = staging.join("semantic_index.db");
        let connection = rusqlite::Connection::open(&live_db)?;
        connection.execute(
            "VACUUM INTO ?1",
            [snapshot_db.to_string_lossy().to_string()],
        )?;

        let mut files = managed_backup_files(&staging)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let byte_len = files.iter().map(|file| file.byte_len).sum();
        let backup = ManagedCorpusBackup {
            format: "wilkes-managed-corpus-backup/v1".to_string(),
            corpus_id: corpus_id.to_string(),
            embedding_space_id: actual_space,
            path: destination.display().to_string(),
            file_count: files.len(),
            byte_len,
            files,
        };
        std::fs::write(
            staging.join("backup-manifest.json"),
            serde_json::to_vec_pretty(&backup)?,
        )?;
        std::fs::rename(&staging, destination)?;
        Ok(backup)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn copy_managed_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(source.is_dir(), "managed source directory is absent");
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_managed_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            anyhow::bail!(
                "managed backup refuses non-file entry {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

pub(crate) fn managed_backup_files(root: &Path) -> anyhow::Result<Vec<ManagedBackupFile>> {
    fn walk(root: &Path, directory: &Path, out: &mut Vec<ManagedBackupFile>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if file_type.is_file() {
                let relative = entry.path().strip_prefix(root)?.to_path_buf();
                let metadata = entry.metadata()?;
                out.push(ManagedBackupFile {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    byte_len: metadata.len(),
                    sha256: wilkes_core::embed::identity::sha256_file(&entry.path())?,
                });
            } else {
                anyhow::bail!(
                    "managed backup refuses non-file entry {}",
                    entry.path().display()
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedAccumulation {
    pub sum: Vec<f32>,
    pub member_count: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedAccumulations {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub groups: Vec<ManagedAccumulation>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkResolution {
    pub embedding_space_id: String,
    pub chunks: Vec<ManagedChunkExport>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ManagedSimilarityProbeRequest {
    pub vector: Vec<f32>,
    #[serde(default)]
    pub scope: Vec<ChunkRef>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedProbeSimilarity {
    pub nearest_chunk_ref: Option<ChunkRef>,
    pub similarity: Option<f32>,
    pub scope_mean: Option<f32>,
    pub scope_size: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkNearest {
    pub chunk_ref: ChunkRef,
    pub probe: usize,
    pub similarity: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkSimilarities {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub probes: Vec<ManagedProbeSimilarity>,
    pub chunks: Vec<ManagedChunkNearest>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkSearchHit {
    pub chunk_ref: ChunkRef,
    pub snapshot_id: String,
    pub rendition_id: String,
    pub ordinal: usize,
    pub similarity: f32,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedProbeSearch {
    pub hits: Vec<ManagedChunkSearchHit>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ManagedChunkSearch {
    pub embedding_space_id: String,
    pub dimension: usize,
    pub probes: Vec<ManagedProbeSearch>,
}

pub const MAX_MANAGED_SEARCH_PROBES: usize = 512;
pub const MAX_MANAGED_SEARCH_TOP_K: usize = 100;

/// Most groups one `chunk_centroids` request may name, and most chunk ids it
/// may name across all of them.
///
/// The reply is one vector per group, so the group cap bounds what comes back
/// (256 groups at 384 dimensions is under half a megabyte) and the id cap
/// bounds the index scan that produces it. Both are generous by the standard
/// of the question: a caller asking for more than a few hundred regions in one
/// request is building a projection of the index, which is what
/// `export_file_chunks` is for.
pub const MAX_CENTROID_GROUPS: usize = 256;
pub const MAX_CENTROID_CHUNK_IDS: usize = 4_096;

/// Result of `chunk_centroids`: one normalized mean per requested group, in
/// the order the groups were asked for.
///
/// `model_id` and `dimension` are the index's own — the identity of the model
/// that produced the vectors being averaged, not of whatever embedder happens
/// to be loaded — because a consumer storing these beside vectors of its own
/// refuses on a mismatch and needs the comparison to be about the same thing.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkCentroids {
    pub engine: String,
    pub model_id: String,
    pub dimension: usize,
    pub centroids: Vec<Vec<f32>>,
}

/// Most probes one `chunk_similarity` request may carry, and most chunk ids it
/// may name across the searched set and every scope together.
///
/// The reply is two scalars per probe and one per chunk, so neither cap is
/// about the size of what comes back — they bound the dot products, which is
/// `probes × chunks`. 512 × 8,192 is a few million multiply-adds, well under a
/// second, and a caller wanting more is measuring a library rather than a
/// document and should say so one document at a time.
pub const MAX_SIMILARITY_PROBES: usize = 512;
pub const MAX_SIMILARITY_CHUNK_IDS: usize = 8_192;

/// One probe of `chunk_similarity`: a vector in the index's space, and
/// optionally the chunk ids to average it over.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct SimilarityProbeRequest {
    pub vector: Vec<f32>,
    #[serde(default)]
    pub scope: Vec<i64>,
}

/// What one probe found: its nearest chunk among the searched set, and the
/// mean similarity over its own scope when it named one.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProbeSimilarity {
    pub nearest_chunk_id: Option<i64>,
    pub similarity: Option<f32>,
    pub scope_mean: Option<f32>,
    pub scope_size: usize,
}

/// What one searched chunk found: the probe it sits closest to, named by its
/// index in the request.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkNearest {
    pub chunk_id: i64,
    pub probe: usize,
    pub similarity: f32,
}

/// Result of `chunk_similarity`: both directions of one comparison, plus the
/// identity of the model whose stored vectors answered.
///
/// `model_id` and `dimension` travel for the reason they travel on
/// [`ChunkCentroids`]: a consumer comparing these numbers against readings
/// taken elsewhere has to be able to refuse rather than average across two
/// spaces. Here it is sharper still — the probes are the consumer's own
/// vectors, so a model mismatch makes every number in the reply a comparison
/// between two different embedders and nothing in the shape says so.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkSimilarities {
    pub engine: String,
    pub model_id: String,
    pub dimension: usize,
    pub probes: Vec<ProbeSimilarity>,
    pub chunks: Vec<ChunkNearest>,
}

/// One exported chunk: text, locators (byte range into the extracted text
/// plus resolved source origin), and the stored vector.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportedChunk {
    pub chunk_id: i64,
    pub ordinal: usize,
    pub text: String,
    pub byte_range: wilkes_core::types::ByteRange,
    pub origin: wilkes_core::types::SourceOrigin,
    pub embedding: Vec<f32>,
}

/// One entry of the document's declared table of contents, resolved to the
/// chunk the section starts at.
///
/// The resolution happens here because this is the only place that holds both
/// halves: the outline says "page 41" or "byte 90210", the export says which
/// chunk that is. A consumer given the raw locator would have to re-derive the
/// mapping from the chunk list it was just handed — the same answer, computed
/// twice, with two chances to disagree.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExportedOutlineEntry {
    pub title: String,
    pub level: u32,
    /// Ordinal of the first exported chunk at or after the entry's position.
    pub chunk_ordinal: usize,
    /// The locator as the document expressed it, kept for display.
    pub page: Option<u32>,
    pub byte_offset: Option<usize>,
    /// What established `byte_offset` — the rung of the resolution ladder that
    /// answered. A consumer segmenting by this outline needs it to know which
    /// boundaries are exact and which are snapped to a page.
    pub anchor: wilkes_core::types::OutlineAnchor,
}

/// Result of `export_file_chunks`, in extraction order. `model_id` is absent
/// when no embedder is currently loaded (the stored vectors remain valid for
/// the model that built the index).
///
/// `outline` is empty when the document declares no table of contents — a fact
/// about the document, not a failure: a plain `.txt` without headings has no
/// sections to report and says so.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileChunkExport {
    pub file_path: PathBuf,
    pub model_id: Option<String>,
    pub dimension: Option<usize>,
    pub outline: Vec<ExportedOutlineEntry>,
    pub chunks: Vec<ExportedChunk>,
}

/// A document's declared structure, independent of its semantic-index state.
///
/// Unlike [`FileChunkExport`], this deliberately carries the locators the
/// document expressed rather than resolving them to chunk ordinals. Reading a
/// PDF's bookmarks must not require an index or return embedding vectors.
#[derive(Clone, Debug, serde::Serialize)]
pub struct FileOutlineExport {
    pub file_path: PathBuf,
    pub outline: Vec<wilkes_core::types::OutlineEntry>,
    pub extraction: wilkes_core::types::ExtractionDiagnostics,
}

/// One document Wilkes serves under a library root, as a consumer that wants to
/// ingest it needs to see it.
///
/// `chunk_count` is the fact that decides whether the document can be exported
/// at all: it counts the passages the semantic index holds for this file under
/// this root, so zero means an export would come back empty. It is a count
/// rather than a flag because a consumer showing the file to a person has a
/// use for the size of what it is about to read, and a flag would have thrown
/// that away to say less.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LibraryFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub extension: String,
    pub modified_at_ms: Option<i64>,
    /// Title from Wilkes's metadata cache, absent until it has read the file.
    pub title: Option<String>,
    pub chunk_count: usize,
}

/// Result of `export_library_files`: one root's documents, ascending by path.
///
/// The root comes back canonicalised because that is the root the counts were
/// read against — a consumer that asked with a symlinked or relative path can
/// see which directory answered.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LibraryFileExport {
    pub root: PathBuf,
    pub files: Vec<LibraryFile>,
}

/// Most chunks one `export_chunk_text` request may name.
///
/// A bound rather than a preference: the endpoint exists so that displaying a
/// passage costs a passage, and a caller that can ask for a thousand chunks has
/// simply rebuilt the full export with extra steps. Generous enough for a long
/// section, small enough that no reply is a surprise.
pub const MAX_CHUNK_TEXT_IDS: usize = 64;

/// One chunk's text and locators, without the vector.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkText {
    pub chunk_id: i64,
    pub ordinal: usize,
    pub text: String,
    pub byte_range: wilkes_core::types::ByteRange,
    pub origin: wilkes_core::types::SourceOrigin,
}

/// Result of `export_chunk_text`, ascending by ordinal — reading order, not the
/// order the ids were asked for.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ChunkTextExport {
    pub file_path: PathBuf,
    pub chunks: Vec<ChunkText>,
}

/// Maps each outline entry onto the first exported chunk at or after its
/// position, dropping entries that resolve past the end of the document.
///
/// Ties are kept: a chapter and its first section legitimately begin at the
/// same chunk, and collapsing them would lose the chapter's title. Entries
/// that resolve nowhere — a bookmark pointing at a page whose text was not
/// extracted, which is what a scanned appendix looks like — are dropped, since
/// a section that starts nowhere is not a section.
trait OutlineChunk {
    fn outline_ordinal(&self) -> usize;
    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange;
    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin;
}

impl OutlineChunk for ExportedChunk {
    fn outline_ordinal(&self) -> usize {
        self.ordinal
    }

    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange {
        &self.byte_range
    }

    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin {
        &self.origin
    }
}

impl OutlineChunk for ManagedChunkData {
    fn outline_ordinal(&self) -> usize {
        self.ordinal
    }

    fn outline_byte_range(&self) -> &wilkes_core::types::ByteRange {
        &self.extraction_byte_range
    }

    fn outline_origin(&self) -> &wilkes_core::types::SourceOrigin {
        &self.origin
    }
}

/// Position first, page second.
///
/// A byte offset is where the heading is; a page is where the heading's page
/// starts, which for a heading halfway down a page is up to a page early. Both
/// are exported, so a consumer can see which one it got and how — the entry's
/// `anchor` says which rung of the resolution ladder answered.
fn resolve_outline<T: OutlineChunk>(
    outline: &[wilkes_core::types::OutlineEntry],
    chunks: &[T],
) -> Vec<ExportedOutlineEntry> {
    outline
        .iter()
        .filter_map(|entry| {
            let ordinal = match (entry.byte_offset, entry.page) {
                (Some(offset), _) => chunks
                    .iter()
                    .find(|chunk| chunk.outline_byte_range().end > offset)
                    .map(OutlineChunk::outline_ordinal),
                (None, Some(page)) => chunks
                    .iter()
                    .find(|chunk| match chunk.outline_origin() {
                        wilkes_core::types::SourceOrigin::PdfPage { page: at, .. } => *at >= page,
                        _ => false,
                    })
                    .map(OutlineChunk::outline_ordinal),
                (None, None) => None,
            }?;
            Some(ExportedOutlineEntry {
                title: entry.title.clone(),
                level: entry.level,
                chunk_ordinal: ordinal,
                page: entry.page,
                byte_offset: entry.byte_offset,
                anchor: entry.anchor,
            })
        })
        .collect()
}

/// Shared application state and lifecycle logic. Both the desktop (Tauri) and
/// the server (axum) create exactly one `Arc<AppContext>` and delegate all
/// business operations to it.
pub struct AppContext {
    /// Workspace-owned databases and conversation persistence.
    pub data_dir: PathBuf,
    /// Shared model downloads, Python environment, and other installation data.
    pub shared_data_dir: PathBuf,
    pub settings_path: PathBuf,
    pub workspace_path: PathBuf,
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
    /// Serializes analyzer loads against each other: each one reads 1.9 GB
    /// of weights into memory, and two at once would hold both.
    image_analyzer_load_lock: tokio::sync::Mutex<()>,
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
    managed_runtime_lock: tokio::sync::Mutex<()>,
    managed_import_lock: tokio::sync::Mutex<()>,
    managed_pending_builds: AtomicU64,
    managed_pending_imports: AtomicU64,
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
        let (worker_manager, event_rx, loop_fut) = WorkerManager::new(paths.clone());
        let (generate_manager, generate_event_rx, generate_loop_fut) = WorkerManager::new(paths);
        let bookmarks_path = data_dir.join("bookmarks.json");
        let ctx = Arc::new(Self {
            data_dir,
            shared_data_dir,
            bookmarks_path,
            settings_path,
            workspace_path,
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
            image_analyzer_load_lock: tokio::sync::Mutex::new(()),
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
            managed_runtime_lock: tokio::sync::Mutex::new(()),
            managed_import_lock: tokio::sync::Mutex::new(()),
            managed_pending_builds: AtomicU64::new(0),
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
        get_scoped_settings(&self.settings_path, &self.workspace_path)
            .await
            .unwrap_or_default()
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

    /// Embed arbitrary short strings with the same model the semantic index
    /// uses. Consumers (the Underdog sidecar client) pin the returned model
    /// id and dimension and treat any later mismatch as a hard error, so the
    /// response always names both.
    pub async fn embed_texts(&self, texts: Vec<String>) -> Result<EmbeddedTexts, String> {
        self.embed_texts_in_role(texts, EmbedRole::Passage).await
    }

    /// The two roles an asymmetric model distinguishes. Which one applies is
    /// decided by the endpoint a caller reached, never by a field it sets: a
    /// flag that can be set correctly can be set wrongly, and the caller is the
    /// party least able to know what the model was trained to expect.
    async fn embed_texts_in_role(
        &self,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<EmbeddedTexts, String> {
        if texts.is_empty() {
            return Err("No texts provided".to_string());
        }
        let embedder = self.embedder.lock().clone().ok_or_else(|| {
            "Semantic model unavailable. Build or restore the semantic index first.".to_string()
        })?;
        let engine = embedder.engine().as_str().to_string();
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();

        let expected = texts.len();
        let embedder_for_task = Arc::clone(&embedder);
        let vectors = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            match role {
                EmbedRole::Passage => embedder_for_task.embed_passages(&refs),
                EmbedRole::Query => embedder_for_task.embed_query(&refs),
            }
        })
        .await
        .map_err(|e| format!("Text embedding task panicked: {e}"))?
        .map_err(|e| format!("Could not embed texts: {e:#}"))?;

        if vectors.len() != expected {
            return Err(format!(
                "Embedder returned {} vectors for {expected} inputs",
                vectors.len()
            ));
        }
        for vector in &vectors {
            if vector.len() != dimension {
                return Err(format!(
                    "Text embedding dimension mismatch. Expected {dimension}, received {}",
                    vector.len()
                ));
            }
            if vector.iter().any(|value| !value.is_finite()) {
                return Err("Embedder returned a non-finite text vector".to_string());
            }
        }
        Ok(EmbeddedTexts {
            engine,
            model_id,
            dimension,
            vectors,
        })
    }

    /// Attach the immutable managed workspace configuration to a concrete
    /// embedder and index. This is idempotent and refuses any exact-space
    /// mismatch; it never rewrites the managed manifest.
    pub async fn ensure_managed_runtime(self: &Arc<Self>) -> Result<String, String> {
        let _pending = PendingManagedOperation::new(&self.managed_pending_builds);
        let _runtime_guard = self.managed_runtime_lock.lock().await;
        if let Some(embedder) = self.embedder.lock().clone() {
            let identity = embedder.embedding_space_identity();
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            if guard
                .as_ref()
                .is_some_and(|index| index.validate_embedding_space(&identity).is_ok())
            {
                return Ok(identity.id().0);
            }
        }
        let settings = self.settings().await;
        let selected = settings.semantic.selected.clone();
        let device = settings.semantic.device_for(selected.engine).to_string();
        let installer = dispatch::get_installer(
            selected.engine,
            selected.model.clone(),
            self.worker_manager.clone(),
            device,
        );
        let (progress_tx, _progress_rx) = mpsc::channel(8);
        installer
            .install(&self.shared_data_dir, progress_tx)
            .await
            .map_err(|error| format!("Could not install managed embedding model: {error:#}"))?;
        let shared_data_dir = self.shared_data_dir.clone();
        let installer_for_build = Arc::clone(&installer);
        let embedder =
            tokio::task::spawn_blocking(move || installer_for_build.build(&shared_data_dir))
                .await
                .map_err(|error| format!("Managed embedder task panicked: {error}"))?
                .map_err(|error| format!("Could not load managed embedder: {error:#}"))?;
        if embedder.dimension() != selected.dimension {
            return Err(format!(
                "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH: configured dimension {}, runtime dimension {}",
                selected.dimension,
                embedder.dimension()
            ));
        }
        let expected_identity = embedder.embedding_space_identity();
        let data_dir = self.data_dir.clone();
        let root = data_dir.join("managed_sources");
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();
        let expected_for_index = expected_identity.clone();
        let index = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            let mut index = if data_dir.join("semantic_index.db").exists() {
                SemanticIndex::open(&data_dir, &model_id, dimension)?
            } else {
                SemanticIndex::create_exact(&data_dir, &expected_for_index, Some(&root))?
            };
            index.validate_embedding_space(&expected_for_index)?;
            index.activate_root(&root)?;
            Ok::<_, anyhow::Error>(index)
        })
        .await
        .map_err(|error| format!("Managed index task panicked: {error}"))?
        .map_err(|error| format!("Could not open managed index: {error:#}"))?;

        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = Some(embedder);
        *self.index.lock() = Arc::new(Mutex::new(Some(index)));
        Ok(expected_identity.id().0)
    }

    fn sanitize_managed_source_name(path: &Path) -> String {
        let original = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source");
        let sanitized: String = original
            .chars()
            .map(|character| {
                if character.is_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if sanitized.is_empty() {
            "source".to_string()
        } else {
            sanitized
        }
    }

    fn retain_managed_snapshot(
        data_dir: &Path,
        source: &Path,
    ) -> anyhow::Result<(PathBuf, PathBuf, String)> {
        let source = std::fs::canonicalize(source)
            .map_err(|error| anyhow::anyhow!("External source cannot be read: {error}"))?;
        anyhow::ensure!(source.is_file(), "External source is not a regular file");
        let before = std::fs::metadata(&source)?;
        let managed_sources = data_dir.join("managed_sources");
        let temporary_dir = managed_sources.join(".imports");
        std::fs::create_dir_all(&temporary_dir)?;
        let temporary = temporary_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::copy(&source, &temporary)?;
        let after = std::fs::metadata(&source)?;
        let copied = std::fs::metadata(&temporary)?;
        let source_sha256 = wilkes_core::embed::identity::sha256_file(&temporary)?;
        let source_after_sha256 = wilkes_core::embed::identity::sha256_file(&source)?;
        if before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || copied.len() != before.len()
            || source_after_sha256 != source_sha256
        {
            let _ = std::fs::remove_file(&temporary);
            anyhow::bail!("SOURCE_CHANGED_DURING_IMPORT");
        }
        let snapshot_dir = managed_sources.join(&source_sha256);
        std::fs::create_dir_all(&snapshot_dir)?;

        // Identical bytes already retained under another original name reuse
        // that immutable copy; display provenance remains in the index row.
        let source_extension = source.extension().and_then(|value| value.to_str());
        let existing = std::fs::read_dir(&snapshot_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.is_file()
                    && path.extension().and_then(|value| value.to_str()) == source_extension
            });
        let snapshot = if let Some(existing) = existing {
            if wilkes_core::embed::identity::sha256_file(&existing)? != source_sha256 {
                let _ = std::fs::remove_file(&temporary);
                anyhow::bail!("DOCUMENT_INDEX_INCOMPLETE: retained snapshot digest mismatch");
            }
            let _ = std::fs::remove_file(&temporary);
            existing
        } else {
            let destination = snapshot_dir.join(Self::sanitize_managed_source_name(&source));
            match std::fs::rename(&temporary, &destination) {
                Ok(()) => destination,
                Err(_error) if destination.exists() => {
                    let destination_sha256 = wilkes_core::embed::identity::sha256_file(
                        &destination,
                    )
                    .map_err(|error| {
                        let _ = std::fs::remove_file(&temporary);
                        error
                    })?;
                    if destination_sha256 != source_sha256 {
                        let _ = std::fs::remove_file(&temporary);
                        anyhow::bail!(
                            "DOCUMENT_INDEX_INCOMPLETE: retained snapshot digest mismatch"
                        );
                    }
                    let _ = std::fs::remove_file(&temporary);
                    destination
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error.into());
                }
            }
        };
        let mut permissions = std::fs::metadata(&snapshot)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&snapshot, permissions)?;
        let relative = snapshot.strip_prefix(data_dir)?.to_path_buf();
        Ok((snapshot, relative, source_sha256))
    }

    /// Resolve a `wilkes_file` import through the same library confinement as
    /// ordinary exports. The returned canonical path is the only path the
    /// managed importer is allowed to open for this source variant.
    pub async fn authorize_managed_workspace_file(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<PathBuf, String> {
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Managed import root")?;
        let (path, _) = Self::canonicalize_supported_file(
            &root,
            &path,
            &settings.supported_extensions,
            "Managed import",
        )?;
        Self::ensure_path_in_library(&path, &library_roots, "Managed import file")?;
        Ok(path)
    }

    pub async fn import_managed_document(
        self: &Arc<Self>,
        corpus_id: String,
        idempotency_key: String,
        source_path: PathBuf,
        source_workspace: Option<Arc<AppContext>>,
        original_source_provenance: serde_json::Value,
    ) -> Result<ManagedDocumentExport, String> {
        let _pending = PendingManagedOperation::new(&self.managed_pending_imports);
        let _import_guard = self.managed_import_lock.lock().await;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err(
                "IDEMPOTENCY_KEY_CONFLICT: idempotency key must contain 1 to 256 bytes".to_string(),
            );
        }
        let settings = self.settings().await;
        let extension = source_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !settings
            .supported_extensions
            .iter()
            .any(|supported| supported.eq_ignore_ascii_case(extension))
        {
            return Err(format!("Unsupported managed import extension: {extension}"));
        }
        let metadata = std::fs::metadata(&source_path)
            .map_err(|error| format!("External source cannot be read: {error}"))?;
        if metadata.len() > settings.max_file_size {
            return Err(format!(
                "Managed import exceeds the configured {} byte file limit",
                settings.max_file_size
            ));
        }
        let (snapshot_path, relative_path, source_sha256) = {
            let data_dir = self.data_dir.clone();
            let source_path = source_path.clone();
            tokio::task::spawn_blocking(move || {
                Self::retain_managed_snapshot(&data_dir, &source_path)
            })
            .await
            .map_err(|error| format!("Snapshot task panicked: {error}"))?
            .map_err(|error| format!("Could not retain managed snapshot: {error:#}"))?
        };
        let expected_identity = {
            let guard = self.index.lock().clone();
            let guard = guard
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "MANAGED_WORKSPACE_NOT_FOUND: runtime is not ready".to_string())?
                .embedding_space_identity()
                .map_err(|error| error.to_string())?
        };
        let expected_space = expected_identity.id();
        let recipe = ExtractionRecipe::for_path(
            &snapshot_path,
            &wilkes_core::extract::production_registry(),
            settings.semantic.chunk_size,
            settings.semantic.chunk_overlap,
        );

        if let Some(existing) = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?
                .managed_document_for_import_key(&idempotency_key, &source_sha256, &recipe.id())
                .map_err(|error| error.to_string())?
        } {
            return Self::managed_export(
                corpus_id,
                existing,
                &expected_identity,
                snapshot_path,
                true,
            );
        }

        if let Some(existing) = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let existing = index
                .managed_document_for_path(&snapshot_path)
                .map_err(|error| error.to_string())?;
            if let Some(existing) = existing.as_ref() {
                index
                    .bind_managed_import_key(&idempotency_key, existing)
                    .map_err(|error| error.to_string())?;
            }
            existing
        } {
            return Self::managed_export(
                corpus_id,
                existing,
                &expected_identity,
                snapshot_path,
                true,
            );
        }

        let adopted = if let Some(source_context) = source_workspace {
            let source_index_arc = source_context.index.lock().clone();
            let source_guard = source_index_arc
                .lock()
                .map_err(|_| "Source semantic index lock was poisoned")?;
            source_guard
                .as_ref()
                .map(|source_index| {
                    source_index.verified_file_for_adoption(
                        &source_sha256,
                        &recipe,
                        &expected_space,
                        &snapshot_path,
                    )
                })
                .transpose()
                .map_err(|error| format!("Could not verify source rendition: {error:#}"))?
                .flatten()
        } else {
            None
        };

        let reused = adopted.is_some();
        let prepared = if let Some(prepared) = adopted {
            prepared
        } else {
            let embedder = self.embedder.lock().clone().ok_or_else(|| {
                "MANAGED_WORKSPACE_NOT_FOUND: managed embedder is unavailable".to_string()
            })?;
            let snapshot_for_task = snapshot_path.clone();
            let chunk_size = settings.semantic.chunk_size;
            let chunk_overlap = settings.semantic.chunk_overlap;
            tokio::task::spawn_blocking(move || {
                let extractors = wilkes_core::extract::production_registry();
                SemanticIndex::prepare_file(
                    &snapshot_for_task,
                    &extractors,
                    embedder.as_ref(),
                    chunk_size,
                    chunk_overlap,
                )
            })
            .await
            .map_err(|error| format!("Managed extraction task panicked: {error}"))?
            .map_err(|error| format!("Could not extract/embed managed snapshot: {error:#}"))?
        };
        let expected_chunks = prepared.chunks.len();
        if expected_chunks == 0 {
            return Err("DOCUMENT_INDEX_INCOMPLETE: document produced no chunks".to_string());
        }
        let managed = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let managed = index
                .write_file_with_recipe(
                    prepared,
                    &recipe,
                    Some(&relative_path),
                    Some(&original_source_provenance),
                    true,
                    reused,
                    Some(&idempotency_key),
                )
                .map_err(|error| format!("Could not publish managed rendition: {error:#}"))?;
            managed
        };
        if managed.source_sha256 != source_sha256 || managed.chunks.len() != expected_chunks {
            return Err("DOCUMENT_INDEX_INCOMPLETE: publication verification failed".to_string());
        }
        Self::managed_export(
            corpus_id,
            managed,
            &expected_identity,
            snapshot_path,
            reused,
        )
    }

    /// Materialize one embedding projection from the canonical admitted
    /// rendition. The immutable source, extracted text, chunk boundaries, and
    /// stable refs remain owned by `canonical`; this context computes only its
    /// model-specific vectors and projection rows.
    pub async fn import_managed_projection(
        self: &Arc<Self>,
        corpus_id: String,
        idempotency_key: String,
        canonical: &Arc<Self>,
        canonical_snapshot_path: PathBuf,
        original_source_provenance: serde_json::Value,
    ) -> Result<ManagedDocumentExport, String> {
        let _pending = PendingManagedOperation::new(&self.managed_pending_imports);
        let _import_guard = self.managed_import_lock.lock().await;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err(
                "IDEMPOTENCY_KEY_CONFLICT: idempotency key must contain 1 to 256 bytes".to_string(),
            );
        }
        let source_sha256 = wilkes_core::embed::identity::sha256_file(&canonical_snapshot_path)
            .map_err(|error| format!("Could not verify canonical managed snapshot: {error:#}"))?;
        let settings = self.settings().await;
        let recipe = ExtractionRecipe::for_path(
            &canonical_snapshot_path,
            &wilkes_core::extract::production_registry(),
            settings.semantic.chunk_size,
            settings.semantic.chunk_overlap,
        );
        let expected_identity = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "MANAGED_WORKSPACE_NOT_FOUND: runtime is not ready".to_string())?
                .embedding_space_identity()
                .map_err(|error| error.to_string())?
        };

        if let Some(existing) = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?
                .managed_document_for_import_key(&idempotency_key, &source_sha256, &recipe.id())
                .map_err(|error| error.to_string())?
        } {
            return Self::managed_export(
                corpus_id,
                existing,
                &expected_identity,
                canonical_snapshot_path,
                true,
            );
        }

        let mut prepared = {
            let index_arc = canonical.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Canonical semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| {
                    "MANAGED_WORKSPACE_NOT_FOUND: canonical index is unavailable".to_string()
                })?
                .managed_file_structure_for_reembedding(
                    &canonical_snapshot_path,
                    &canonical_snapshot_path,
                    &recipe,
                )
                .map_err(|error| format!("Could not read canonical rendition: {error:#}"))?
                .ok_or_else(|| {
                    "DOCUMENT_INDEX_INCOMPLETE: canonical snapshot is not admitted".to_string()
                })?
        };
        if prepared.chunks.is_empty() {
            return Err("DOCUMENT_INDEX_INCOMPLETE: document produced no chunks".to_string());
        }
        let embedder = self.embedder.lock().clone().ok_or_else(|| {
            "MANAGED_WORKSPACE_NOT_FOUND: managed embedder is unavailable".to_string()
        })?;
        let texts: Vec<&str> = prepared
            .chunks
            .iter()
            .map(|(chunk, _)| chunk.text.as_str())
            .collect();
        let embeddings = embedder
            .embed_passages(&texts)
            .map_err(|error| format!("Could not embed canonical rendition: {error:#}"))?;
        if embeddings.len() != prepared.chunks.len() {
            return Err(format!(
                "DOCUMENT_INDEX_INCOMPLETE: embedder returned {} vectors for {} canonical chunks",
                embeddings.len(),
                prepared.chunks.len()
            ));
        }
        for ((_, vector), embedding) in prepared.chunks.iter_mut().zip(embeddings) {
            *vector = embedding;
        }
        let expected_chunks = prepared.chunks.len();
        let managed = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            index
                .write_file_with_recipe(
                    prepared,
                    &recipe,
                    None,
                    Some(&original_source_provenance),
                    true,
                    false,
                    Some(&idempotency_key),
                )
                .map_err(|error| format!("Could not publish embedding projection: {error:#}"))?
        };
        if managed.source_sha256 != source_sha256 || managed.chunks.len() != expected_chunks {
            return Err("DOCUMENT_INDEX_INCOMPLETE: projection verification failed".to_string());
        }
        Self::managed_export(
            corpus_id,
            managed,
            &expected_identity,
            canonical_snapshot_path,
            false,
        )
    }

    /// The retained snapshots this managed corpus has admitted — what a
    /// projection must hold to be level with it.
    pub fn managed_admitted_sources(&self) -> Result<Vec<PathBuf>, String> {
        let index_arc = self.index.lock().clone();
        let guard = index_arc
            .lock()
            .map_err(|_| "Semantic index lock was poisoned")?;
        guard
            .as_ref()
            .ok_or_else(|| "Managed index unavailable".to_string())?
            .managed_admitted_source_paths()
            .map_err(|error| error.to_string())
    }

    pub fn managed_pending_operations(&self) -> (u64, u64) {
        (
            self.managed_pending_imports.load(Ordering::Acquire),
            self.managed_pending_builds.load(Ordering::Acquire),
        )
    }

    /// Writes one point-in-time, self-verifying managed-corpus directory.
    /// Imports and runtime replacement are excluded for the whole snapshot;
    /// immutable sources are copied under that lock and SQLite supplies the
    /// index snapshot through `VACUUM INTO`, so WAL/checkpoint timing cannot
    /// produce a database assembled from different instants.
    pub async fn backup_managed_corpus(
        &self,
        corpus_id: String,
        expected_embedding_space_id: String,
    ) -> Result<ManagedCorpusBackup, String> {
        let _import_guard = self.managed_import_lock.lock().await;
        let _runtime_guard = self.managed_runtime_lock.lock().await;
        let data_dir = self.data_dir.clone();
        let destination = self.shared_data_dir.join("managed_backups").join(format!(
            "{}-{}",
            corpus_id,
            uuid::Uuid::new_v4()
        ));
        tokio::task::spawn_blocking(move || {
            backup_managed_directory(
                &data_dir,
                &corpus_id,
                &expected_embedding_space_id,
                &destination,
            )
        })
        .await
        .map_err(|error| format!("Managed backup task panicked: {error}"))?
        .map_err(|error| format!("Could not back up managed corpus: {error:#}"))
    }

    /// Back up canonical membership with whichever embedding projection is
    /// currently selected by Underdog. Secondary projection workspaces do not
    /// own sources, so snapshotting one alone would create an unrestorable
    /// half-corpus.
    pub async fn backup_managed_corpus_projection(
        &self,
        projection: &Arc<Self>,
        corpus_id: String,
        expected_embedding_space_id: String,
    ) -> Result<ManagedCorpusBackup, String> {
        let _canonical_import_guard = self.managed_import_lock.lock().await;
        let _canonical_runtime_guard = self.managed_runtime_lock.lock().await;
        let _projection_import_guard = projection.managed_import_lock.lock().await;
        let _projection_runtime_guard = projection.managed_runtime_lock.lock().await;
        let canonical_data_dir = self.data_dir.clone();
        let projection_data_dir = projection.data_dir.clone();
        let destination = self.shared_data_dir.join("managed_backups").join(format!(
            "{}-{}",
            corpus_id,
            uuid::Uuid::new_v4()
        ));
        tokio::task::spawn_blocking(move || {
            backup_managed_directory_parts(
                &canonical_data_dir,
                &projection_data_dir,
                &corpus_id,
                &expected_embedding_space_id,
                &destination,
            )
        })
        .await
        .map_err(|error| format!("Managed backup task panicked: {error}"))?
        .map_err(|error| format!("Could not back up managed corpus: {error:#}"))
    }

    fn managed_export(
        corpus_id: String,
        document: ManagedDocumentData,
        embedding_identity: &wilkes_core::embed::EmbeddingSpaceIdentity,
        snapshot_path: PathBuf,
        reused: bool,
    ) -> Result<ManagedDocumentExport, String> {
        let source_byte_len = std::fs::metadata(&snapshot_path)
            .map_err(|error| format!("Could not read retained snapshot metadata: {error}"))?
            .len();
        let media_type = match snapshot_path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => "application/pdf",
            "md" | "markdown" => "text/markdown",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        }
        .to_string();
        let registry = wilkes_core::extract::production_registry();
        let declared_outline = wilkes_core::extract::document_outline(&snapshot_path, &registry)
            .map_err(|error| format!("Could not read retained document outline: {error:#}"))?;
        let outline = resolve_outline(&declared_outline.entries, &document.chunks);
        let chunk_count = document.chunks.len();
        Ok(ManagedDocumentExport {
            corpus_id,
            source_sha256: document.source_sha256,
            source_byte_len,
            media_type,
            snapshot_id: document.snapshot_id.0,
            rendition_id: document.rendition_id.0,
            extraction_recipe_id: document.extraction_recipe_id,
            extracted_content_sha256: document.extracted_content_sha256,
            chunk_count,
            embedding_space_id: embedding_identity.id().0,
            engine: embedding_identity.engine.as_str().to_string(),
            model_id: embedding_identity.model_id.clone(),
            dimension: embedding_identity.dimension,
            passage_input_recipe: embedding_identity.passage_input_recipe.clone(),
            outline,
            extraction: declared_outline.diagnostics,
            chunks: document
                .chunks
                .into_iter()
                .map(|chunk| ManagedChunkExport {
                    chunk_ref: chunk.chunk_ref,
                    ordinal: chunk.ordinal,
                    text: chunk.text,
                    text_sha256: chunk.text_sha256,
                    byte_range: chunk.extraction_byte_range,
                    origin: chunk.origin,
                })
                .collect(),
            embedding_work: ManagedEmbeddingWork {
                chunks: chunk_count,
                reused: if reused { chunk_count } else { 0 },
                computed: if reused { 0 } else { chunk_count },
            },
        })
    }

    pub async fn managed_embed_texts(
        &self,
        texts: Vec<String>,
    ) -> Result<ManagedEmbeddedTexts, String> {
        let embedded = self.embed_texts(texts).await?;
        let space = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?
                .embedding_space_id()
                .map_err(|error| error.to_string())?
        };
        Ok(ManagedEmbeddedTexts {
            embedding_space_id: space.0,
            engine: embedded.engine,
            model_id: embedded.model_id,
            dimension: embedded.dimension,
            vectors: embedded.vectors,
        })
    }

    pub async fn managed_accumulate(
        &self,
        groups: Vec<Vec<ChunkRef>>,
    ) -> Result<ManagedAccumulations, String> {
        if groups.is_empty() {
            return Err("Aggregate request names no groups".to_string());
        }
        let total: usize = groups.iter().map(Vec::len).sum();
        if groups.len() > MAX_CENTROID_GROUPS || total > MAX_CENTROID_CHUNK_IDS {
            return Err("Aggregate request exceeds the documented request cap".to_string());
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let space = index
                .embedding_space_id()
                .map_err(|error| error.to_string())?;
            let groups: Vec<ChunkAccumulation> = index
                .accumulate_chunk_refs(&groups)
                .map_err(|error| error.to_string())?;
            Ok(ManagedAccumulations {
                embedding_space_id: space.0,
                dimension: index.status().dimension,
                groups: groups
                    .into_iter()
                    .map(|group| ManagedAccumulation {
                        sum: group.sum,
                        member_count: group.member_count,
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Aggregate task panicked: {error}"))?
    }

    pub async fn managed_resolve_chunks(
        &self,
        refs: Vec<ChunkRef>,
    ) -> Result<ManagedChunkResolution, String> {
        if refs.is_empty() {
            return Err("Resolve request names no chunk refs".to_string());
        }
        if refs.len() > MAX_SIMILARITY_CHUNK_IDS {
            return Err("Resolve request exceeds the documented request cap".to_string());
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let chunks = index
                .managed_chunks_for_refs(&refs)
                .map_err(|error| error.to_string())?;
            Ok(ManagedChunkResolution {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                chunks: chunks
                    .into_iter()
                    .map(|chunk| ManagedChunkExport {
                        chunk_ref: chunk.chunk_ref,
                        ordinal: chunk.ordinal,
                        text: chunk.text,
                        text_sha256: chunk.text_sha256,
                        byte_range: chunk.extraction_byte_range,
                        origin: chunk.origin,
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Resolve task panicked: {error}"))?
    }

    pub async fn managed_chunk_similarity(
        &self,
        probes: Vec<ManagedSimilarityProbeRequest>,
        chunk_refs: Vec<ChunkRef>,
    ) -> Result<ManagedChunkSimilarities, String> {
        if probes.is_empty() {
            return Err("Similarity request names no probes".to_string());
        }
        let total = chunk_refs.len() + probes.iter().map(|probe| probe.scope.len()).sum::<usize>();
        if probes.len() > MAX_SIMILARITY_PROBES || total > MAX_SIMILARITY_CHUNK_IDS {
            return Err("Similarity request exceeds the documented request cap".to_string());
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let wanted: HashSet<ChunkRef> = chunk_refs
                .iter()
                .cloned()
                .chain(probes.iter().flat_map(|probe| probe.scope.iter().cloned()))
                .collect();
            let resolved = index
                .resolve_chunk_refs(&wanted)
                .map_err(|error| error.to_string())?;
            let reverse: HashMap<i64, ChunkRef> = resolved
                .iter()
                .map(|(stable_ref, rowid)| (*rowid, stable_ref.clone()))
                .collect();
            let searched: Vec<i64> = chunk_refs
                .iter()
                .map(|stable_ref| resolved[stable_ref])
                .collect();
            let probes: Vec<wilkes_core::embed::index::db::SimilarityProbe> = probes
                .into_iter()
                .map(|probe| wilkes_core::embed::index::db::SimilarityProbe {
                    vector: probe.vector,
                    scope: probe
                        .scope
                        .iter()
                        .map(|stable_ref| resolved[stable_ref])
                        .collect(),
                })
                .collect();
            let found = index
                .chunk_similarity(&probes, &searched)
                .map_err(|error| error.to_string())?;
            Ok(ManagedChunkSimilarities {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                dimension: index.status().dimension,
                probes: found
                    .probes
                    .into_iter()
                    .map(|probe| ManagedProbeSimilarity {
                        nearest_chunk_ref: probe
                            .nearest_chunk_id
                            .map(|rowid| reverse[&rowid].clone()),
                        similarity: probe.similarity,
                        scope_mean: probe.scope_mean,
                        scope_size: probe.scope_size,
                    })
                    .collect(),
                chunks: found
                    .chunks
                    .into_iter()
                    .map(|chunk| ManagedChunkNearest {
                        chunk_ref: reverse[&chunk.chunk_id].clone(),
                        probe: chunk.probe,
                        similarity: chunk.similarity,
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Similarity task panicked: {error}"))?
    }

    pub async fn managed_chunk_search(
        &self,
        probes: Vec<ManagedSearchProbeInput>,
        top_k: usize,
        min_similarity: f32,
    ) -> Result<ManagedChunkSearch, String> {
        if probes.is_empty() {
            return Err("Search request names no probes".to_string());
        }
        if probes.len() > MAX_MANAGED_SEARCH_PROBES
            || top_k == 0
            || top_k > MAX_MANAGED_SEARCH_TOP_K
        {
            return Err("Search request exceeds the documented request cap".to_string());
        }
        let probes = self.resolve_search_probes(probes).await?;
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let hits = index
                .managed_chunk_search(&probes, top_k, min_similarity)
                .map_err(|error| error.to_string())?;
            Ok(ManagedChunkSearch {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                dimension: index.status().dimension,
                probes: hits
                    .into_iter()
                    .map(|hits| ManagedProbeSearch {
                        hits: hits
                            .into_iter()
                            .map(|hit| ManagedChunkSearchHit {
                                chunk_ref: hit.chunk_ref,
                                snapshot_id: hit.snapshot_id.0,
                                rendition_id: hit.rendition_id.0,
                                ordinal: hit.ordinal,
                                similarity: hit.similarity,
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Search task panicked: {error}"))?
    }

    /// Turn a mixed probe list into vectors, embedding the text ones in the
    /// query role and keeping the caller's order.
    ///
    /// The texts go in one batch because they must land in one space: two
    /// embed calls are two chances for a model to be swapped between them.
    async fn resolve_search_probes(
        &self,
        probes: Vec<ManagedSearchProbeInput>,
    ) -> Result<Vec<Vec<f32>>, String> {
        let texts: Vec<String> = probes
            .iter()
            .filter_map(|probe| match probe {
                ManagedSearchProbeInput::Text(text) => Some(text.clone()),
                ManagedSearchProbeInput::Vector(_) => None,
            })
            .collect();
        if texts.is_empty() {
            return Ok(probes
                .into_iter()
                .map(|probe| match probe {
                    ManagedSearchProbeInput::Vector(vector) => vector,
                    ManagedSearchProbeInput::Text(_) => unreachable!("no text probes"),
                })
                .collect());
        }
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err("A text probe is empty and names nothing to search for".to_string());
        }
        let embedded = self.embed_texts_in_role(texts, EmbedRole::Query).await?;
        let mut embedded = embedded.vectors.into_iter();
        probes
            .into_iter()
            .map(|probe| match probe {
                ManagedSearchProbeInput::Vector(vector) => Ok(vector),
                ManagedSearchProbeInput::Text(_) => embedded
                    .next()
                    .ok_or_else(|| "Embedder returned fewer vectors than text probes".to_string()),
            })
            .collect()
    }

    /// The normalized mean of the stored vectors of named chunks, one mean per
    /// group.
    ///
    /// This is the whole of what a consumer needs when it wants to know *where
    /// in the corpus* a set of passages sits, and it is deliberately not a way
    /// to get the passages' vectors. `export_file_chunks` hands those out for
    /// ingestion — a consumer that must never re-extract has to receive what
    /// the index holds — but a consumer keeping a vector space of its own is
    /// better served by asking Wilkes for the number than by rebuilding the
    /// arithmetic on its side, where the mean, the normalisation and the
    /// dimension check would all become a second definition of a vector this
    /// index already knows how to make.
    ///
    /// Groups rather than a single set for the same reason `embed_texts` takes
    /// a list: a caller computing one region per concept has hundreds of them,
    /// and one request per region turns a scan into a stampede.
    ///
    /// Chunk ids the index does not hold are an error, not an omission — see
    /// `SemanticIndex::chunk_centroids`, which owns that rule.
    pub async fn chunk_centroids(&self, groups: Vec<Vec<i64>>) -> Result<ChunkCentroids, String> {
        if groups.is_empty() {
            return Err("Centroid request names no groups.".to_string());
        }
        if groups.len() > MAX_CENTROID_GROUPS {
            return Err(format!(
                "Centroid request names {} groups; {MAX_CENTROID_GROUPS} is the most one request \
                 may ask for.",
                groups.len(),
            ));
        }
        let total: usize = groups.iter().map(Vec::len).sum();
        if total > MAX_CENTROID_CHUNK_IDS {
            return Err(format!(
                "Centroid request names {total} chunk ids; {MAX_CENTROID_CHUNK_IDS} is the most \
                 one request may ask for.",
            ));
        }
        // The same refusal the chunk export makes: mid-rebuild, the ids in
        // flight belong to neither the old index nor the new one.
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before asking for centroids.",
        )?;

        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard.as_ref().ok_or_else(|| {
                "Semantic index unavailable. Build or restore the semantic index first.".to_string()
            })?;
            let status = index.status();
            let centroids = index
                .chunk_centroids(&groups)
                .map_err(|error| format!("Could not compute chunk centroids: {error:#}"))?;
            Ok(ChunkCentroids {
                engine: status.engine.as_str().to_string(),
                model_id: status.model_id,
                dimension: status.dimension,
                centroids,
            })
        })
        .await
        .map_err(|error| format!("Centroid task panicked: {error}"))?
    }

    /// How close a consumer's own vectors sit to named passages of this index,
    /// in both directions, plus a per-probe mean over a scope of the consumer's
    /// choosing.
    ///
    /// The sibling of [`Self::chunk_centroids`] and it exists for the same
    /// reason: a consumer that keeps its own vector space wants a *number*
    /// about this index, and the way to give it one without handing over the
    /// index's vectors is to do the arithmetic here. The centroid endpoint
    /// answers "where do these passages sit"; this one answers "how far is
    /// this from them", which is the question a coverage measurement asks in
    /// both directions at once.
    ///
    /// Probe vectors are used as given — see `SemanticIndex::chunk_similarity`,
    /// which owns that rule and explains why normalizing here would break the
    /// group-mean probe.
    pub async fn chunk_similarity(
        &self,
        probes: Vec<SimilarityProbeRequest>,
        chunk_ids: Vec<i64>,
    ) -> Result<ChunkSimilarities, String> {
        if probes.is_empty() {
            return Err("Similarity request names no probes.".to_string());
        }
        if probes.len() > MAX_SIMILARITY_PROBES {
            return Err(format!(
                "Similarity request names {} probes; {MAX_SIMILARITY_PROBES} is the most one \
                 request may ask for.",
                probes.len(),
            ));
        }
        let total: usize =
            chunk_ids.len() + probes.iter().map(|probe| probe.scope.len()).sum::<usize>();
        if total > MAX_SIMILARITY_CHUNK_IDS {
            return Err(format!(
                "Similarity request names {total} chunk ids across the searched set and its \
                 scopes; {MAX_SIMILARITY_CHUNK_IDS} is the most one request may ask for.",
            ));
        }
        // The same refusal the centroid and the chunk export make: mid-rebuild,
        // the ids in flight belong to neither the old index nor the new one.
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before asking for similarities.",
        )?;

        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard.as_ref().ok_or_else(|| {
                "Semantic index unavailable. Build or restore the semantic index first.".to_string()
            })?;
            let status = index.status();
            let probes: Vec<wilkes_core::embed::index::db::SimilarityProbe> = probes
                .into_iter()
                .map(|probe| wilkes_core::embed::index::db::SimilarityProbe {
                    vector: probe.vector,
                    scope: probe.scope,
                })
                .collect();
            let found = index
                .chunk_similarity(&probes, &chunk_ids)
                .map_err(|error| format!("Could not compute chunk similarities: {error:#}"))?;
            Ok(ChunkSimilarities {
                engine: status.engine.as_str().to_string(),
                model_id: status.model_id,
                dimension: status.dimension,
                probes: found
                    .probes
                    .into_iter()
                    .map(|probe| ProbeSimilarity {
                        nearest_chunk_id: probe.nearest_chunk_id,
                        similarity: probe.similarity,
                        scope_mean: probe.scope_mean,
                        scope_size: probe.scope_size,
                    })
                    .collect(),
                chunks: found
                    .chunks
                    .into_iter()
                    .map(|chunk| ChunkNearest {
                        chunk_id: chunk.chunk_id,
                        probe: chunk.probe,
                        similarity: chunk.similarity,
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Similarity task panicked: {error}"))?
    }

    /// One indexed document's chunks, in extraction order, with the ordinals
    /// every chunk export speaks in.
    ///
    /// Shared by [`Self::export_file_chunks`] and [`Self::export_chunk_text`]
    /// rather than written out twice. An ordinal is a position in *this*
    /// ordering and has no meaning apart from it, so a second copy of the sort
    /// would be a second definition of what "chunk 12" is — and a consumer that
    /// stored an ordinal from one export and redeemed it against the other
    /// would be reading whatever the drift left there.
    async fn indexed_chunks(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<(PathBuf, Vec<ExportedChunk>), String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before exporting chunks.",
        )?;
        let (root, path) = self.export_file_path(root, path, "Chunk export").await?;

        let index_arc = self.index.lock().clone();
        let task_root = root.clone();
        let task_path = path.clone();
        let mut chunks = tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard.as_ref().ok_or_else(|| {
                "Semantic index unavailable. Build or restore the semantic index first.".to_string()
            })?;
            index
                .topic_chunks_for_file(&task_root, &task_path)
                .map_err(|error| format!("Could not load indexed chunks: {error:#}"))
        })
        .await
        .map_err(|error| format!("Chunk export task panicked: {error}"))??;

        // Stable reading order: extraction position, not row id.
        chunks.sort_by_key(|chunk| chunk.extraction_byte_range.start);
        let chunks = chunks
            .into_iter()
            .enumerate()
            .map(|(ordinal, chunk)| ExportedChunk {
                chunk_id: chunk.chunk_id,
                ordinal,
                text: chunk.chunk_text,
                byte_range: chunk.extraction_byte_range,
                origin: chunk.origin,
                embedding: chunk.embedding,
            })
            .collect();
        Ok((path, chunks))
    }

    /// Canonicalize one library document for an export route. All file export
    /// endpoints share this boundary so a new read surface cannot accidentally
    /// weaken the library-root or supported-file restrictions.
    async fn export_file_path(
        &self,
        root: PathBuf,
        path: PathBuf,
        label: &str,
    ) -> Result<(PathBuf, PathBuf), String> {
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&root)?;
        Self::ensure_path_in_library(&root, &library_roots, &format!("{label} root"))?;
        let (path, _) =
            Self::canonicalize_supported_file(&root, &path, &settings.supported_extensions, label)?;
        Self::ensure_path_in_library(&path, &library_roots, &format!("{label} file"))?;
        Ok((root, path))
    }

    /// Export one document's declared outline without loading an index or an
    /// embedder. The outline retains the document's native page/byte locators;
    /// callers that need chunk ordinals should use [`Self::export_file_chunks`].
    pub async fn export_file_outline(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<FileOutlineExport, String> {
        let (_, path) = self.export_file_path(root, path, "Outline export").await?;
        let outline_path = path.clone();
        let declared_outline = tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            wilkes_core::extract::document_outline(&outline_path, &registry)
                .map_err(|error| format!("Could not read the document outline: {error:#}"))
        })
        .await
        .map_err(|error| format!("Outline export task panicked: {error}"))??;

        Ok(FileOutlineExport {
            file_path: path,
            outline: declared_outline.entries,
            extraction: declared_outline.diagnostics,
        })
    }

    /// Export one indexed document's chunks with their locators and stored
    /// vectors — the source of segment positions for consumers that must
    /// never re-extract (Underdog requirement E1). Read-only over the live
    /// semantic index.
    pub async fn export_file_chunks(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<FileChunkExport, String> {
        let model_id = self
            .embedder
            .lock()
            .clone()
            .map(|embedder| embedder.model_id().to_string());

        let (path, chunks) = self.indexed_chunks(root, path).await?;
        let dimension = chunks.first().map(|chunk| chunk.embedding.len());

        // The declared outline, resolved against the chunks just exported.
        //
        // Read from the file rather than the index: an outline is what the
        // author wrote, and the index stores what was extracted. It rides
        // along with an export instead of needing an endpoint and a second
        // round trip — the export is already the request that wants to know
        // where this document's sections begin. A file whose outline cannot be
        // read is reported as an error and not as an absent outline: "this
        // document declares no sections" is a claim consumers act on.
        let outline_path = path.clone();
        let outline = tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            wilkes_core::extract::document_outline(&outline_path, &registry)
                .map_err(|error| format!("Could not read the document outline: {error:#}"))
        })
        .await
        .map_err(|error| format!("Outline task panicked: {error}"))??;
        let outline = resolve_outline(&outline.entries, &chunks);

        Ok(FileChunkExport {
            file_path: path,
            model_id,
            dimension,
            outline,
            chunks,
        })
    }

    /// Every document Wilkes serves under one library root, each with the
    /// number of passages the index holds for it.
    ///
    /// This is the browse half of the export surface. Wilkes decides what
    /// counts as a document — its supported extensions, its size limit, its
    /// ignore rules — and its index decides what can be exported, so a
    /// consumer that walked the directory itself would be reimplementing two
    /// rules it does not own and would disagree with `export_file_chunks` the
    /// moment either changed. `/api/files` cannot answer this: it is confined
    /// to the uploads directory, which a real library root never is.
    ///
    /// The listing is the filesystem's, the counts are the index's, and they
    /// are reported together rather than merged into a verdict: a file present
    /// but unindexed is a normal state with a fix in Wilkes, and only the
    /// caller knows whether it wants to show it, hide it or refuse it.
    pub async fn export_library_files(&self, root: PathBuf) -> Result<LibraryFileExport, String> {
        // The same refusal the chunk export makes, for the same reason: while
        // the index is being rewritten its counts describe neither the old
        // index nor the new one, and a listing that invited a person to pick
        // from them would be handing out exports that are about to fail.
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before listing library files.",
        )?;
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Library listing root")?;

        let listing = self
            .list_files(root.clone())
            .await
            .map_err(|error| format!("Could not list library files: {error:#}"))?;

        let index_arc = self.index.lock().clone();
        let counts_root = root.clone();
        let counts = tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            // No index is not an error here. The directory still has documents
            // in it, and "none of them is indexed yet" is exactly what a
            // person who has not built the index should be told — by seeing
            // their files with nothing behind them, not by an empty screen.
            let Some(index) = guard.as_ref() else {
                return Ok::<_, String>(std::collections::HashMap::new());
            };
            index
                .indexed_chunk_counts_for_root(&counts_root)
                .map_err(|error| format!("Could not read indexed passage counts: {error:#}"))
        })
        .await
        .map_err(|error| format!("Library listing task panicked: {error}"))??;

        let mut files: Vec<LibraryFile> = listing
            .files
            .into_iter()
            .map(|entry| LibraryFile {
                chunk_count: counts.get(&entry.path).copied().unwrap_or(0),
                path: entry.path,
                size_bytes: entry.size_bytes,
                extension: entry.extension,
                modified_at_ms: entry.modified_at_ms,
                title: entry.title,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(LibraryFileExport { root, files })
    }

    /// The text of named chunks of one indexed document, without their vectors
    /// — for a consumer that already knows which passage it wants and needs to
    /// show it to a person.
    ///
    /// Separate from [`Self::export_file_chunks`] because the two answer
    /// different questions at different scales. A full export is a
    /// once-per-document ingestion step whose reply runs to megabytes, nearly
    /// all of it embeddings; this is a per-view lookup of a paragraph or two,
    /// and asking for a whole book to display one of them would make the size
    /// of the reply a function of the document rather than of the request.
    ///
    /// Chunks are named by `chunk_id` because that is the addressing a consumer
    /// keeps: an export hands back both, but it is the id that gets written down
    /// against whatever the consumer derived from the passage. The reply carries
    /// the ordinal too, so positional locators recorded from an export stay
    /// usable without a second round trip to translate them.
    ///
    /// An id this document does not have is an error rather than an omission. It
    /// means the file was re-indexed since the caller recorded that id, and
    /// returning the chunks that *did* resolve would show a person a passage
    /// from the wrong place while looking like a complete answer.
    pub async fn export_chunk_text(
        &self,
        root: PathBuf,
        path: PathBuf,
        chunk_ids: Vec<i64>,
    ) -> Result<ChunkTextExport, String> {
        if chunk_ids.is_empty() {
            return Err("Chunk text export names no chunks.".to_string());
        }
        if chunk_ids.len() > MAX_CHUNK_TEXT_IDS {
            return Err(format!(
                "Chunk text export asks for {} chunks; {MAX_CHUNK_TEXT_IDS} is the most one \
                 request may name. Ask for the passage you mean to show, not the document.",
                chunk_ids.len(),
            ));
        }

        let (path, chunks) = self.indexed_chunks(root, path).await?;

        let mut wanted: Vec<i64> = chunk_ids;
        wanted.sort_unstable();
        wanted.dedup();
        let mut found: Vec<ChunkText> = chunks
            .iter()
            .filter(|chunk| wanted.binary_search(&chunk.chunk_id).is_ok())
            .map(|chunk| ChunkText {
                chunk_id: chunk.chunk_id,
                ordinal: chunk.ordinal,
                text: chunk.text.clone(),
                byte_range: chunk.byte_range.clone(),
                origin: chunk.origin.clone(),
            })
            .collect();
        if found.len() != wanted.len() {
            let missing: Vec<String> = wanted
                .iter()
                .filter(|id| !found.iter().any(|chunk| chunk.chunk_id == **id))
                .map(i64::to_string)
                .collect();
            return Err(format!(
                "Chunk text export asks for chunk{} {} which this document does not have — it was \
                 re-indexed since those ids were recorded.",
                if missing.len() == 1 { "" } else { "s" },
                missing.join(", "),
            ));
        }
        // Ascending by ordinal: reading order, whatever order the ids arrived in.
        found.sort_by_key(|chunk| chunk.ordinal);
        Ok(ChunkTextExport {
            file_path: path,
            chunks: found,
        })
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
        self.list_files_filtered_with_ignore(
            root,
            collection_id,
            tag_ids,
            collection_expression,
            true,
            None,
        )
        .await
    }

    /// List the searchable files under `root`, enriched with metadata, tags and
    /// collection eligibility.
    ///
    /// `only_path` narrows the enumeration step to a single known file. The
    /// enrichment and filtering below are unchanged by it — they simply operate
    /// on a one-entry listing — so a targeted query pays for one file instead of
    /// walking the whole root to discard all but one result.
    async fn list_files_filtered_with_ignore(
        &self,
        root: PathBuf,
        collection_id: Option<&str>,
        tag_ids: &[String],
        collection_expression: Option<&str>,
        respect_gitignore: bool,
        only_path: Option<PathBuf>,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        let s = self.get_settings().await;
        let mut response = match only_path {
            Some(path) => {
                crate::commands::files::list_single_file(
                    path,
                    s.supported_extensions.clone(),
                    s.max_file_size,
                )
                .await?
            }
            None => {
                crate::commands::files::list_files_with_ignore(
                    root.clone(),
                    s.supported_extensions.clone(),
                    s.max_file_size,
                    respect_gitignore,
                )
                .await?
            }
        };

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
        self.ensure_writable().map_err(anyhow::Error::msg)?;
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
        self.ensure_writable().map_err(anyhow::Error::msg)?;
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

    /// Whether this context belongs to a workspace an application owns rather
    /// than a person (Underdog's semantic corpus is the only one today).
    ///
    /// Such a workspace has exactly one writer of its semantic index: the
    /// managed import API, which publishes rows carrying the document identity
    /// its consumer's stable chunk refs are derived from. The interactive
    /// indexing machinery — the directory watcher and the background reindex it
    /// feeds — must never run there. Its root is `managed_sources`, which every
    /// import writes into, so it would otherwise re-index each imported
    /// document through the identity-less path and strip the very refs the
    /// import just handed out.
    fn is_application_managed(&self) -> bool {
        if self.workspace_path == self.settings_path {
            return false;
        }
        crate::workspace::read_manifest(&self.workspace_path)
            .map(|manifest| manifest.is_application_managed())
            .unwrap_or(false)
    }

    /// Whether the user may only read this workspace. The same condition as
    /// [`Self::is_application_managed`], named for the caller that asks it:
    /// a workspace whose sole writer is an application's import API is
    /// read-only to everybody else.
    pub fn is_read_only(&self) -> bool {
        self.is_application_managed()
    }

    /// The one gate every user-initiated write to this workspace's documents
    /// or index passes through.
    ///
    /// An application-managed corpus is now listed and searchable like any
    /// other workspace, so a person can activate it and reach every read path
    /// the UI offers. What used to stop them — the workspace being absent from
    /// the listing and refused by `switch` — protected the corpus only by
    /// making it unreachable, which cost the reads as well as the writes. This
    /// is the protection stated where it belongs: on the write.
    ///
    /// Deliberately not applied to [`Self::import_managed_document`] and the
    /// rest of the managed corpus API. That caller *is* the workspace's owner;
    /// it is every other caller that must be turned away.
    pub fn ensure_writable(&self) -> Result<(), String> {
        if self.is_application_managed() {
            return Err(
                "MANAGED_WORKSPACE_PROTECTED: this workspace is owned by another application and \
                 can only be read"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn start_directory_watcher(self: &Arc<Self>, root: PathBuf) {
        self.stop_directory_watcher();
        if self.is_application_managed() {
            info!(
                "not watching {}: an application-managed corpus is written only by its import API",
                root.display()
            );
            return;
        }
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

        let registry = wilkes_core::extract::production_registry();
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
        let current = match get_scoped_settings(&self.settings_path, &self.workspace_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("update_semantic_settings: read: {e:#}");
                return;
            }
        };
        let semantic = f(current.semantic);
        if let Err(e) = update_scoped_settings(
            &self.settings_path,
            &self.workspace_path,
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
    /// Reconcile the process-wide image analyzer after a settings edit.
    ///
    /// The Ollama endpoint is in here because the describer speaks to it:
    /// moving the server moves the describer, even though nothing under
    /// `image_analysis` changed.
    fn on_image_analysis_settings_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if before.image_analysis == after.image_analysis
            && before.generation.ollama_url == after.generation.ollama_url
        {
            return;
        }
        if !after.image_analysis.enabled {
            // Synchronously, with the settings transition: extraction reads the
            // analyzer on every call, and a disabled feature must not keep
            // enriching while a detach is queued.
            info!("image analysis disabled; detaching the analyzer");
            wilkes_core::extract::image::configure(None);
            return;
        }
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = ctx.load_image_analyzer().await {
                error!("could not attach the image analyzer: {error:#}");
            }
        });
    }

    /// Build the analyzer the settings describe and install it for the whole
    /// process.
    ///
    /// Loading is 1.9 GB read into memory, so it is serialized against itself
    /// and run off the async executor. A failure detaches rather than leaving
    /// the previous analyzer in place: the settings no longer describe it, and
    /// silently enriching under the old recipe is the one outcome that would
    /// put two answers into one index.
    pub async fn load_image_analyzer(self: &Arc<Self>) -> anyhow::Result<bool> {
        let _serialized = self.image_analyzer_load_lock.lock().await;
        let settings = self.get_settings().await;
        let data_dir = self.shared_data_dir.clone();
        let built = tokio::task::spawn_blocking(move || {
            wilkes_core::extract::image::build_analyzer(
                &data_dir,
                &settings.image_analysis,
                &settings.generation.ollama_url,
            )
        })
        .await?;
        match built {
            Ok(analyzer) => {
                let attached = analyzer.is_some();
                wilkes_core::extract::image::configure(analyzer);
                Ok(attached)
            }
            Err(error) => {
                wilkes_core::extract::image::configure(None);
                self.events.emit(
                    "image-analysis-error",
                    serde_json::json!({ "error": format!("{error:#}") }),
                );
                Err(error)
            }
        }
    }

    /// Whether the recognizer the shipped recipe names is on disk and intact.
    pub fn is_image_recognizer_installed(&self) -> bool {
        wilkes_core::extract::image::recognizer_installed(&self.shared_data_dir)
    }

    /// What the shipped recognizer is, where it came from, and under what
    /// terms. Answers before the download, which is the only time it is of
    /// any use to whoever has to decide about it.
    pub fn image_recognizer_inventory(
        &self,
    ) -> wilkes_core::extract::image::paddleocr_vl::RecognizerInventory {
        wilkes_core::extract::image::recognizer_inventory()
    }

    /// Download and verify the recognizer, then attach the analyzer if the
    /// settings ask for one.
    ///
    /// Progress travels on its own event stream for the reason the generation
    /// one does: borrowing `embed-*` would put the UI into "indexing" with no
    /// terminal event to leave it.
    pub async fn install_image_recognizer(self: &Arc<Self>) -> anyhow::Result<()> {
        let (progress_tx, mut progress_rx) = mpsc::channel::<EmbedProgress>(64);
        let events = Arc::clone(&self.events);
        let forward = tokio::spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                events.emit("image-analysis-progress", serde_json::json!(progress));
            }
        });
        let data_dir = self.shared_data_dir.clone();
        let installed = tokio::task::spawn_blocking(move || {
            wilkes_core::extract::image::install_recognizer(&data_dir, Some(progress_tx))
        })
        .await?;
        let _ = forward.await;
        if let Err(error) = installed {
            self.events.emit(
                "image-analysis-error",
                serde_json::json!({ "error": format!("{error:#}") }),
            );
            return Err(error);
        }
        self.load_image_analyzer().await?;
        self.events.emit("image-analysis-done", serde_json::json!({}));
        Ok(())
    }

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
            let before = get_scoped_settings(&self.settings_path, &self.workspace_path)
                .await
                .unwrap_or_default();
            let updated =
                update_scoped_settings(&self.settings_path, &self.workspace_path, patch).await?;
            (before, updated)
        };
        self.on_directory_setting_maybe_changed(&before, &updated);
        self.on_zotero_settings_maybe_changed(&before, &updated);
        self.on_semantic_scholar_settings_maybe_changed(&before, &updated);
        self.on_openalex_settings_maybe_changed(&before, &updated);
        self.on_search_runtime_settings_maybe_changed(&before, &updated);
        self.on_generation_settings_maybe_changed(&before, &updated);
        self.on_image_analysis_settings_maybe_changed(&before, &updated);
        Ok(updated)
    }

    /// Reconcile the two consumers of the shared index after a settings edit.
    /// Semantic search owns the embedder; semantic search and index-backed exact
    /// search jointly own index residency. Loads run off the settings write path,
    /// while resources made unnecessary by the new state detach synchronously.
    fn on_search_runtime_settings_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if before.search_prefer_semantic == after.search_prefer_semantic
            && before.grep_use_index == after.grep_use_index
        {
            return;
        }

        if !after.search_prefer_semantic {
            // Semantic search is off. Its model must not remain resident, but
            // exact search may still own the already-open index.
            self.detach_semantic_embedder();
        }
        if !after.search_prefer_semantic && !after.grep_use_index {
            self.detach_search_index();
            return;
        }

        let semantic_just_enabled = after.search_prefer_semantic && !before.search_prefer_semantic;
        let exact_just_enabled = after.grep_use_index && !before.grep_use_index;
        if semantic_just_enabled || (exact_just_enabled && !self.is_index_loaded()) {
            // Reload can install/probe the embedder or open the index, so do it
            // off the settings write path. Exact-only activation never touches
            // the embedding model.
            let ctx = Arc::clone(self);
            tokio::spawn(async move {
                ctx.activate_required_search_runtime_from_disk().await;
            });
        }
    }

    /// Release the semantic-only model state while preserving an index still
    /// owned by index-backed exact search.
    fn detach_semantic_embedder(&self) {
        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = None;
    }

    /// Release the shared index after its final consumer is disabled. The
    /// on-disk DB is preserved so either consumer can reactivate it cheaply.
    fn detach_search_index(&self) {
        self.invalidate_topic_tree_cache();
        *self.index.lock() = Arc::new(Mutex::new(None));
    }

    fn is_index_loaded(&self) -> bool {
        self.index
            .lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// Reload the embedder, index, and watcher from the on-disk DB when a usable
    /// one exists. No-op if semantic is already live or nothing is built yet.
    async fn activate_semantic_from_disk(self: &Arc<Self>) {
        if self.is_semantic_ready() {
            return;
        }
        let settings = self.get_settings().await;
        if !settings.search_prefer_semantic {
            return;
        }
        if let Some(loaded) = self.load_restore_state(settings).await {
            if !self.get_settings().await.search_prefer_semantic {
                return;
            }
            self.finish_restore_state(&loaded.plan, loaded.embedder, loaded.index)
                .await;
        }
    }

    /// Activate every search resource required by the latest settings. Exact
    /// index restoration is attempted after semantic restoration so it can
    /// still succeed when the embedding model is unavailable.
    async fn activate_required_search_runtime_from_disk(self: &Arc<Self>) {
        let settings = self.get_settings().await;
        if settings.search_prefer_semantic {
            self.activate_semantic_from_disk().await;
        }
        let settings = self.get_settings().await;
        if settings.grep_use_index && !self.is_index_loaded() {
            self.activate_exact_index_from_disk().await;
        }
    }

    /// Open only the existing index for exact search. This intentionally does
    /// not install, probe, or retain an embedding model. Files changed while no
    /// embedder is resident are rejected by identity checks and extracted live.
    async fn activate_exact_index_from_disk(self: &Arc<Self>) {
        if self.is_index_loaded() {
            return;
        }
        let settings = self.get_settings().await;
        if !settings.grep_use_index {
            return;
        }
        let Some((_plan, index)) = self.load_restore_index_only(settings).await else {
            return;
        };
        let latest = self.get_settings().await;
        if !latest.grep_use_index || self.is_index_loaded() {
            return;
        }
        self.restore_store_index_only(index);
        info!("restore_state: exact-search index restored without embedder");
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
        self.ensure_writable()?;
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
        generate_dispatch::list_models(settings.engine, &self.shared_data_dir, &settings.ollama_url)
            .await
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
            &self.shared_data_dir,
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
        if self.is_application_managed() {
            info!(
                "not reindexing {}: an application-managed corpus is written only by its import API",
                root.display()
            );
            return;
        }
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
        if query.mode == SearchMode::Grep && settings.grep_use_index && !self.is_index_loaded() {
            self.activate_exact_index_from_disk().await;
        }
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
        let mut documents = Vec::new();
        let mut catalog_errors = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();
        let catalog_started = std::time::Instant::now();
        // A `File` scope names exactly one document, so the catalog step
        // resolves that path directly instead of enumerating the root.
        let only_path = match &query.scope {
            SearchScope::File { path } => Some(path.clone()),
            _ => None,
        };
        for root in &eligibility_roots {
            match self
                .list_files_filtered_with_ignore(
                    root.clone(),
                    query.collection_id.as_deref(),
                    &query.tag_ids,
                    None,
                    query.respect_gitignore && only_path.is_none(),
                    only_path.clone(),
                )
                .await
            {
                Ok(listed) => {
                    if let SearchScope::File { path } = &query.scope {
                        if let Some(omitted) =
                            listed.omitted.iter().find(|entry| entry.file.path == *path)
                        {
                            if omitted.reason == OmittedFileReason::TooLarge {
                                catalog_errors.push(format!(
                                    "Search file exceeds the configured maximum size ({} bytes > {} bytes): {}",
                                    omitted.file.size_bytes,
                                    settings.max_file_size,
                                    path.display()
                                ));
                            }
                        }
                    }
                    for entry in listed.files {
                        if seen_paths.insert(entry.path.clone()) {
                            documents.push(SearchDocument {
                                path: entry.path,
                                file_type: entry.file_type,
                                title: entry.title,
                                author: entry.author,
                            });
                        }
                    }
                }
                Err(error) => catalog_errors.push(format!("{}: {error:#}", root.display())),
            }
        }
        let catalog_elapsed_ms = catalog_started.elapsed().as_millis() as u64;

        let mut semantic_indexing = None;
        if query.scope == SearchScope::All {
            catalog_errors.splice(0..0, library_root_errors);
        }
        let (embedder, index) = match query.mode {
            SearchMode::Semantic => {
                let runtime = if query.scope == SearchScope::All {
                    self.prepare_global_semantic_runtime(&settings).await?
                } else {
                    self.prepare_semantic_runtime(&query.root, &settings)
                        .await?
                };
                semantic_indexing = Some(runtime.indexing);
                (Some(runtime.embedder), Some(runtime.index))
            }
            SearchMode::Grep => {
                let index = settings.grep_use_index.then(|| self.index.lock().clone());
                (None, index)
            }
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
            documents,
            catalog_errors,
            embedder,
            index,
            semantic_indexing,
            log,
            retrieval,
            generator,
            settings.grep_use_index,
        )
        .with_catalog_elapsed_ms(catalog_elapsed_ms)
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
            // The configured registry, not a bare extractor: a summary read
            // under a different recipe from the index's is a second answer to
            // what this document says.
            wilkes_core::extract::production_registry()
                .find(path, None)
                .ok_or_else(|| format!("No extractor for {}", path.display()))?
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
        self.ensure_writable()?;
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
        identity: wilkes_core::embed::EmbeddingSpaceIdentity,
    ) -> Result<SemanticIndex, String> {
        match tokio::task::spawn_blocking(move || SemanticIndex::open_exact(&data_dir, &identity))
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
        let mut index = self
            .open_built_index(data_dir.to_path_buf(), embedder.embedding_space_identity())
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
        let data_dir = self.shared_data_dir.clone();
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
        if let Err(e) = installer.install(&self.shared_data_dir, probe_tx).await {
            return Err(format!("Failed to probe model dimensions: {e:#}"));
        }

        match installer.build(&self.shared_data_dir) {
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

    /// Whether [`Self::shutdown`] has run. For callers that own several
    /// contexts and have to assert they released all of them.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
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
        self.ensure_writable().map_err(anyhow::Error::msg)?;
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

    async fn load_restore_plan(&self, settings: Settings) -> Option<RestoreStatePlan> {
        let db_status = self.load_restore_db_status(&settings).await?;
        match Self::prepare_restore_state_plan(settings, db_status) {
            RestoreStatePreparation::Ready(plan) => Some(plan),
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                info!(
                    "restore_state: index selection '{:?}/{}' != settings selection '{:?}/{}', clearing stale index reference",
                    db_status.engine, db_status.model_id, selected.engine, selected.model.model_id()
                );
                self.clear_restore_state_settings().await;
                None
            }
        }
    }

    async fn load_restore_state(
        self: &Arc<Self>,
        settings: Settings,
    ) -> Option<RestoreLoadedState> {
        let plan = self.load_restore_plan(settings).await?;

        let embedder = self
            .restore_embedder(&plan.selected, plan.device.clone())
            .await?;
        let index = self
            .restore_index(&plan.selected, embedder.dimension())
            .await?;
        if let Err(error) =
            index.validate_local_embedding_space(&embedder.embedding_space_identity())
        {
            error!("restore_state: incompatible embedding space: {error:#}");
            return None;
        }

        Some(RestoreLoadedState {
            plan,
            embedder,
            index,
        })
    }

    async fn load_restore_index_only(
        &self,
        settings: Settings,
    ) -> Option<(RestoreStatePlan, SemanticIndex)> {
        let plan = self.load_restore_plan(settings).await?;
        let index = self
            .restore_index(&plan.selected, plan.db_status.dimension)
            .await?;
        Some((plan, index))
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
        if let Err(err) = installer.install(&self.shared_data_dir, probe_tx).await {
            error!("restore_state: install probe failed: {err:#}");
            return None;
        }
        if !installer.is_available(&self.shared_data_dir) {
            info!("restore_state: model files absent, skipping");
            return None;
        }

        let data_dir = self.shared_data_dir.clone();
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
        self.spawn_full_text_backfill();
        index_arc
    }

    fn restore_store_index_only(&self, index: SemanticIndex) -> Arc<Mutex<Option<SemanticIndex>>> {
        self.invalidate_topic_tree_cache();
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);
        self.spawn_full_text_backfill();
        index_arc
    }

    /// Converge `full_text` right after a restored-from-disk index is installed.
    /// Legacy rows that carry chunks but no stored text force exact search to
    /// re-extract them live on every query; fill them once, in the background,
    /// off the index lock. Self-limiting: a cheap no-op query once nothing is
    /// stale, so it is safe to run on every load (startup and runtime toggle).
    fn spawn_full_text_backfill(&self) {
        let index_arc = Arc::clone(&*self.index.lock());
        tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            let filled = SemanticIndex::backfill_missing_full_text(&index_arc, &registry);
            if filled > 0 {
                info!("full_text backfill: filled {filled} legacy row(s)");
            }
        });
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

    /// Restore the search resources required by current settings and restart
    /// the filesystem watcher. Semantic search loads the embedder plus index;
    /// index-backed exact search can load the index alone. Run once after `new`.
    pub async fn restore_state(self: Arc<Self>) {
        let settings = match get_scoped_settings(&self.settings_path, &self.workspace_path).await {
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
        // Likewise independent: image analysis has its own enable flag, and
        // every extraction path — not only indexing — reads the analyzer.
        if settings.image_analysis.enabled {
            let ctx = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = ctx.load_image_analyzer().await {
                    error!("restore_state: could not attach the image analyzer: {e:#}");
                }
            });
        }
        if !settings.search_prefer_semantic && !settings.grep_use_index {
            info!("restore_state: no search consumer requires the index");
            return;
        }
        self.activate_required_search_runtime_from_disk().await;
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

    #[test]
    fn managed_backup_is_a_self_verifying_sqlite_snapshot() {
        let dir = tempdir().unwrap();
        let managed_sources = dir.path().join("managed_sources");
        std::fs::create_dir(&managed_sources).unwrap();
        std::fs::write(managed_sources.join("source.txt"), b"retained source").unwrap();
        std::fs::write(dir.path().join("workspace.json"), b"{\"version\":1}").unwrap();
        let index = SemanticIndex::create(
            dir.path(),
            "backup-test-model",
            2,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        let space = index.embedding_space_identity().unwrap().id().0;
        drop(index);

        let destination = dir.path().join("backup");
        let backup =
            backup_managed_directory(dir.path(), "corpus-test", &space, &destination).unwrap();

        assert_eq!(backup.format, "wilkes-managed-corpus-backup/v1");
        assert_eq!(backup.corpus_id, "corpus-test");
        assert_eq!(backup.embedding_space_id, space);
        assert!(destination.join("semantic_index.db").is_file());
        assert!(destination.join("backup-manifest.json").is_file());
        assert!(backup
            .files
            .iter()
            .any(|file| file.path == "managed_sources/source.txt"));
        for file in &backup.files {
            let path = destination.join(&file.path);
            assert_eq!(std::fs::metadata(&path).unwrap().len(), file.byte_len);
            assert_eq!(
                wilkes_core::embed::identity::sha256_file(&path).unwrap(),
                file.sha256
            );
        }

        let mismatch = dir.path().join("wrong-space-backup");
        let error =
            backup_managed_directory(dir.path(), "corpus-test", "space-not-the-index", &mismatch)
                .unwrap_err();
        assert!(error.to_string().contains("EMBEDDING_SPACE_MISMATCH"));
        assert!(!mismatch.exists());
    }

    #[test]
    fn interrupted_snapshot_placement_leaves_no_temporary_source() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source.txt");
        std::fs::write(&source, b"immutable source").unwrap();
        let digest = wilkes_core::embed::identity::sha256_file(&source).unwrap();
        let obstructing_destination = dir
            .path()
            .join("managed_sources")
            .join(digest)
            .join("source.txt");
        std::fs::create_dir_all(&obstructing_destination).unwrap();

        AppContext::retain_managed_snapshot(dir.path(), &source)
            .expect_err("a directory cannot be published as an immutable source file");

        let imports = dir.path().join("managed_sources").join(".imports");
        assert_eq!(
            std::fs::read_dir(imports).unwrap().count(),
            0,
            "a failed atomic placement must clean its staging file"
        );
    }
    use tokio::sync::mpsc;
    use tracing::subscriber;
    use tracing_subscriber::prelude::*;
    use wilkes_core::embed::index::chunk::Chunk;
    use wilkes_core::embed::index::db::PreparedFile;
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
        ctx.detach_semantic_embedder();
        ctx.detach_search_index();
        assert!(ctx.topic_tree_caches.lock().root.is_none());
        assert!(ctx.topic_tree_caches.lock().document.is_none());
        assert!(ctx.semantic_index_revision.load(Ordering::Acquire) > revision);
    }

    #[tokio::test]
    async fn embed_texts_returns_pinned_identity_and_validated_vectors() {
        let (_dir, ctx) = test_ctx();

        // No embedder loaded: loud, actionable error.
        let err = ctx.embed_texts(vec!["a".to_string()]).await.unwrap_err();
        assert!(err.contains("Semantic model unavailable"), "{err}");

        *ctx.embedder.lock() = Some(Arc::new(TopicEmbedder {
            calls: Arc::new(AtomicUsize::new(0)),
        }));

        let err = ctx.embed_texts(Vec::new()).await.unwrap_err();
        assert!(err.contains("No texts provided"), "{err}");

        let result = ctx
            .embed_texts(vec!["cat concept".to_string(), "dog concept".to_string()])
            .await
            .unwrap();
        assert_eq!(result.model_id, "topic-test");
        assert_eq!(result.dimension, 2);
        assert_eq!(result.vectors.len(), 2);
        assert_eq!(result.vectors[0], vec![1.0, 0.0]);
        assert_eq!(result.vectors[1], vec![0.0, 1.0]);
    }

    #[tokio::test]
    async fn export_file_chunks_returns_extraction_ordered_chunks_with_vectors() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("export-root");
        std::fs::create_dir_all(&root).unwrap();
        let document = root.join("paper.txt");
        std::fs::write(&document, "chunk export passages").unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        // No index yet: loud, actionable error.
        let err = ctx
            .export_file_chunks(root.clone(), document.clone())
            .await
            .unwrap_err();
        assert!(err.contains("Semantic index unavailable"), "{err}");

        let mut index = SemanticIndex::create(
            dir.path(),
            "export-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        // Written out of extraction order on purpose: the export must sort
        // by byte range, not row id.
        let chunks = vec![
            (
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: document.clone(),
                    text: "second passage".to_string(),
                    byte_range: ByteRange { start: 50, end: 64 },
                    origin: SourceOrigin::TextFile { line: 5, col: 1 },
                },
                vec![0.0, 1.0],
            ),
            (
                wilkes_core::embed::index::chunk::Chunk {
                    file_path: document.clone(),
                    text: "first passage".to_string(),
                    byte_range: ByteRange { start: 0, end: 13 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            ),
        ];
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: document.clone(),
                chunks,
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let export = ctx
            .export_file_chunks(root.clone(), document.clone())
            .await
            .unwrap();
        assert_eq!(export.dimension, Some(2));
        assert_eq!(export.model_id, None, "no live embedder loaded");
        assert_eq!(export.chunks.len(), 2);
        assert_eq!(export.chunks[0].ordinal, 0);
        assert_eq!(export.chunks[0].text, "first passage");
        assert_eq!(export.chunks[0].byte_range.start, 0);
        assert_eq!(export.chunks[0].embedding, vec![1.0, 0.0]);
        assert_eq!(export.chunks[1].ordinal, 1);
        assert_eq!(export.chunks[1].text, "second passage");
        assert!(
            export.outline.is_empty(),
            "the document declares no headings"
        );

        // Files outside the library are refused.
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let err = ctx
            .export_file_chunks(root.clone(), outside)
            .await
            .unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn outline_export_reads_declared_headings_without_a_semantic_index() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("outline-only-root");
        std::fs::create_dir_all(&root).unwrap();
        let document = root.join("notes.md");
        std::fs::write(&document, "# One\nbody\n## Two\nmore body\n").unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        // No semantic index or embedder is installed. A declared outline is
        // source structure, not an embedding export, so this still succeeds.
        let export = ctx
            .export_file_outline(root.clone(), document.clone())
            .await
            .unwrap();

        assert_eq!(export.file_path, std::fs::canonicalize(&document).unwrap());
        assert_eq!(export.outline.len(), 2);
        assert_eq!(export.outline[0].title, "One");
        assert_eq!(export.outline[0].level, 0);
        assert_eq!(
            export.outline[0].anchor,
            wilkes_core::types::OutlineAnchor::TextOffset
        );
        assert_eq!(export.outline[1].title, "Two");
        assert_eq!(export.outline[1].level, 1);
        assert_eq!(export.extraction.pages, 0);

        let outside = dir.path().join("outside.md");
        std::fs::write(&outside, "# Not in the library\n").unwrap();
        let err = ctx.export_file_outline(root, outside).await.unwrap_err();
        assert!(err.contains("not in the library"), "{err}");
    }

    /// Browsing a root answers two questions at once, and the test holds both:
    /// which documents Wilkes serves there (its rules, not the caller's), and
    /// how much of each one the index can actually export.
    #[tokio::test]
    async fn library_file_export_lists_served_documents_with_indexed_passage_counts() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("browse-root");
        std::fs::create_dir_all(&root).unwrap();
        let indexed = root.join("indexed.txt");
        let unindexed = root.join("unindexed.txt");
        std::fs::write(&indexed, "alpha passage").unwrap();
        std::fs::write(&unindexed, "beta passage").unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        // No index at all: the documents are still there to be seen, and every
        // count is zero rather than the listing being an error.
        let export = ctx.export_library_files(root.clone()).await.unwrap();
        assert_eq!(export.root, root.canonicalize().unwrap());
        assert_eq!(export.files.len(), 2);
        assert!(export.files.iter().all(|file| file.chunk_count == 0));

        let mut index = SemanticIndex::create(
            dir.path(),
            "browse-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: String::new(),
                path: indexed.clone(),
                chunks: vec![(
                    wilkes_core::embed::index::chunk::Chunk {
                        file_path: indexed.clone(),
                        text: "alpha passage".to_string(),
                        byte_range: ByteRange { start: 0, end: 13 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0],
                )],
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let export = ctx.export_library_files(root.clone()).await.unwrap();
        let by_path: std::collections::HashMap<_, _> = export
            .files
            .iter()
            .map(|file| (file.path.clone(), file.chunk_count))
            .collect();
        assert_eq!(
            by_path.get(&indexed.canonicalize().unwrap()),
            Some(&1),
            "the indexed document reports the passages an export would return"
        );
        assert_eq!(
            by_path.get(&unindexed.canonicalize().unwrap()),
            Some(&0),
            "a served but unindexed document is listed, not hidden"
        );
        // Ascending by path, so a consumer rendering the list does not have to
        // decide the order Wilkes already knows.
        let mut sorted = export
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect::<Vec<_>>();
        let unsorted = sorted.clone();
        sorted.sort();
        assert_eq!(unsorted, sorted);

        // A directory outside the library is refused, exactly as the chunk
        // export refuses a file outside it.
        let outside = dir.path().join("outside-root");
        std::fs::create_dir_all(&outside).unwrap();
        let err = ctx.export_library_files(outside).await.unwrap_err();
        assert!(err.contains("not in the library"), "{err}");
    }

    /// The export is the only place that holds both the outline's locators and
    /// the chunk list, so it is where the two are joined — a consumer given the
    /// raw locators would have to re-derive this mapping from the chunks it was
    /// just handed.
    #[tokio::test]
    async fn export_resolves_declared_headings_onto_chunk_ordinals() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("outline-root");
        std::fs::create_dir_all(&root).unwrap();
        let document = root.join("notes.md");
        let text = "# One\nalpha body\n## One point one\nbeta body\n";
        std::fs::write(&document, text).unwrap();
        let mut settings = Settings::default();
        settings.last_directory = Some(root.clone());
        std::fs::write(&ctx.settings_path, serde_json::to_vec(&settings).unwrap()).unwrap();

        let second_heading = text.find("## One").unwrap();
        let mut index = SemanticIndex::create(
            dir.path(),
            "outline-model",
            2,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        index
            .write_file(wilkes_core::embed::index::db::PreparedFile {
                full_text: text.to_string(),
                path: document.clone(),
                chunks: vec![
                    (
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: document.clone(),
                            text: "# One\nalpha body".to_string(),
                            byte_range: ByteRange {
                                start: 0,
                                end: second_heading,
                            },
                            origin: SourceOrigin::TextFile { line: 1, col: 1 },
                        },
                        vec![1.0, 0.0],
                    ),
                    (
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: document.clone(),
                            text: "## One point one\nbeta body".to_string(),
                            byte_range: ByteRange {
                                start: second_heading,
                                end: text.len(),
                            },
                            origin: SourceOrigin::TextFile { line: 3, col: 1 },
                        },
                        vec![0.0, 1.0],
                    ),
                ],
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let export = ctx
            .export_file_chunks(root.clone(), document.clone())
            .await
            .unwrap();
        let outline: Vec<(&str, u32, usize)> = export
            .outline
            .iter()
            .map(|e| (e.title.as_str(), e.level, e.chunk_ordinal))
            .collect();
        assert_eq!(
            outline,
            vec![("One", 0, 0), ("One point one", 1, 1)],
            "each heading lands on the chunk its section starts in"
        );
        // The locator the document expressed survives alongside the resolution.
        assert_eq!(export.outline[1].byte_offset, Some(second_heading));
        assert_eq!(export.outline[1].page, None);
    }

    /// A section that starts after the last chunk starts nowhere — a bookmark
    /// into a scanned appendix, say. Dropping it keeps every surviving entry a
    /// position a consumer can act on.
    #[test]
    fn outline_entries_that_resolve_past_the_document_are_dropped() {
        let chunk = |ordinal: usize, start: usize, end: usize, page: u32| ExportedChunk {
            chunk_id: ordinal as i64,
            ordinal,
            text: String::new(),
            byte_range: ByteRange { start, end },
            origin: SourceOrigin::PdfPage { page, bbox: None },
            embedding: Vec::new(),
        };
        let chunks = vec![chunk(0, 0, 10, 1), chunk(1, 10, 20, 4)];
        let entry =
            |page: Option<u32>, byte_offset: Option<usize>| wilkes_core::types::OutlineEntry {
                title: "T".to_string(),
                level: 0,
                page,
                byte_offset,
                anchor: wilkes_core::types::OutlineAnchor::Page,
            };

        // A bookmark on a page with no extracted text resolves forward to the
        // next page that has some, which is where its section's text begins.
        let resolved = resolve_outline(&[entry(Some(2), None)], &chunks);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].chunk_ordinal, 1);

        // Past the end: nothing to point at.
        assert!(resolve_outline(&[entry(Some(9), None)], &chunks).is_empty());
        assert!(resolve_outline(&[entry(None, Some(999))], &chunks).is_empty());
        // No locator at all is not a position either.
        assert!(resolve_outline(&[entry(None, None)], &chunks).is_empty());
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
        fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
            wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

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

    fn scoped_ctx(dir: &std::path::Path, kind: serde_json::Value) -> Arc<AppContext> {
        let workspace_path = dir.join("workspace.json");
        std::fs::write(
            &workspace_path,
            serde_json::json!({
                "version": 1,
                "id": "workspace-under-test",
                "name": "under test",
                "kind": kind,
            })
            .to_string(),
        )
        .unwrap();
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: dir.to_path_buf(),
        };
        let (ctx, _rx, _loop) = AppContext::new_scoped(
            dir.to_path_buf(),
            dir.to_path_buf(),
            dir.join("settings.json"),
            workspace_path,
            paths,
            Arc::new(MockEmitter {
                events: Arc::new(Mutex::new(Vec::new())),
            }),
        );
        ctx
    }

    /// The managed import API is the only writer of an application-managed
    /// index. A watcher on `managed_sources` would re-index every imported
    /// document through the identity-less path, dropping the stable chunk refs
    /// the import had already handed to its consumer.
    #[tokio::test]
    async fn test_application_managed_root_is_never_watched() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("managed_sources");
        std::fs::create_dir_all(&root).unwrap();

        let managed = scoped_ctx(
            dir.path(),
            serde_json::json!({
                "kind": "application_managed",
                "owner": "underdog",
                "purpose": "semantic-corpus",
                "corpus_key": "store-id",
            }),
        );
        managed.start_directory_watcher(root.clone());
        assert!(
            managed.directory_watcher.lock().is_none(),
            "an application-managed corpus must not be watched"
        );

        let personal = tempdir().unwrap();
        let personal_root = personal.path().join("library");
        std::fs::create_dir_all(&personal_root).unwrap();
        let user = scoped_ctx(personal.path(), serde_json::json!({ "kind": "user" }));
        user.start_directory_watcher(personal_root);
        assert!(
            user.directory_watcher.lock().is_some(),
            "an ordinary workspace is still watched"
        );
    }

    /// The corpus is reachable now — listed, activatable, searchable — so the
    /// protection that used to come from being unreachable has to be stated on
    /// the write itself.
    #[tokio::test]
    async fn test_application_managed_workspace_refuses_writes() {
        let dir = tempdir().unwrap();
        let managed = scoped_ctx(
            dir.path(),
            serde_json::json!({
                "kind": "application_managed",
                "owner": "underdog",
                "purpose": "semantic-corpus",
                "corpus_key": "store-id",
            }),
        );
        assert!(managed.is_read_only());
        let error = managed.ensure_writable().unwrap_err();
        assert!(error.contains("MANAGED_WORKSPACE_PROTECTED"), "{error}");
        assert!(managed
            .save_document(dir.path().join("note.md"), "text".to_string())
            .await
            .unwrap_err()
            .contains("MANAGED_WORKSPACE_PROTECTED"));
        assert!(managed
            .delete_index(None)
            .await
            .unwrap_err()
            .to_string()
            .contains("MANAGED_WORKSPACE_PROTECTED"));

        let personal = tempdir().unwrap();
        let user = scoped_ctx(personal.path(), serde_json::json!({ "kind": "user" }));
        assert!(!user.is_read_only());
        assert!(user.ensure_writable().is_ok());
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
            .open_built_index(
                dir.path().to_path_buf(),
                wilkes_core::embed::EmbeddingSpaceIdentity::for_runtime(
                    EmbeddingEngine::Candle,
                    "missing-model",
                    384,
                ),
            )
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
    async fn test_restore_state_skips_index_when_both_consumers_are_off() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        // A fully restorable index DB sits on disk with the default selection,
        // so only the absence of both runtime consumers can prevent restore.
        let selected = SelectedEmbedder::default();
        SemanticIndex::create(
            &ctx.data_dir,
            selected.model.model_id(),
            384,
            selected.engine,
            None,
        )
        .unwrap();

        // Persist settings with both semantic preference and index-backed exact
        // search off while the built index remains recorded on disk.
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

        // Positive control: the same index is structurally restorable, proving
        // the consumer settings are the gate rather than stale-selection reset.
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
        assert!(!ctx.is_index_loaded());
        assert!(!ctx.get_settings().await.search_prefer_semantic);
    }

    #[tokio::test]
    async fn restore_state_loads_index_without_embedder_for_exact_search() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let selected = SelectedEmbedder::default();
        SemanticIndex::create(
            &ctx.data_dir,
            selected.model.model_id(),
            selected.dimension,
            selected.engine,
            None,
        )
        .unwrap();
        let settings = Settings {
            search_prefer_semantic: false,
            grep_use_index: true,
            last_directory: Some(root),
            semantic: SemanticSettings {
                enabled: true,
                index_path: Some(ctx.data_dir.join("semantic_index.db")),
                selected,
                ..SemanticSettings::default()
            },
            ..Settings::default()
        };
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&settings).unwrap(),
        )
        .unwrap();

        Arc::clone(&ctx).restore_state().await;

        assert!(ctx.is_index_loaded());
        assert!(ctx.embedder.lock().is_none());
        assert!(!ctx.is_semantic_ready());
        assert!(ctx.directory_watcher.lock().is_some());
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

    /// The feature's reachability, end to end at this layer: a settings edit
    /// is what turns image enrichment on and off, and what extraction reads is
    /// the process-wide analyzer the edit moves.
    ///
    /// Enabling it here fails, because the test context has no recognizer
    /// installed — and that failure is the point twice over: it proves the
    /// edit is acted on rather than ignored, and it proves an enabled feature
    /// that cannot run says so instead of quietly extracting without it.
    #[tokio::test]
    async fn an_image_analysis_settings_edit_attaches_and_detaches_the_analyzer() {
        let (_dir, ctx) = test_ctx();
        assert!(
            wilkes_core::extract::image::configured().is_none(),
            "nothing is configured before anything asks for it"
        );

        ctx.update_settings(serde_json::json!({
            "image_analysis": { "enabled": true }
        }))
        .await
        .unwrap();
        assert!(
            ctx.load_image_analyzer().await.is_err(),
            "an enabled analyzer with no recognizer installed is a failure"
        );
        assert!(
            wilkes_core::extract::image::configured().is_none(),
            "a failed load detaches rather than leaving a stale analyzer"
        );

        ctx.update_settings(serde_json::json!({
            "image_analysis": { "enabled": false }
        }))
        .await
        .unwrap();
        assert!(wilkes_core::extract::image::configured().is_none());
        assert!(!ctx.is_image_recognizer_installed());
    }

    #[tokio::test]
    async fn semantic_pref_off_with_no_exact_consumer_releases_embedder_and_index() {
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

    /// A text probe reaches the embedder in the query role and lands where
    /// the caller put it, beside vectors the caller already held.
    #[tokio::test]
    async fn text_probes_resolve_in_place_beside_vector_probes() {
        let (_dir, ctx) = test_ctx();
        *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);

        let mut held = vec![0.0f32; 384];
        held[7] = 1.0;
        let resolved = ctx
            .resolve_search_probes(vec![
                ManagedSearchProbeInput::Text("what teaches this".to_string()),
                ManagedSearchProbeInput::Vector(held.clone()),
                ManagedSearchProbeInput::Text("and this".to_string()),
            ])
            .await
            .expect("probes resolve");

        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[1], held, "a held vector passes through untouched");
        assert_eq!(resolved[0].len(), 384);
        assert_eq!(resolved[2].len(), 384);
    }

    /// Without a text probe there is nothing to embed, so a search over held
    /// vectors must not require a live embedder.
    #[tokio::test]
    async fn vector_only_probes_need_no_embedder() {
        let (_dir, ctx) = test_ctx();
        assert!(ctx.embedder.lock().is_none());
        let resolved = ctx
            .resolve_search_probes(vec![ManagedSearchProbeInput::Vector(vec![1.0, 0.0])])
            .await
            .expect("vectors need no model");
        assert_eq!(resolved, vec![vec![1.0, 0.0]]);
    }

    #[tokio::test]
    async fn an_empty_text_probe_is_refused() {
        let (_dir, ctx) = test_ctx();
        *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
        let error = ctx
            .resolve_search_probes(vec![ManagedSearchProbeInput::Text("   ".to_string())])
            .await
            .expect_err("an empty probe names nothing");
        assert!(error.contains("empty"), "{error}");
    }

    #[tokio::test]
    async fn semantic_pref_off_retains_index_for_exact_search() {
        let (dir, ctx) = test_ctx();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&Settings {
                search_prefer_semantic: true,
                grep_use_index: true,
                last_directory: Some(root.clone()),
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "retained-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
        *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);
        ctx.start_directory_watcher(root);

        ctx.update_settings(serde_json::json!({ "search_prefer_semantic": false }))
            .await
            .unwrap();

        assert!(ctx.embedder.lock().is_none());
        assert!(ctx.is_index_loaded());
        assert!(ctx.directory_watcher.lock().is_some());
    }

    #[tokio::test]
    async fn exact_index_toggle_loads_and_releases_index_without_embedder() {
        let (dir, ctx) = test_ctx();
        let selected = SelectedEmbedder::default();
        SemanticIndex::create(
            &ctx.data_dir,
            selected.model.model_id(),
            selected.dimension,
            selected.engine,
            None,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&Settings {
                search_prefer_semantic: false,
                grep_use_index: false,
                semantic: SemanticSettings {
                    enabled: true,
                    index_path: Some(ctx.data_dir.join("semantic_index.db")),
                    selected,
                    ..SemanticSettings::default()
                },
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();

        ctx.update_settings(serde_json::json!({ "grep_use_index": true }))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !ctx.is_index_loaded() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("index-only activation should finish");
        assert!(ctx.embedder.lock().is_none());

        ctx.update_settings(serde_json::json!({ "grep_use_index": false }))
            .await
            .unwrap();
        assert!(!ctx.is_index_loaded());
        assert!(ctx.embedder.lock().is_none());
    }

    #[tokio::test]
    async fn disabling_exact_index_keeps_semantic_runtime_resident() {
        let (dir, ctx) = test_ctx();
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string(&Settings {
                search_prefer_semantic: true,
                grep_use_index: true,
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();
        let index = SemanticIndex::create(
            &ctx.data_dir,
            "semantic-model",
            384,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));
        *ctx.embedder.lock() = Some(Arc::new(MockEmbedder::default()) as Arc<dyn Embedder>);

        ctx.update_settings(serde_json::json!({ "grep_use_index": false }))
            .await
            .unwrap();

        assert!(ctx.is_index_loaded());
        assert!(ctx.embedder.lock().is_some());
        assert!(ctx.is_semantic_ready());
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
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder {
            model_id: "build-model".to_string(),
            ..MockEmbedder::default()
        });

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

        let patch = serde_json::json!({
            "context_lines": 5,
            "max_file_size": 23 * 1024 * 1024
        });
        let updated = ctx.update_settings(patch).await.unwrap();
        assert_eq!(updated.context_lines, 5);
        assert_eq!(
            wilkes_agent::search::SearchService::max_search_file_size(ctx.clone()).await,
            23 * 1024 * 1024
        );

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
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "body without the query terms").unwrap();
        let canonical_path = std::fs::canonicalize(&path).unwrap();
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
        let identity = FileIdentity::for_path(&canonical_path).unwrap();
        ctx.metadata_cache()
            .unwrap()
            .lock()
            .unwrap()
            .upsert(
                &canonical_path,
                identity,
                &DocumentMetadata {
                    title: Some("Composed Search Title".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();

        let query = SearchQuery {
            pattern: "composed search title".to_string(),
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

        let mut handle = ctx.clone().start_search(query).await.unwrap();
        let result = handle.next().await.unwrap();
        assert_eq!(result.path, canonical_path);
        assert!(result.matches.is_empty());
        assert_eq!(result.field_matches.len(), 1);
        assert_eq!(
            result.field_matches[0].field,
            wilkes_core::types::SearchField::Title
        );
        assert!(handle.next().await.is_none());
        assert!(handle.finish().await.is_empty());
    }

    #[tokio::test]
    async fn exact_search_setting_routes_the_shared_index_and_reports_fallbacks() {
        let dir = tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let pdf = root.join("paper.pdf");
        // Intentionally not a valid PDF: successful matching proves the app
        // boundary supplied indexed text instead of silently extracting live.
        std::fs::write(&pdf, "%PDF-1.7 opaque bytes").unwrap();

        let emitter = Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let (ctx, _rx, _loop) = AppContext::new(
            root.clone(),
            root.join("settings.json"),
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
            serde_json::json!({
                "last_directory": root,
                "grep_use_index": true
            }),
        )
        .await
        .unwrap();

        let mut index = SemanticIndex::create(
            &ctx.data_dir,
            "test-model",
            3,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        index
            .write_file(PreparedFile {
                path: pdf.clone(),
                full_text: "alpha beta gamma".to_string(),
                chunks: vec![(
                    Chunk {
                        file_path: pdf.clone(),
                        text: "alpha beta gamma".to_string(),
                        byte_range: ByteRange { start: 0, end: 16 },
                        origin: SourceOrigin::PdfPage {
                            page: 1,
                            bbox: None,
                        },
                    },
                    vec![1.0, 0.0, 0.0],
                )],
            })
            .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(Some(index)));

        let query = SearchQuery {
            pattern: "beta".to_string(),
            is_regex: false,
            case_sensitive: true,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 0,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: SearchScope::File { path: pdf.clone() },
            supported_extensions: Vec::new(),
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let mut indexed_results = Vec::new();
        let indexed_stats = ctx
            .clone()
            .start_search(query.clone())
            .await
            .unwrap()
            .run(|result| {
                indexed_results.push(result);
                async { true }
            })
            .await;
        assert_eq!(indexed_results.len(), 1);
        assert_eq!(indexed_results[0].matches[0].matched_text, "beta");
        assert_eq!(indexed_stats.indexed_pdf_reads, 1);
        assert_eq!(indexed_stats.live_pdf_fallbacks, 0);
        assert_eq!(indexed_stats.index_unavailable_fallbacks, 0);
        assert!(indexed_stats.errors.is_empty());

        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "grep_use_index": false }),
        )
        .await
        .unwrap();
        let disabled_stats = ctx
            .clone()
            .start_search(query.clone())
            .await
            .unwrap()
            .run(|_| async { true })
            .await;
        assert_eq!(disabled_stats.indexed_pdf_reads, 0);
        assert_eq!(disabled_stats.live_pdf_fallbacks, 1);
        assert_eq!(disabled_stats.index_unavailable_fallbacks, 0);
        assert_eq!(disabled_stats.errors.len(), 1);

        crate::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "grep_use_index": true }),
        )
        .await
        .unwrap();
        *ctx.index.lock() = Arc::new(Mutex::new(None));
        std::fs::remove_file(ctx.data_dir.join("semantic_index.db")).unwrap();
        let unavailable_stats = ctx
            .clone()
            .start_search(query)
            .await
            .unwrap()
            .run(|_| async { true })
            .await;
        assert_eq!(unavailable_stats.indexed_pdf_reads, 0);
        assert_eq!(unavailable_stats.live_pdf_fallbacks, 1);
        assert_eq!(unavailable_stats.index_unavailable_fallbacks, 1);
        assert_eq!(unavailable_stats.errors.len(), 1);
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
        // Seed each row under the file's real identity. A fabricated identity
        // would make every listing treat the file as uncached and schedule a
        // background re-extraction, which would then own (and overwrite) the
        // DOI these edges are keyed by.
        let source_id = FileIdentity::for_path(&source).unwrap();
        let cited_id = FileIdentity::for_path(&cited).unwrap();
        {
            let guard = cache.lock().unwrap();
            guard
                .upsert(
                    &source,
                    source_id,
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
                    cited_id,
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
