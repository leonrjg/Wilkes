use parking_lot::{Mutex as PLMutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::{mpsc, Mutex};
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::{EmbeddingSpaceIdentity, ExtractionRecipe};
use wilkes_core::types::{SelectedEmbedder, SemanticSettings};
use wilkes_core::worker::manager::{ManagerEvent, WorkerPaths};

use wilkes_core::consumer::{consumer_error, ConsumerError, ConsumerErrorCode};
use wilkes_core::{consumer_bail, consumer_ensure};

use crate::commands::settings::get_scoped_settings;
use crate::context::{
    AppContext, EventAudience, EventEmitter, IndexSpace, ManagedCorpusBackup, ViewEventSink,
};

const REGISTRY_VERSION: u32 = 1;
const MANIFEST_VERSION: u32 = 1;

mod relocation;
use relocation::UploadRelocation;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub active_root: Option<PathBuf>,
    /// The embedder this workspace indexes with, after the manifest is merged
    /// over global settings.
    ///
    /// Reported rather than left to be discovered: each workspace owns its own
    /// index, so vectors from two workspaces are only comparable when this
    /// matches. A consumer that keeps one vector space across workspaces
    /// (Underdog pins model + dimension) can only tell which workspaces it may
    /// draw from if the listing says so — otherwise it finds out by exporting
    /// a document and failing.
    pub embedding: SelectedEmbedder,
    /// Whether the user may only read this workspace.
    ///
    /// Reported rather than expressed by leaving the row out. An
    /// application-managed corpus is protected from being *written* — the
    /// import API is its only writer — and dropping it from the listing was a
    /// second, cruder statement of the same protection, one that also cost the
    /// user any way to look inside a corpus whose documents sit on their own
    /// disk. The protections live where the writes do
    /// (`AppContext::ensure_writable`, `update_scoped_settings`, and the
    /// refusal to watch or reindex a managed root), so the listing is free to
    /// name the workspace and say what it is.
    pub read_only: bool,
    /// The application that owns a read-only workspace, so the user can see
    /// whose corpus they are looking at. `None` for an ordinary workspace.
    pub managed_by: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceState {
    /// The workspace the asker shows: a window's own, or the default for a
    /// request that comes from no window.
    pub active_workspace_id: String,
    pub workspaces: Vec<WorkspaceSummary>,
    /// Workspaces other windows show. Switching to one of these is refused;
    /// the asker brings that window forward instead.
    #[serde(default)]
    pub open_elsewhere: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceKind {
    #[default]
    User,
    ApplicationManaged {
        owner: String,
        purpose: String,
        corpus_key: String,
        /// `None` is the canonical corpus workspace. A value names the
        /// canonical corpus whose membership this internal workspace projects
        /// into one additional embedding space.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_corpus_id: Option<String>,
    },
}

/// How a consumer request names the index it is asking about. The whole of it:
/// every chunk, embed and export route takes this object and nothing else for
/// addressing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsumerScope {
    /// Absent means the active workspace. A managed corpus is addressed by
    /// putting its `corpus_id` here — they are the same token, and pretending
    /// otherwise was the adapter's doing rather than the data model's.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The embedding space the caller believes it is talking to. Optional on a
    /// user workspace, where it verifies; required on a managed corpus, where
    /// it also routes.
    #[serde(default)]
    pub expected_embedding_space_id: Option<String>,
}

/// An opened index and what it can be asked.
pub struct ConsumerIndex {
    context: Arc<AppContext>,
    space: IndexSpace,
}

/// The context behind an opened index is a live workspace, not a value; what
/// is worth printing about one of these is which space it turned out to be.
impl std::fmt::Debug for ConsumerIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerIndex")
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}

impl ConsumerIndex {
    /// An opened index, checked against the pin the caller supplied.
    ///
    /// This is the whole of the rule for an index that is addressed as
    /// itself — a user workspace, or the one context a handler test owns. A
    /// pin only ever verifies here: an index that cannot prove which space it
    /// is fails exactly as an index of another space does, because neither can
    /// be shown to be the one the caller named.
    pub fn verified(
        context: Arc<AppContext>,
        space: IndexSpace,
        expected_embedding_space_id: Option<&str>,
    ) -> Result<Self, ConsumerError> {
        if let Some(pin) = expected_embedding_space_id {
            if space.id() != Some(pin) {
                return Err(ConsumerError::new(
                    ConsumerErrorCode::EmbeddingSpaceMismatch,
                    format!("index={}, request={pin}", space.id().unwrap_or("none")),
                ));
            }
        }
        Ok(Self { context, space })
    }

    pub fn context(&self) -> &Arc<AppContext> {
        &self.context
    }

    pub fn into_context(self) -> Arc<AppContext> {
        self.context
    }

    /// The space id to report, which is `None` exactly when the index cannot
    /// prove one — no index, or one built before schema v10.
    pub fn embedding_space_id(&self) -> Option<&str> {
        self.space.id()
    }

    /// The space id, for a route that is about to name passages.
    ///
    /// An index without chunk refs refuses here rather than degrading: every
    /// ref it could return would be null, and a shorter answer would look
    /// like a complete one.
    pub fn addressable_space_id(&self) -> Result<&str, ConsumerError> {
        match &self.space {
            IndexSpace::Exact(id) => Ok(id.as_str()),
            IndexSpace::Unverified => Err(ConsumerError::new(
                ConsumerErrorCode::IndexIdentityUnverified,
                "this index predates stable chunk references and cannot address a passage; \
                 rebuild it",
            )),
            IndexSpace::Absent => Err(ConsumerError::untyped(
                "Semantic index unavailable. Build or restore the semantic index first.",
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnsureManagedWorkspace {
    /// The application this corpus belongs to, as it appears in
    /// [`WorkspaceKind::ApplicationManaged`].
    ///
    /// Supplied rather than compiled in: a managed semantic corpus is a thing
    /// Wilkes offers, not a thing one named consumer is. The manifests already
    /// carry the owner, so a consumer that sends its own name matches the
    /// corpus it already had — no migration.
    pub owner: String,
    pub corpus_key: String,
    pub embedding: SelectedEmbedder,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ManagedWorkspaceStatus {
    /// Opaque corpus token. It is implemented by a workspace id but does not
    /// grant generic workspace capabilities.
    pub corpus_id: String,
    /// Absent until the corpus has an index. There is no embedding space
    /// before one exists, and the id a future build will produce cannot be
    /// known from configuration alone.
    pub embedding_space_id: Option<String>,
    pub embedding_space_identity: Option<EmbeddingSpaceIdentity>,
    pub extraction_recipe_id: String,
    pub ready: bool,
    pub indexed_documents: usize,
    pub indexed_chunks: usize,
    pub required_chunks: usize,
    pub embedded_chunks: usize,
    pub reused_chunks: usize,
    pub computed_chunks: usize,
    pub managed_source_bytes: u64,
    pub temporary_bytes: u64,
    pub index_bytes: u64,
    pub total_managed_bytes: u64,
    pub integrity_checked_at_ms: i64,
    pub pending_imports: u64,
    pub pending_builds: u64,
    /// Stable digest of ready rendition/chunk membership. All routable spaces
    /// beneath one corpus must report the canonical workspace's digest.
    pub corpus_generation: String,
    /// Every embedding projection owned by this logical corpus. Child
    /// projection workspaces are implementation details and never become
    /// independent corpus ids on this API.
    pub spaces: Vec<ManagedEmbeddingSpaceStatus>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ManagedEmbeddingSpaceStatus {
    pub embedding_space_id: String,
    pub embedding_space_identity: EmbeddingSpaceIdentity,
    pub ready: bool,
    pub indexed_generation: String,
    pub workspace_id: String,
    pub primary: bool,
}

/// How a refusal names the spaces a corpus holds: `none`, or their ids.
///
/// A refusal used to name the corpus's own `embedding_space_id`, which is
/// absent by design — the canonical corpus holds passages and no vectors — so
/// a corpus with three projections was reported as `corpus=none`.
pub fn describe_corpus_spaces(spaces: &[ManagedEmbeddingSpaceStatus]) -> String {
    if spaces.is_empty() {
        return "none".to_string();
    }
    spaces
        .iter()
        .map(|space| space.embedding_space_id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// What a caller refused for naming a space the corpus does not hold can do
/// about it. The step a first-time consumer misses is the one that matters:
/// `PUT /api/corpora` takes an embedding and creates no vectors, and nothing
/// in its reply says that searching needs a projection.
pub fn corpus_spaces_remedy(spaces: &[ManagedEmbeddingSpaceStatus]) -> &'static str {
    if spaces.is_empty() {
        "the corpus holds passages and no vectors until a projection embeds them: \
         PUT /api/corpora/spaces with its corpus_id and an embedding creates one, and its \
         embedding_space_id is the pin"
    } else {
        "pin one of the corpus's spaces; GET /api/corpora/status lists them, and \
         PUT /api/corpora/spaces creates another"
    }
}

/// What routing a request to a corpus needs to know: its membership
/// generation and the projections that hold an index. See
/// [`WorkspaceManager::managed_spaces`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedSpaces {
    pub corpus_generation: String,
    pub spaces: Vec<ManagedEmbeddingSpaceStatus>,
}

impl ManagedSpaces {
    /// The spaces level with the corpus: ready, and holding exactly the
    /// membership it admitted. Each one holds every document the corpus does,
    /// so each is a pin a caller may send with its next import or search.
    pub fn level_space_ids(&self) -> Vec<String> {
        self.spaces
            .iter()
            .filter(|space| space.ready && space.indexed_generation == self.corpus_generation)
            .map(|space| space.embedding_space_id.clone())
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsureManagedEmbeddingSpace {
    pub corpus_id: String,
    pub embedding: SelectedEmbedder,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceManifest {
    #[serde(default = "manifest_version")]
    version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: WorkspaceKind,
    #[serde(default)]
    pub favorites: Vec<PathBuf>,
    #[serde(default)]
    pub recent_roots: Vec<PathBuf>,
    #[serde(default)]
    pub active_root: Option<PathBuf>,
    #[serde(default)]
    pub semantic: Option<SemanticSettings>,
}

fn manifest_version() -> u32 {
    MANIFEST_VERSION
}

impl WorkspaceManifest {
    fn new(id: String, name: String) -> Self {
        Self {
            version: MANIFEST_VERSION,
            id,
            name,
            kind: WorkspaceKind::User,
            favorites: Vec::new(),
            recent_roots: Vec::new(),
            active_root: None,
            semantic: None,
        }
    }

    /// A new application-managed workspace: a canonical corpus when
    /// `parent_corpus_id` is `None`, otherwise one embedding projection of
    /// that corpus. Its `managed_sources` directory is created here, because
    /// the manifest names it as the workspace's one root.
    fn application_managed(
        app_data_dir: &Path,
        name: String,
        owner: &str,
        corpus_key: &str,
        parent_corpus_id: Option<&str>,
        mut semantic: SemanticSettings,
    ) -> anyhow::Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let root = workspace_root(app_data_dir, &id);
        let managed_sources = root.join("managed_sources");
        std::fs::create_dir_all(&managed_sources)?;
        semantic.index_path = Some(root.join("semantic_index.db"));
        Ok(Self {
            version: MANIFEST_VERSION,
            id,
            name,
            kind: WorkspaceKind::ApplicationManaged {
                owner: owner.to_string(),
                purpose: "semantic-corpus".to_string(),
                corpus_key: corpus_key.to_string(),
                parent_corpus_id: parent_corpus_id.map(str::to_string),
            },
            favorites: vec![managed_sources.clone()],
            recent_roots: Vec::new(),
            active_root: Some(managed_sources),
            semantic: Some(semantic),
        })
    }

    pub(crate) fn is_application_managed(&self) -> bool {
        matches!(self.kind, WorkspaceKind::ApplicationManaged { .. })
    }

    fn summary(&self, embedding: SelectedEmbedder) -> WorkspaceSummary {
        let mut roots = self.favorites.clone();
        for root in &self.recent_roots {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        if let Some(root) = &self.active_root {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        let managed_by = match &self.kind {
            WorkspaceKind::User => None,
            WorkspaceKind::ApplicationManaged { owner, .. } => Some(owner.clone()),
        };
        WorkspaceSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            roots,
            active_root: self.active_root.clone(),
            embedding,
            read_only: managed_by.is_some(),
            managed_by,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkspaceRegistryFile {
    version: u32,
    /// The default workspace: the one shown by the window focused last, and
    /// the one a request naming no workspace is answered from.
    active_workspace_id: String,
    workspace_ids: Vec<String>,
}

/// The directory every workspace's own directory sits in.
pub(crate) fn workspaces_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("workspaces")
}

pub(crate) fn workspace_root(app_data_dir: &Path, id: &str) -> PathBuf {
    workspaces_dir(app_data_dir).join(id)
}

pub(crate) fn workspace_manifest_path(app_data_dir: &Path, id: &str) -> PathBuf {
    workspace_root(app_data_dir, id).join("workspace.json")
}

fn registry_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("workspaces.json")
}

fn contains_legacy_library(app_data_dir: &Path) -> bool {
    [
        "semantic_index.db",
        "semantic_index.db.tmp",
        "semantic_index.db.replacement-backup",
        "file_metadata.db",
        "research.db",
        "chat-conversations.json",
        "bookmarks.json",
        "uploads",
    ]
    .iter()
    .any(|name| app_data_dir.join(name).exists())
}

fn settings_contain_legacy_roots(settings_path: &Path) -> anyhow::Result<bool> {
    if !settings_path.exists() {
        return Ok(false);
    }
    let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(settings_path)?)?;
    Ok(settings
        .get("favorites")
        .or_else(|| settings.get("bookmarked_dirs"))
        .and_then(|value| value.as_array())
        .is_some_and(|roots| !roots.is_empty())
        || settings
            .get("recent_dirs")
            .and_then(|value| value.as_array())
            .is_some_and(|roots| !roots.is_empty())
        || settings
            .get("last_directory")
            .is_some_and(|root| !root.is_null()))
}

/// Everything a pre-workspace ("alpha") library kept beside the settings
/// file, in the order it is moved into the workspace that adopts it.
///
/// Broader than [`contains_legacy_library`]'s list on purpose: that one names
/// the files whose presence proves an old library exists, while this one has
/// to name every companion file too — a SQLite database left without its
/// `-wal` sibling loses the writes that sibling holds.
const LEGACY_LIBRARY_ENTRIES: &[&str] = &[
    "semantic_index.db",
    "semantic_index.db-wal",
    "semantic_index.db-shm",
    "semantic_index.db.tmp",
    "semantic_index.db.tmp-wal",
    "semantic_index.db.tmp-shm",
    "semantic_index.db.replacement-backup",
    "semantic_index.status.json",
    "file_metadata.db",
    "file_metadata.db-wal",
    "file_metadata.db-shm",
    "research.db",
    "research.db-wal",
    "research.db-shm",
    "chat-conversations.json",
    "bookmarks.json",
    "bookmarks.json.migrated",
    "uploads",
];

/// Records the workspace an interrupted migration had already committed to.
///
/// Written before the first file moves: once anything has moved, a second
/// attempt must adopt the same workspace id and the same manifest, or it would
/// leave half the library under one id and half under another.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct MigrationPlan {
    workspace_id: String,
    manifest: WorkspaceManifest,
    #[serde(default)]
    upload_relocations: Vec<UploadRelocation>,
}

fn migration_plan_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(".workspace-migration.json")
}

struct LegacyRoots {
    favorites: Vec<PathBuf>,
    recent_roots: Vec<PathBuf>,
    active_root: Option<PathBuf>,
    semantic: Option<SemanticSettings>,
}

/// The workspace-owned half of a pre-workspace settings file.
///
/// Every field is genuinely optional — an alpha install that never opened a
/// directory wrote none of them — but a value that is present and unreadable
/// is a fault, not an absence, and is reported rather than dropped: those
/// roots are the only record of what the user had open.
fn read_legacy_roots(settings_path: &Path) -> anyhow::Result<LegacyRoots> {
    if !settings_path.exists() {
        return Ok(LegacyRoots {
            favorites: Vec::new(),
            recent_roots: Vec::new(),
            active_root: None,
            semantic: None,
        });
    }
    let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(settings_path)?)?;
    let field = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| settings.get(*name))
            .filter(|value| !value.is_null())
            .cloned()
    };
    fn parse<T: serde::de::DeserializeOwned>(
        value: Option<serde_json::Value>,
        name: &str,
    ) -> anyhow::Result<Option<T>> {
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| anyhow::anyhow!("legacy setting {name} is unreadable: {error}"))
    }
    Ok(LegacyRoots {
        favorites: parse(field(&["favorites", "bookmarked_dirs"]), "favorites")?
            .unwrap_or_default(),
        recent_roots: parse(field(&["recent_dirs"]), "recent_dirs")?.unwrap_or_default(),
        active_root: parse(field(&["last_directory"]), "last_directory")?,
        semantic: parse(field(&["semantic"]), "semantic")?,
    })
}

/// Drops the keys the adopting workspace manifest now owns, leaving every
/// other global preference untouched. Left in place they would be a second,
/// stale answer to which roots are open.
fn clear_legacy_roots(settings_path: &Path) -> anyhow::Result<()> {
    if !settings_path.exists() {
        return Ok(());
    }
    let mut settings: serde_json::Value = serde_json::from_slice(&std::fs::read(settings_path)?)?;
    let Some(object) = settings.as_object_mut() else {
        anyhow::bail!(
            "settings file {} is not a JSON object",
            settings_path.display()
        );
    };
    for key in [
        "favorites",
        "bookmarked_dirs",
        "recent_dirs",
        "last_directory",
        "semantic",
    ] {
        object.remove(key);
    }
    atomic_write_json(settings_path, &settings)
}

/// Where each surviving legacy entry has to move, refusing rather than
/// choosing when the same entry exists in both the data and the config
/// directory: only the user knows which of two libraries is the real one.
fn plan_legacy_moves(
    app_data_dir: &Path,
    settings_path: &Path,
    workspace_dir: &Path,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let mut source_dirs = vec![app_data_dir.to_path_buf()];
    if let Some(config_dir) = settings_path.parent() {
        if config_dir != app_data_dir {
            source_dirs.push(config_dir.to_path_buf());
        }
    }

    let mut moves = Vec::new();
    for entry in LEGACY_LIBRARY_ENTRIES {
        let candidates: Vec<PathBuf> = source_dirs
            .iter()
            .map(|dir| dir.join(entry))
            .filter(|path| path.exists())
            .collect();
        anyhow::ensure!(
            candidates.len() <= 1,
            "multiple pre-workspace copies of {entry} exist; refusing to choose between {}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(source) = candidates.into_iter().next() {
            let destination = workspace_dir.join(entry);
            moves.push((source, destination));
        }
    }
    Ok(moves)
}

/// Adopts a pre-workspace ("alpha") library into a Default workspace, in
/// place, at startup.
///
/// Done by the application rather than by a script the user is told to run:
/// the migration is mechanical — the whole library becomes one workspace, and
/// there is nothing to decide — so a startup screen asking for it only cost
/// every alpha user a manual step to reach a state the app could have reached
/// itself.
///
/// Resumable rather than transactional: files move one at a time and the run
/// can be interrupted between any two. The plan file records the workspace id
/// and manifest before the first move, so a second attempt continues the same
/// migration instead of starting a rival one, and the registry — the thing
/// that makes the workspace real — is written only once every file has landed.
fn migrate_legacy_library(app_data_dir: &Path, settings_path: &Path) -> anyhow::Result<String> {
    let plan_path = migration_plan_path(app_data_dir);
    let (id, mut manifest, mut upload_relocations) = if plan_path.exists() {
        let plan: MigrationPlan = serde_json::from_slice(&std::fs::read(&plan_path)?)?;
        anyhow::ensure!(
            plan.manifest.id == plan.workspace_id,
            "interrupted workspace migration records a manifest for a different workspace"
        );
        (plan.workspace_id, plan.manifest, plan.upload_relocations)
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let legacy = read_legacy_roots(settings_path)?;
        let mut manifest = WorkspaceManifest::new(id.clone(), "Default".to_string());
        manifest.favorites = legacy.favorites;
        manifest.recent_roots = legacy.recent_roots;
        manifest.active_root = legacy.active_root;
        manifest.semantic = legacy.semantic;
        (id, manifest, Vec::new())
    };

    let workspace_dir = workspace_root(app_data_dir, &id);
    let moves = plan_legacy_moves(app_data_dir, settings_path, &workspace_dir)?;
    // Also recover plans written by older builds, including after uploads
    // have already moved. New plans retain canonical aliases before moving.
    if upload_relocations.is_empty() {
        upload_relocations = relocation::plan(app_data_dir, settings_path, &workspace_dir)?;
    }
    relocation::remap_manifest(&mut manifest, &upload_relocations);
    tracing::info!(
        workspace = %id,
        entries = moves.len(),
        "adopting pre-workspace library into a Default workspace"
    );

    std::fs::create_dir_all(app_data_dir)?;
    atomic_write_json(
        &plan_path,
        &MigrationPlan {
            workspace_id: id.clone(),
            manifest: manifest.clone(),
            upload_relocations: upload_relocations.clone(),
        },
    )?;
    std::fs::create_dir_all(&workspace_dir)?;
    for (source, destination) in moves {
        anyhow::ensure!(
            !destination.exists(),
            "workspace migration would overwrite {}",
            destination.display()
        );
        std::fs::rename(&source, &destination).map_err(|error| {
            anyhow::anyhow!(
                "could not move {} into the workspace at {}: {error}",
                source.display(),
                destination.display()
            )
        })?;
    }

    relocation::remap_stores(&workspace_dir, &upload_relocations)?;
    write_manifest(&workspace_manifest_path(app_data_dir, &id), &manifest)?;
    clear_legacy_roots(settings_path)?;
    atomic_write_json(
        &registry_path(app_data_dir),
        &WorkspaceRegistryFile {
            version: REGISTRY_VERSION,
            active_workspace_id: id.clone(),
            workspace_ids: vec![id.clone()],
        },
    )?;
    std::fs::remove_file(&plan_path)?;
    Ok(id)
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

pub(crate) fn read_manifest(path: &Path) -> anyhow::Result<WorkspaceManifest> {
    let _guard = manifest_lock().lock();
    read_manifest_unlocked(path)
}

fn read_manifest_unlocked(path: &Path) -> anyhow::Result<WorkspaceManifest> {
    let manifest: WorkspaceManifest = serde_json::from_slice(&std::fs::read(path)?)?;
    anyhow::ensure!(
        manifest.version == MANIFEST_VERSION,
        "workspace manifest version {} is not supported",
        manifest.version
    );
    Ok(manifest)
}

/// Adds a new workspace to the registry: its manifest first, then its id.
///
/// The one way a workspace comes to exist after the first, whoever asks for
/// it — the user, an application's corpus, or that corpus's projection. The
/// caller holds `registry_lock` and hands in the registry it loaded under it.
fn register_workspace(
    app_data_dir: &Path,
    registry: &mut WorkspaceRegistryFile,
    manifest: &WorkspaceManifest,
) -> anyhow::Result<()> {
    write_manifest(
        &workspace_manifest_path(app_data_dir, &manifest.id),
        manifest,
    )?;
    registry.workspace_ids.push(manifest.id.clone());
    atomic_write_json(&registry_path(app_data_dir), registry)
}

pub(crate) fn write_manifest(path: &Path, manifest: &WorkspaceManifest) -> anyhow::Result<()> {
    let _guard = manifest_lock().lock();
    atomic_write_json(path, manifest)
}

pub(crate) fn update_manifest(
    path: &Path,
    update: impl FnOnce(&mut WorkspaceManifest) -> anyhow::Result<()>,
) -> anyhow::Result<WorkspaceManifest> {
    let _guard = manifest_lock().lock();
    let mut manifest = read_manifest_unlocked(path)?;
    update(&mut manifest)?;
    atomic_write_json(path, &manifest)?;
    Ok(manifest)
}

fn manifest_lock() -> &'static PLMutex<()> {
    static LOCK: OnceLock<PLMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| PLMutex::new(()))
}

fn load_registry(app_data_dir: &Path) -> anyhow::Result<WorkspaceRegistryFile> {
    let path = registry_path(app_data_dir);
    anyhow::ensure!(
        path.exists(),
        "No workspace registry exists at {}. Run the workspace migration script before starting this build.",
        path.display()
    );
    let registry: WorkspaceRegistryFile = serde_json::from_slice(&std::fs::read(&path)?)?;
    anyhow::ensure!(
        registry.version == REGISTRY_VERSION,
        "workspace registry version {} is not supported",
        registry.version
    );
    anyhow::ensure!(
        registry
            .workspace_ids
            .contains(&registry.active_workspace_id),
        "active workspace is absent from the registry"
    );
    Ok(registry)
}

/// Reports the id of the active workspace, creating the registry if this
/// installation has none: a fresh install gets one empty Default workspace,
/// and a pre-workspace ("alpha") install has its existing library adopted into
/// one by [`migrate_legacy_library`].
///
/// One entry point rather than a registry-creating path beside a separate
/// migration the user has to invoke: whether an installation predates
/// workspaces is answered by looking at it, and answering it twice — once here
/// and once by whoever decided to run a script — is how an install ends up
/// with both a fresh empty registry and an unadopted library.
///
/// The id rather than a [`WorkspaceState`]: describing the registry is
/// [`read_workspace_state`]'s job, and it has to read every manifest and the
/// settings that merge over them to do it. Creating a registry needs none of
/// that, and callers here are starting a manager, not rendering a list.
pub fn initialize_workspace_registry(
    app_data_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<String> {
    let path = registry_path(app_data_dir);
    if path.exists() {
        return Ok(load_registry(app_data_dir)?.active_workspace_id);
    }
    // The plan file first: an interrupted migration may already have moved
    // every file out of the way, leaving nothing for the other two checks to
    // recognize, and finishing it is still the only correct outcome.
    if migration_plan_path(app_data_dir).exists()
        || contains_legacy_library(app_data_dir)
        || settings_contain_legacy_roots(settings_path)?
    {
        return migrate_legacy_library(app_data_dir, settings_path);
    }
    std::fs::create_dir_all(app_data_dir)?;
    let id = uuid::Uuid::new_v4().to_string();
    let manifest = WorkspaceManifest::new(id.clone(), "Default".to_string());
    write_manifest(&workspace_manifest_path(app_data_dir, &id), &manifest)?;
    atomic_write_json(
        &path,
        &WorkspaceRegistryFile {
            version: REGISTRY_VERSION,
            active_workspace_id: id.clone(),
            workspace_ids: vec![id.clone()],
        },
    )?;
    Ok(id)
}

/// Every workspace, each described with the roots and the embedder it would be
/// served with — without activating any of them.
///
/// Async because the embedder is not the manifest's alone: a workspace that
/// declares no `semantic` block inherits the global one, and
/// [`get_scoped_settings`] is where that merge lives. Reading it here rather
/// than restating the precedence keeps one answer to "what does this
/// workspace embed with".
pub async fn read_workspace_state(
    app_data_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<WorkspaceState> {
    let registry = load_registry(app_data_dir)?;
    let mut workspaces = Vec::with_capacity(registry.workspace_ids.len());
    for id in &registry.workspace_ids {
        let manifest_path = workspace_manifest_path(app_data_dir, id);
        let manifest = read_manifest(&manifest_path)?;
        anyhow::ensure!(
            manifest.id == *id,
            "workspace manifest id mismatch for {id}"
        );
        let settings = get_scoped_settings(settings_path, &manifest_path).await?;
        if matches!(
            manifest.kind,
            WorkspaceKind::ApplicationManaged {
                parent_corpus_id: Some(_),
                ..
            }
        ) {
            continue;
        }
        workspaces.push(manifest.summary(settings.semantic.selected));
    }
    Ok(WorkspaceState {
        active_workspace_id: registry.active_workspace_id,
        workspaces,
        open_elsewhere: Vec::new(),
    })
}

/// The label of the view a manager starts with: the desktop app's main
/// window, or the web UI in front of `wilkes-server`.
pub const PRIMARY_VIEW: &str = "main";

/// A view asked for a workspace another view already shows.
///
/// Refused rather than allowed because a window keeps state of its own per
/// workspace — open editors, viewer tabs, folder state — and two windows on
/// one workspace would each write over the other's. The shell's remedy is to
/// bring `view` forward instead.
#[derive(Debug)]
pub struct WorkspaceShownElsewhere {
    pub workspace_id: String,
    pub view: String,
}

impl std::fmt::Display for WorkspaceShownElsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "This workspace is already open in another window")
    }
}

impl std::error::Error for WorkspaceShownElsewhere {}

/// Which workspace each view shows, and which is the default.
///
/// Its own lock, shared with every context's event emitter, because routing
/// an event needs exactly this and nothing else. Whenever both this and the
/// manager's map of contexts are taken, this is taken first.
struct Views {
    /// The workspace answering whatever names none: the HTTP API, an MCP
    /// tool given no workspace, a window that shows none (the standalone
    /// reader). The workspace of the view focused last, persisted as the
    /// registry's `active_workspace_id`. Its context is always open.
    default: String,
    /// The workspace each view shows, by view label. One view per workspace:
    /// see [`WorkspaceShownElsewhere`].
    ///
    /// Not persisted: a window other than the primary one is not reopened at
    /// launch. Reopening them would open a context per window in a loop, and
    /// a context's restore loads the models its settings enable — starting
    /// workers in a loop.
    shown: std::collections::HashMap<String, String>,
}

impl Views {
    fn viewer_of(&self, workspace_id: &str) -> Option<&str> {
        self.shown
            .iter()
            .find(|(_, shown)| shown.as_str() == workspace_id)
            .map(|(view, _)| view.as_str())
    }

    /// Whether anything still needs this workspace's context open.
    fn holds(&self, workspace_id: &str) -> bool {
        self.default == workspace_id || self.viewer_of(workspace_id).is_some()
    }

    fn bind(&mut self, view: &str, workspace_id: &str) {
        self.shown
            .insert(view.to_string(), workspace_id.to_string());
    }

    fn unbind(&mut self, view: &str) -> Option<String> {
        self.shown.remove(view)
    }
}

pub struct WorkspaceManager {
    app_data_dir: PathBuf,
    settings_path: PathBuf,
    events: Arc<dyn ViewEventSink>,
    views: Arc<RwLock<Views>>,
    /// Every context this manager has open, one per workspace: those shown
    /// in a view, the default's, and those a request opened by naming a
    /// workspace ([`Self::context_for`]).
    ///
    /// One map, so "a workspace's databases are held by one context" is what
    /// the structure says rather than something two maps are kept in step to
    /// mean. Kept rather than opened per request because a context owns the
    /// index and metadata handles for its workspace: reopening them for every
    /// exported document would pay that cost per file.
    contexts: PLMutex<std::collections::HashMap<String, Arc<AppContext>>>,
    registry_lock: PLMutex<()>,
    switch_lock: Mutex<()>,
    /// Projection builds in flight, keyed by projection workspace id. See
    /// [`WorkspaceManager::build_projection`].
    projection_builds: PLMutex<std::collections::HashMap<String, ProjectionBuild>>,
    /// Tells one build from the next under the same projection id, so a
    /// finished build removes its own entry and never its successor's.
    projection_build_serial: std::sync::atomic::AtomicU64,
    /// This process's hold on the data directory. Last, so it is released
    /// only after every other field — every context and its databases — has
    /// been dropped.
    _installation: crate::installation::InstallationLock,
}

/// One projection build, as a request waiting on it sees it.
#[derive(Clone)]
struct ProjectionBuild {
    serial: u64,
    corpus_id: String,
    outcome: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
}

/// Removes a build's entry when its task ends, however it ends. A build that
/// panicked would otherwise leave an entry every later caller waits on.
struct ProjectionBuildSlot {
    manager: Arc<WorkspaceManager>,
    projection_id: String,
    serial: u64,
}

impl Drop for ProjectionBuildSlot {
    fn drop(&mut self) {
        let mut builds = self.manager.projection_builds.lock();
        if builds
            .get(&self.projection_id)
            .is_some_and(|build| build.serial == self.serial)
        {
            builds.remove(&self.projection_id);
        }
    }
}

struct WorkspaceEventEmitter {
    workspace_id: String,
    views: Arc<RwLock<Views>>,
    inner: Arc<dyn ViewEventSink>,
}

impl EventEmitter for WorkspaceEventEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let (views, default) = {
            let views = self.views.read();
            (
                views
                    .viewer_of(&self.workspace_id)
                    .map(str::to_string)
                    .into_iter()
                    .collect::<Vec<_>>(),
                views.default == self.workspace_id,
            )
        };
        // A workspace nobody is looking at — one the HTTP API or an MCP tool
        // opened by naming it — reports to no one, as it always has.
        if views.is_empty() && !default {
            return;
        }
        self.inner.emit_to(
            EventAudience {
                views: &views,
                default,
            },
            name,
            payload,
        );
    }
}

fn scoped_events(
    workspace_id: &str,
    views: Arc<RwLock<Views>>,
    events: Arc<dyn ViewEventSink>,
) -> Arc<dyn EventEmitter> {
    Arc::new(WorkspaceEventEmitter {
        workspace_id: workspace_id.to_string(),
        views,
        inner: events,
    })
}

/// The MCP servers reach every workspace through this rather than through one
/// context captured at startup: a tool call names a workspace and reads it
/// without the registry, the window or the active context moving.
#[async_trait::async_trait]
impl wilkes_agent::search::WorkspaceCatalog for WorkspaceManager {
    async fn workspaces(&self) -> Result<Vec<wilkes_agent::search::WorkspaceDescriptor>, String> {
        let state = self.state().await.map_err(|error| error.to_string())?;
        Ok(state
            .workspaces
            .into_iter()
            .map(|workspace| wilkes_agent::search::WorkspaceDescriptor {
                active: workspace.id == state.active_workspace_id,
                id: workspace.id,
                name: workspace.name,
                roots: workspace.roots,
                active_root: workspace.active_root,
                read_only: workspace.read_only,
            })
            .collect())
    }

    async fn search_for(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Arc<dyn wilkes_agent::search::SearchService>, String> {
        let ctx = match workspace_id {
            Some(id) => self
                .context_for(id)
                .await
                .map_err(|error| format!("Workspace {id} is not available: {error}"))?,
            None => self.active(),
        };
        Ok(ctx)
    }
}

impl WorkspaceManager {
    pub fn new(
        app_data_dir: PathBuf,
        settings_path: PathBuf,
        events: Arc<dyn ViewEventSink>,
    ) -> anyhow::Result<(
        Arc<Self>,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    )> {
        // Before anything reads or writes the directory: a second opener must
        // be turned away before it can touch the registry it would share.
        let installation = crate::installation::InstallationLock::acquire(&app_data_dir)?;
        let id = initialize_workspace_registry(&app_data_dir, &settings_path)?;
        // The primary view shows the default workspace: the one shown by the
        // window focused last when the installation was last open.
        let views = Arc::new(RwLock::new(Views {
            default: id.clone(),
            shown: std::collections::HashMap::from([(PRIMARY_VIEW.to_string(), id.clone())]),
        }));
        let (ctx, event_rx, loop_fut) = AppContext::new_scoped(
            workspace_root(&app_data_dir, &id),
            app_data_dir.clone(),
            settings_path.clone(),
            workspace_manifest_path(&app_data_dir, &id),
            WorkerPaths::resolve(&app_data_dir),
            scoped_events(&id, Arc::clone(&views), Arc::clone(&events)),
        );
        Ok((
            Arc::new(Self {
                app_data_dir,
                settings_path,
                events,
                views,
                contexts: PLMutex::new(std::collections::HashMap::from([(id, ctx)])),
                registry_lock: PLMutex::new(()),
                switch_lock: Mutex::new(()),
                projection_builds: PLMutex::new(std::collections::HashMap::new()),
                projection_build_serial: std::sync::atomic::AtomicU64::new(0),
                _installation: installation,
            }),
            event_rx,
            loop_fut,
        ))
    }

    /// The default workspace's context: what answers a request that names no
    /// workspace and comes from no window.
    pub fn active(&self) -> Arc<AppContext> {
        let views = self.views.read();
        Arc::clone(
            self.contexts
                .lock()
                .get(&views.default)
                .expect("the default workspace's context is open while it is the default"),
        )
    }

    /// The context of the workspace `view` shows. A view that shows none —
    /// the standalone reader, or a window not yet bound — is answered from
    /// the default workspace, as every such window was before there was more
    /// than one.
    pub fn view_context(&self, view: &str) -> Arc<AppContext> {
        let views = self.views.read();
        let id = views.shown.get(view).unwrap_or(&views.default);
        Arc::clone(
            self.contexts
                .lock()
                .get(id)
                .expect("a shown workspace's context is open while it is shown"),
        )
    }

    /// The workspace `view` shows, if it shows one.
    pub fn view_workspace_id(&self, view: &str) -> Option<String> {
        self.views.read().shown.get(view).cloned()
    }

    /// The view showing workspace `id`, if one does.
    pub fn view_showing(&self, id: &str) -> Option<String> {
        self.views.read().viewer_of(id).map(str::to_string)
    }

    /// The installation's listing: `active_workspace_id` is the default
    /// workspace. For a request that comes from no view.
    pub async fn state(&self) -> anyhow::Result<WorkspaceState> {
        // The registry snapshot is taken under the lock and the manifests are
        // read after it: `read_workspace_state` awaits, and this lock must not
        // be held across an await.
        let _snapshot = {
            let _guard = self.registry_lock.lock();
            load_registry(&self.app_data_dir)?
        };
        read_workspace_state(&self.app_data_dir, &self.settings_path).await
    }

    /// The listing as one view sees it: `active_workspace_id` is the
    /// workspace that view shows, and `open_elsewhere` the ones other views
    /// show.
    pub async fn state_for_view(&self, view: &str) -> anyhow::Result<WorkspaceState> {
        let mut state = self.state().await?;
        let views = self.views.read();
        state.active_workspace_id = views.shown.get(view).unwrap_or(&views.default).clone();
        state.open_elsewhere = views
            .shown
            .iter()
            .filter(|(label, _)| label.as_str() != view)
            .map(|(_, id)| id.clone())
            .collect();
        Ok(state)
    }

    /// The context serving one workspace, opening it if none is open. Moves
    /// nothing: no view, the default workspace and the registry are left
    /// exactly as they were.
    ///
    /// This is what lets a caller work across workspaces at all. Every
    /// document endpoint reads the index of the context it is given, and each
    /// workspace has its own — so before this, reaching a document meant
    /// switching, which rewrites the registry on disk, shuts the previous
    /// context down and moves the user's window. A read had to perform a
    /// write to happen.
    pub async fn context_for(&self, id: &str) -> anyhow::Result<Arc<AppContext>> {
        // The same lock every view change takes: while this decides whether
        // a context for `id` is open and opens one, none can be retired.
        let _guard = self.switch_lock.lock().await;
        self.ensure_registered(id)?;
        Ok(self.context_or_open(id))
    }

    fn ensure_registered(&self, id: &str) -> anyhow::Result<()> {
        let _registry_guard = self.registry_lock.lock();
        let registry = load_registry(&self.app_data_dir)?;
        anyhow::ensure!(
            registry.workspace_ids.iter().any(|item| item == id),
            "Unknown workspace"
        );
        Ok(())
    }

    /// The open context for `id`, or a new one. The caller holds
    /// `switch_lock`, so no other caller can open a second one meanwhile.
    fn context_or_open(&self, id: &str) -> Arc<AppContext> {
        if let Some(ctx) = self.contexts.lock().get(id) {
            return Arc::clone(ctx);
        }
        let (ctx, event_rx, loop_fut) = AppContext::new_scoped(
            workspace_root(&self.app_data_dir, id),
            self.app_data_dir.clone(),
            self.settings_path.clone(),
            workspace_manifest_path(&self.app_data_dir, id),
            WorkerPaths::resolve(&self.app_data_dir),
            // Routed by what the views show: a context no view shows reports
            // to no window, so opening one cannot make a window report
            // progress from a workspace the user is not looking at.
            scoped_events(id, Arc::clone(&self.views), Arc::clone(&self.events)),
        );
        ctx.clone().spawn_background_tasks(event_rx, loop_fut);
        self.contexts
            .lock()
            .insert(id.to_string(), Arc::clone(&ctx));
        ctx
    }

    /// Shuts down `id`'s context if no view shows it and it is not the
    /// default. The caller holds `switch_lock`.
    async fn retire_if_unheld(&self, id: &str) {
        let retired = {
            let views = self.views.read();
            if views.holds(id) {
                None
            } else {
                self.contexts.lock().remove(id)
            }
        };
        if let Some(ctx) = retired {
            ctx.shutdown().await;
        }
    }

    /// Writes the default workspace to the registry, before the views take
    /// it on: a failure leaves memory and disk agreeing. The caller holds
    /// `switch_lock`, so no other change lands between this and the views
    /// being updated.
    fn persist_default(&self, default: &str) -> anyhow::Result<()> {
        let _registry_guard = self.registry_lock.lock();
        let mut registry = load_registry(&self.app_data_dir)?;
        anyhow::ensure!(
            registry.workspace_ids.iter().any(|item| item == default),
            "Unknown workspace"
        );
        if registry.active_workspace_id == default {
            return Ok(());
        }
        registry.active_workspace_id = default.to_string();
        atomic_write_json(&registry_path(&self.app_data_dir), &registry)
    }

    /// Shows workspace `id` in a new view. The default does not move: the
    /// window that opens takes it when it is focused ([`Self::focus_view`]).
    pub async fn open_view(&self, view: &str, id: &str) -> anyhow::Result<()> {
        let _guard = self.switch_lock.lock().await;
        self.ensure_registered(id)?;
        {
            let views = self.views.read();
            anyhow::ensure!(
                !views.shown.contains_key(view),
                "A window named {view} already shows a workspace"
            );
            if let Some(other) = views.viewer_of(id) {
                return Err(WorkspaceShownElsewhere {
                    workspace_id: id.to_string(),
                    view: other.to_string(),
                }
                .into());
            }
        }
        self.context_or_open(id);
        self.views.write().bind(view, id);
        Ok(())
    }

    /// Makes the workspace `view` shows the default, as the user moving to
    /// that window does. A view that shows none leaves the default where it
    /// is.
    pub async fn focus_view(&self, view: &str) -> anyhow::Result<()> {
        let _guard = self.switch_lock.lock().await;
        let (previous, next) = {
            let views = self.views.read();
            let Some(next) = views.shown.get(view) else {
                return Ok(());
            };
            if *next == views.default {
                return Ok(());
            }
            (views.default.clone(), next.clone())
        };
        self.persist_default(&next)?;
        self.views.write().default = next;
        self.retire_if_unheld(&previous).await;
        Ok(())
    }

    /// Forgets a view that has closed, and shuts down the workspace it showed
    /// unless something else still holds it.
    pub async fn close_view(&self, view: &str) -> anyhow::Result<()> {
        let _guard = self.switch_lock.lock().await;
        let shown = self.views.write().unbind(view);
        if let Some(shown) = shown {
            self.retire_if_unheld(&shown).await;
        }
        Ok(())
    }

    /// Set where this installation embeds, and the host's token.
    ///
    /// `token` replaces the stored one when given — an empty one removes it —
    /// and `None` leaves it as it is. Every open workspace drops its embedder
    /// and loads it again from the new place: the setting is the
    /// installation's, and a workspace still embedding at the old place would
    /// be the second answer to "where does this run".
    pub async fn configure_remote_embedding(
        &self,
        settings: wilkes_core::types::RemoteEmbeddingSettings,
        token: Option<String>,
    ) -> Result<(), String> {
        let active = self.active();
        let token_path = active.remote_embedding_token_path();
        match token.as_deref().map(str::trim) {
            Some("") => wilkes_core::token_file::remove(&token_path),
            Some(token) => wilkes_core::token_file::write(&token_path, token),
            None => Ok(()),
        }
        .map_err(|error| format!("Could not store the embedding host's token: {error:#}"))?;

        let before = active.get_settings().await.remote_embedding;
        // A changed setting reloads the default workspace's embedder as part
        // of saving it; a token alone changes no setting, so that workspace
        // is reloaded here instead.
        active
            .update_settings(serde_json::json!({ "remote_embedding": settings }))
            .await
            .map_err(|error| format!("{error:#}"))?;
        if before == settings {
            active.embedding_location_changed();
        }
        let others: Vec<Arc<AppContext>> = self
            .contexts
            .lock()
            .values()
            .filter(|ctx| !Arc::ptr_eq(ctx, &active))
            .cloned()
            .collect();
        for ctx in others {
            ctx.embedding_location_changed();
        }
        Ok(())
    }

    /// Shuts down every context this manager owns — those the views show and
    /// any opened for other workspaces.
    ///
    /// The default's context is shut down but stays in the map: a request
    /// still arriving during exit is answered by a context that refuses it,
    /// not by a manager with no default to give it.
    pub async fn shutdown_all(&self) {
        let (others, default) = {
            let views = self.views.read();
            let mut contexts = self.contexts.lock();
            let default = contexts.get(&views.default).cloned();
            let others: Vec<Arc<AppContext>> = contexts
                .drain()
                .filter(|(id, _)| *id != views.default)
                .map(|(_, ctx)| ctx)
                .collect();
            if let Some(default) = &default {
                contexts.insert(views.default.clone(), Arc::clone(default));
            }
            (others, default)
        };
        for ctx in others {
            ctx.shutdown().await;
        }
        if let Some(default) = default {
            default.shutdown().await;
        }
    }
    pub async fn create(&self, name: String) -> anyhow::Result<WorkspaceSummary> {
        let (manifest, manifest_path) = {
            let _guard = self.registry_lock.lock();
            let name = name.trim();
            anyhow::ensure!(!name.is_empty(), "Workspace name cannot be empty");
            let mut registry = load_registry(&self.app_data_dir)?;
            let id = uuid::Uuid::new_v4().to_string();
            let manifest = WorkspaceManifest::new(id.clone(), name.to_string());
            let manifest_path = workspace_manifest_path(&self.app_data_dir, &id);
            register_workspace(&self.app_data_dir, &mut registry, &manifest)?;
            (manifest, manifest_path)
        };
        // A fresh workspace declares no semantic block, so what it embeds with
        // is whatever global settings say — read rather than assumed, because
        // the caller is told the embedder and would otherwise be told a guess.
        let settings = get_scoped_settings(&self.settings_path, &manifest_path).await?;
        Ok(manifest.summary(settings.semantic.selected))
    }

    pub async fn rename(&self, id: &str, name: String) -> anyhow::Result<WorkspaceSummary> {
        let (manifest, path) = {
            let _guard = self.registry_lock.lock();
            let name = name.trim();
            anyhow::ensure!(!name.is_empty(), "Workspace name cannot be empty");
            let registry = load_registry(&self.app_data_dir)?;
            anyhow::ensure!(
                registry.workspace_ids.iter().any(|item| item == id),
                "Unknown workspace"
            );
            let path = workspace_manifest_path(&self.app_data_dir, id);
            let manifest = update_manifest(&path, |manifest| {
                consumer_ensure!(
                    !manifest.is_application_managed(),
                    ConsumerErrorCode::ManagedWorkspaceProtected,
                    "managed workspaces cannot be renamed",
                );
                manifest.name = name.to_string();
                Ok(())
            })?;
            (manifest, path)
        };
        let settings = get_scoped_settings(&self.settings_path, &path).await?;
        Ok(manifest.summary(settings.semantic.selected))
    }

    /// Shows workspace `id` in `view`, and makes it the default: the user
    /// switching a window is the user working in it.
    ///
    /// Refused with [`WorkspaceShownElsewhere`] when another view shows `id`.
    pub async fn switch_view(&self, view: &str, id: &str) -> anyhow::Result<WorkspaceState> {
        {
            let _guard = self.switch_lock.lock().await;
            self.ensure_registered(id)?;
            let change = {
                let views = self.views.read();
                if let Some(other) = views.viewer_of(id) {
                    if other != view {
                        return Err(WorkspaceShownElsewhere {
                            workspace_id: id.to_string(),
                            view: other.to_string(),
                        }
                        .into());
                    }
                }
                let previous_shown = views.shown.get(view).cloned();
                if previous_shown.as_deref() == Some(id) && views.default == id {
                    None
                } else {
                    Some((previous_shown, views.default.clone()))
                }
            };
            let Some((previous_shown, previous_default)) = change else {
                drop(_guard);
                return self.state_for_view(view).await;
            };
            self.context_or_open(id);
            self.persist_default(id)?;
            {
                let mut views = self.views.write();
                views.default = id.to_string();
                views.bind(view, id);
            }
            if let Some(previous_shown) = previous_shown {
                self.retire_if_unheld(&previous_shown).await;
            }
            self.retire_if_unheld(&previous_default).await;
        }
        let state = self.state().await?;
        self.events.emit(
            "workspace-changed",
            serde_json::to_value(&state).unwrap_or_default(),
        );
        self.state_for_view(view).await
    }

    /// Removes a workspace from the registry and deletes everything it owns
    /// on disk.
    ///
    /// Managed or not. A protected corpus refuses every *write* — the import
    /// API is its only writer — but refusing to delete it too left the user
    /// with gigabytes of an application's index and no way to reclaim them
    /// except by finding the directory themselves. Protecting the content of
    /// a corpus is not the same as protecting its existence, and only the
    /// second is the user's to decide.
    ///
    /// Deleting a canonical corpus deletes the workspaces that project it
    /// into other embedding spaces: those exist only to hold vectors for its
    /// membership, they are never listed on their own, and leaving them
    /// behind would leave a projection of a corpus that no longer exists.
    ///
    /// The active workspace is refused rather than switched away from.
    /// Activating one moves the user's window, shuts a context down and
    /// rewrites the registry; a caller that wants that has [`Self::switch`],
    /// and one that does not would be surprised to find the window somewhere
    /// else because it deleted a workspace it was not looking at.
    pub async fn delete(&self, id: &str) -> anyhow::Result<WorkspaceState> {
        // The same lock every view change and `context_for` take: no view may
        // move to a workspace, and no context may be opened for one, between
        // the decision below and the removal that follows it.
        let _switch_guard = self.switch_lock.lock().await;
        let removals = {
            let _registry_guard = self.registry_lock.lock();
            let registry = load_registry(&self.app_data_dir)?;
            anyhow::ensure!(
                registry.workspace_ids.iter().any(|item| item == id),
                "Unknown workspace"
            );
            let mut removals = vec![id.to_string()];
            for candidate in &registry.workspace_ids {
                let manifest =
                    read_manifest(&workspace_manifest_path(&self.app_data_dir, candidate))?;
                if matches!(
                    &manifest.kind,
                    WorkspaceKind::ApplicationManaged {
                        parent_corpus_id: Some(parent),
                        ..
                    } if parent == id
                ) && !removals.contains(candidate)
                {
                    removals.push(candidate.clone());
                }
            }
            {
                let views = self.views.read();
                anyhow::ensure!(
                    !removals.iter().any(|removal| views.holds(removal)),
                    "A workspace open in a window cannot be deleted. Switch that window to another workspace, or close it, first."
                );
            }
            removals
        };

        // Contexts first, registry second, files last. A context holds this
        // workspace's index and metadata databases open, and the registry is
        // what makes the workspace real: an interruption after the registry
        // is written leaves files nothing names, which is recoverable, while
        // the opposite order would leave the registry naming a workspace
        // whose manifest is gone — and every listing reads every manifest.
        for removal in &removals {
            let context = { self.contexts.lock().remove(removal) };
            if let Some(context) = context {
                context.shutdown().await;
            }
        }
        {
            let _registry_guard = self.registry_lock.lock();
            let mut registry = load_registry(&self.app_data_dir)?;
            registry
                .workspace_ids
                .retain(|candidate| !removals.contains(candidate));
            atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
        }
        for removal in &removals {
            let root = workspace_root(&self.app_data_dir, removal);
            if let Err(error) = std::fs::remove_dir_all(&root) {
                // Not a failure of the deletion: the workspace is gone from
                // the registry and nothing will open it again. What is left
                // is bytes, and the path to them is named here rather than
                // swallowed so the user can be told where to look.
                tracing::error!(
                    workspace = %removal,
                    path = %root.display(),
                    %error,
                    "workspace was deregistered but its directory could not be removed"
                );
            }
        }
        drop(_switch_guard);

        let state = self.state().await?;
        self.events.emit(
            "workspace-changed",
            serde_json::to_value(&state).unwrap_or_default(),
        );
        Ok(state)
    }

    /// Create or retrieve one application's protected corpus workspace.
    /// Configuration is immutable after the first successful ensure.
    pub async fn ensure_managed_workspace(
        &self,
        request: EnsureManagedWorkspace,
    ) -> anyhow::Result<ManagedWorkspaceStatus> {
        let corpus_key = request.corpus_key.trim();
        let owner = request.owner.trim();
        anyhow::ensure!(!owner.is_empty(), "owner must not be empty");
        anyhow::ensure!(!corpus_key.is_empty(), "corpus_key must not be empty");
        anyhow::ensure!(request.chunk_size > 0, "chunk_size must be positive");
        anyhow::ensure!(
            request.chunk_overlap < request.chunk_size,
            "chunk_overlap must be smaller than chunk_size"
        );

        let mut semantic = get_scoped_settings(&self.settings_path, &self.settings_path)
            .await?
            .semantic;
        semantic.enabled = true;
        semantic.selected = request.embedding.clone();
        semantic.chunk_size = request.chunk_size;
        semantic.chunk_overlap = request.chunk_overlap;

        let id = {
            let _guard = self.registry_lock.lock();
            let mut registry = load_registry(&self.app_data_dir)?;
            let mut existing = None;
            for id in &registry.workspace_ids {
                let manifest = read_manifest(&workspace_manifest_path(&self.app_data_dir, id))?;
                if matches!(
                    &manifest.kind,
                    WorkspaceKind::ApplicationManaged {
                        owner: existing_owner,
                        purpose,
                        corpus_key: key,
                        parent_corpus_id: None,
                    }
                        if existing_owner == owner
                            && purpose == "semantic-corpus"
                            && key == corpus_key
                ) {
                    existing = Some(manifest);
                    break;
                }
            }
            if let Some(manifest) = existing {
                let configured = manifest.semantic.as_ref().ok_or_else(|| {
                    consumer_error(
                        ConsumerErrorCode::ManagedWorkspaceConfigurationMismatch,
                        "semantic configuration is absent",
                    )
                })?;
                // Extraction only. The canonical corpus owns chunking, which
                // is fixed when it is created because every `chunk_ref` in
                // every consumer's graph derives from it — that is the one
                // thing here nobody can recompute, so it is the one thing
                // still refused.
                //
                // The embedder used to be compared too, and never updated, so
                // this endpoint refused for the life of the corpus any request
                // naming a different model. A consumer that sent its live
                // setting — which is the only setting it has — could not
                // ingest again, and the remedy it was pointed at could not
                // help, because the canonical corpus's embedder is not what
                // serves it. It no longer embeds at all; its projections do,
                // and `PUT /api/corpora/spaces` is where a model is chosen.
                consumer_ensure!(
                    configured.chunk_size == request.chunk_size
                        && configured.chunk_overlap == request.chunk_overlap,
                    ConsumerErrorCode::ManagedWorkspaceConfigurationMismatch,
                    "existing corpus was chunked at {}/{} and cannot be re-chunked; \
                     requested {}/{}",
                    configured.chunk_size,
                    configured.chunk_overlap,
                    request.chunk_size,
                    request.chunk_overlap,
                );
                manifest.id
            } else {
                let manifest = WorkspaceManifest::application_managed(
                    &self.app_data_dir,
                    format!("{owner} semantic corpus"),
                    owner,
                    corpus_key,
                    None,
                    semantic,
                )?;
                register_workspace(&self.app_data_dir, &mut registry, &manifest)?;
                manifest.id
            }
        };
        self.managed_workspace_status(&id).await
    }

    /// The registry half of [`Self::ensure_managed_space`]: the projection's
    /// manifest, keyed by its parent corpus and its embedder, and idempotent
    /// on both. Separated because it is the part that decides what a
    /// projection *is* — a hidden child of one canonical corpus, sharing that
    /// corpus's key and extraction settings — while everything after it is the
    /// vector work that gives the projection its contents.
    fn ensure_projection_workspace(
        &self,
        corpus_id: &str,
        owner: &str,
        corpus_key: &str,
        parent_semantic: &SemanticSettings,
        embedding: &SelectedEmbedder,
    ) -> anyhow::Result<String> {
        let _guard = self.registry_lock.lock();
        let mut registry = load_registry(&self.app_data_dir)?;
        for id in &registry.workspace_ids {
            let candidate = read_manifest(&workspace_manifest_path(&self.app_data_dir, id))?;
            if matches!(
                candidate.kind,
                WorkspaceKind::ApplicationManaged {
                    parent_corpus_id: Some(ref parent),
                    ..
                } if parent == corpus_id
            ) && candidate
                .semantic
                .as_ref()
                .is_some_and(|semantic| &semantic.selected == embedding)
            {
                return Ok(id.clone());
            }
        }
        let mut semantic = parent_semantic.clone();
        semantic.selected = embedding.clone();
        let manifest = WorkspaceManifest::application_managed(
            &self.app_data_dir,
            format!("{owner} embedding projection"),
            owner,
            corpus_key,
            Some(corpus_id),
            semantic,
        )?;
        register_workspace(&self.app_data_dir, &mut registry, &manifest)?;
        Ok(manifest.id)
    }

    /// Adds one derived embedding projection to an existing managed corpus.
    /// The child workspace is an implementation detail: callers continue to
    /// address the canonical `corpus_id` plus the returned opaque space id.
    pub async fn ensure_managed_space(
        self: &Arc<Self>,
        request: EnsureManagedEmbeddingSpace,
    ) -> anyhow::Result<ManagedEmbeddingSpaceStatus> {
        let parent_path = workspace_manifest_path(&self.app_data_dir, &request.corpus_id);
        let parent = read_manifest(&parent_path)?;
        let (owner, corpus_key, parent_semantic) = match (&parent.kind, &parent.semantic) {
            (
                WorkspaceKind::ApplicationManaged {
                    owner,
                    purpose,
                    corpus_key,
                    parent_corpus_id: None,
                },
                Some(semantic),
            ) if purpose == "semantic-corpus" => {
                (owner.clone(), corpus_key.clone(), semantic.clone())
            }
            _ => consumer_bail!(
                ConsumerErrorCode::ManagedWorkspaceNotFound,
                "this id names no managed semantic corpus",
            ),
        };

        let id = self.ensure_projection_workspace(
            &request.corpus_id,
            &owner,
            &corpus_key,
            &parent_semantic,
            &request.embedding,
        )?;

        // Catching up offers every admitted source in the corpus to the
        // projection, and a projection already level with its corpus takes
        // nothing from any of them: the sweep is a hash and an index lookup
        // per document, answering a question the corpus generation has already
        // answered for all of them at once. `catch_up_corpus` has always made
        // this check per space; ensuring a space did not, so a caller that put
        // to this endpoint on a timer paid the sweep every time. The check is
        // the same one, and it must stay the same one.
        let status = self.managed_spaces(&request.corpus_id).await?;
        if let Some(level) = status
            .spaces
            .iter()
            .find(|space| space.workspace_id == id)
            .filter(|space| space.ready && space.indexed_generation == status.corpus_generation)
        {
            return Ok(level.clone());
        }

        self.build_projection(&request.corpus_id, &id, None).await?;

        let canonical = self.managed_spaces(&request.corpus_id).await?;
        canonical
            .spaces
            .into_iter()
            .find(|space| space.workspace_id == id)
            .ok_or_else(|| anyhow::anyhow!("new embedding projection did not become visible"))
    }

    /// Brings one projection level with its corpus, and is the only thing that
    /// ever does.
    ///
    /// Every canonical source is offered to the projection under a
    /// content-derived idempotency key, so a document the projection already
    /// holds costs a hash and a lookup, and one that it lacks — because the
    /// space was created after the document, or because a fan-out died
    /// half way — is embedded now. That makes catching up idempotent and
    /// therefore safe to drive from anywhere: a crash leaves no half-state to
    /// reconcile, only work not yet done.
    pub async fn catch_up_projection(
        &self,
        corpus_id: &str,
        projection_workspace_id: &str,
        source_workspace: Option<Arc<AppContext>>,
    ) -> anyhow::Result<()> {
        let child = self.context_for(projection_workspace_id).await?;
        let canonical_context = self.context_for(corpus_id).await?;
        let corpus_id = corpus_id.to_string();
        // A build of the projection's index, run as one: it loads the model
        // and embeds with it, so it is what the projection's cancel stops.
        let projection = Arc::clone(&child);
        child
            .run_as_embed_task(move |cancel_flag| async move {
                projection
                    .ensure_managed_runtime()
                    .await
                    .map_err(anyhow::Error::msg)?;
                canonical_context
                    .ensure_managed_index()
                    .await
                    .map_err(anyhow::Error::msg)?;

                // The projection computes vectors only: it neither copies
                // sources nor repeats extraction or chunking. Its passages are
                // the canonical admitted rendition's passages, which is what
                // keeps every space's chunk refs identical.
                let sources = canonical_context
                    .managed_admitted_sources()
                    .map_err(anyhow::Error::msg)?;
                for (source, rendition) in sources {
                    // Between documents, as a build checks: a cancel raises
                    // the flag before it kills, so the next document is never
                    // started into a killed worker.
                    anyhow::ensure!(
                        !cancel_flag.load(Ordering::Relaxed),
                        "catching up was cancelled"
                    );
                    // Keyed by the rendition, not by the source bytes. The key
                    // is bound to a rendition for the life of the index and
                    // nothing can rebind it, so a key that names less than
                    // what it is bound to becomes a tombstone the moment the
                    // unnamed part moves: the source digest survives a
                    // re-extraction, the rendition does not, and a corpus
                    // whose recipe had changed could never bring a projection
                    // level again.
                    projection
                        .import_managed_projection(
                            format!("space-backfill-{rendition}"),
                            &canonical_context,
                            source.clone(),
                            serde_json::json!({
                                "kind": "managed_corpus_projection",
                                "canonical_corpus_id": corpus_id,
                            }),
                            source_workspace.as_ref(),
                        )
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Ok(())
            })
            .await
    }

    /// Brings one projection level with its corpus in a task the manager
    /// owns, and waits for it.
    ///
    /// The build used to run inside the request that asked for it, so it
    /// lived exactly as long as that request. A client that gave up — a
    /// 300-second timeout while a download held the machine — dropped the
    /// build half way through its documents, and nothing resumed it: the
    /// projection sat part-embedded and unready until someone happened to put
    /// to the endpoint again. A caller that stops waiting now stops only its
    /// own waiting.
    ///
    /// One build per projection at a time. A caller that finds one in flight
    /// waits for it and then checks the projection again rather than taking
    /// its result as its own: the build in flight listed the corpus's sources
    /// when it began, which may be before the document this caller imported.
    pub async fn build_projection(
        self: &Arc<Self>,
        corpus_id: &str,
        projection_id: &str,
        source_workspace: Option<Arc<AppContext>>,
    ) -> anyhow::Result<()> {
        let manager = Arc::clone(self);
        let corpus = corpus_id.to_string();
        let projection = projection_id.to_string();
        self.run_projection_build(corpus_id, projection_id, move || {
            let manager = Arc::clone(&manager);
            let corpus = corpus.clone();
            let projection = projection.clone();
            let source_workspace = source_workspace.clone();
            async move {
                manager
                    .catch_up_projection(&corpus, &projection, source_workspace)
                    .await
                    .map_err(|error| format!("{error:#}"))
            }
        })
        .await
    }

    /// The task and the bookkeeping of [`Self::build_projection`], apart from
    /// what a build does, so the lifetime rules can be tested without a model.
    async fn run_projection_build<Work, Build>(
        self: &Arc<Self>,
        corpus_id: &str,
        projection_id: &str,
        mut work: Work,
    ) -> anyhow::Result<()>
    where
        Work: FnMut() -> Build,
        Build: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        loop {
            let (build, ours) = {
                let mut builds = self.projection_builds.lock();
                match builds.get(projection_id) {
                    Some(build) => (build.clone(), false),
                    None => {
                        let serial = self
                            .projection_build_serial
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let (report, outcome) = tokio::sync::watch::channel(None);
                        let build = ProjectionBuild {
                            serial,
                            corpus_id: corpus_id.to_string(),
                            outcome,
                        };
                        builds.insert(projection_id.to_string(), build.clone());
                        let slot = ProjectionBuildSlot {
                            manager: Arc::clone(self),
                            projection_id: projection_id.to_string(),
                            serial,
                        };
                        let corpus_id = corpus_id.to_string();
                        let projection_id = projection_id.to_string();
                        let job = work();
                        tokio::spawn(async move {
                            let result = job.await;
                            match &result {
                                Ok(()) => tracing::info!(
                                    "embedding projection {projection_id} of corpus {corpus_id} \
                                     is level with its corpus"
                                ),
                                Err(error) => tracing::error!(
                                    "embedding projection {projection_id} of corpus {corpus_id} \
                                     did not finish building: {error}"
                                ),
                            }
                            // Out of the map before the outcome is published,
                            // so a waiter that loops finds no finished build
                            // to wait on again.
                            drop(slot);
                            let _ = report.send(Some(result));
                        });
                        (build, true)
                    }
                }
            };
            let mut outcome = build.outcome;
            let result = match outcome.wait_for(Option::is_some).await {
                Ok(finished) => finished.clone().expect("waited for an outcome"),
                Err(_) => Err("the projection build ended without an outcome".to_string()),
            };
            if ours {
                return result.map_err(anyhow::Error::msg);
            }
            let status = self.managed_spaces(corpus_id).await?;
            if status.spaces.iter().any(|space| {
                space.workspace_id == projection_id
                    && space.ready
                    && space.indexed_generation == status.corpus_generation
            }) {
                return Ok(());
            }
        }
    }

    /// Projection builds of one corpus in flight, which the corpus status
    /// reports among its pending builds: a build waiting for a worker shows
    /// nothing else, since a projection has no index until its first document
    /// is embedded.
    pub fn projection_builds_in_flight(&self, corpus_id: &str) -> u64 {
        self.projection_builds
            .lock()
            .values()
            .filter(|build| build.corpus_id == corpus_id)
            .count() as u64
    }

    /// Brings every projection of one corpus level with it, reporting per
    /// space rather than stopping at the first failure.
    ///
    /// One model being unavailable is not a reason to leave the others behind:
    /// the spaces are independent derivations of the same membership, and a
    /// space that cannot catch up simply goes on failing closed until it can.
    /// `source_workspace` is the library the document being imported came
    /// from, when there is one. It is offered to each projection as a place to
    /// adopt vectors from rather than recompute them — the reason it is
    /// threaded this far is that the import handler resolves it and used to
    /// drop it one line later, so a corpus re-embedded documents its own
    /// Wilkes had already embedded under the same model.
    pub async fn catch_up_corpus(
        self: &Arc<Self>,
        corpus_id: &str,
        source_workspace: Option<Arc<AppContext>>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let status = self.managed_spaces(corpus_id).await?;
        let mut failures = Vec::new();
        for space in status.spaces.iter().filter(|space| !space.primary) {
            if space.ready && space.indexed_generation == status.corpus_generation {
                continue;
            }
            if let Err(error) = self
                .build_projection(corpus_id, &space.workspace_id, source_workspace.clone())
                .await
            {
                failures.push((space.embedding_space_id.clone(), format!("{error:#}")));
            }
        }
        Ok(failures)
    }

    /// Resolves a logical corpus/space pair to the internal projection
    /// workspace that owns those vectors, refusing a projection whose
    /// rendition/chunk membership is behind the canonical corpus.
    ///
    /// Refusing names catching up as the remedy, because that is a thing the
    /// caller can cause: `catch_up_corpus` is idempotent and the ensure-space
    /// endpoint performs it. Serving the stale space instead would be the one
    /// unacceptable outcome — coordinates for a corpus this space does not
    /// hold.
    pub async fn managed_space_context(
        &self,
        corpus_id: &str,
        embedding_space_id: &str,
    ) -> anyhow::Result<Arc<AppContext>> {
        let status = self.managed_spaces(corpus_id).await?;
        let space = status
            .spaces
            .iter()
            .find(|space| space.embedding_space_id == embedding_space_id)
            .ok_or_else(|| {
                consumer_error(
                    ConsumerErrorCode::EmbeddingSpaceMismatch,
                    format!(
                        "corpus={}, request={embedding_space_id}; {}",
                        describe_corpus_spaces(&status.spaces),
                        corpus_spaces_remedy(&status.spaces),
                    ),
                )
            })?;
        consumer_ensure!(
            space.ready && space.indexed_generation == status.corpus_generation,
            ConsumerErrorCode::EmbeddingSpaceStale,
            "projection generation {}, corpus generation {}; \
             the projection has not been caught up to the corpus",
            space.indexed_generation,
            status.corpus_generation,
        );
        self.context_for(&space.workspace_id).await
    }

    /// The one way a consumer route opens an index.
    ///
    /// Every chunk, embed and export route addresses an index with the same
    /// object, and this is where that object becomes a database. Before this
    /// there were two resolvers — a workspace one that opened whatever was
    /// asked for, and a managed one that also routed a corpus to the
    /// projection holding a space — and a route inherited its addressing from
    /// whichever half it had been written against.
    ///
    /// The pin is optional on a user workspace and required on a managed
    /// corpus because on a managed corpus it *routes* as well as verifies.
    /// That is one rule with a stated reason rather than two mechanisms: in
    /// both cases a supplied pin is honoured exactly, and in neither case is a
    /// mismatch ever served.
    pub async fn consumer_index(
        &self,
        scope: &ConsumerScope,
    ) -> Result<ConsumerIndex, ConsumerError> {
        let id = match &scope.workspace_id {
            Some(id) => id.clone(),
            // Not a filter, and not "whichever workspace happens to be open"
            // by accident: chunk refs are per-index, so an unnamed workspace
            // has to resolve to exactly one index and say which.
            None => self
                .active_workspace_id()
                .map_err(|error| ConsumerError::from_anyhow(&error))?,
        };
        let manifest =
            read_manifest(&workspace_manifest_path(&self.app_data_dir, &id)).map_err(|_| {
                ConsumerError::new(
                    ConsumerErrorCode::ManagedWorkspaceNotFound,
                    format!("no workspace with id {id}"),
                )
            })?;
        let pin = scope.expected_embedding_space_id.as_deref();

        let parent_corpus_id = match &manifest.kind {
            WorkspaceKind::User => {
                let context = self
                    .context_for(&id)
                    .await
                    .map_err(|error| ConsumerError::from_anyhow(&error))?;
                let space = context.index_space()?;
                return ConsumerIndex::verified(context, space, pin);
            }
            WorkspaceKind::ApplicationManaged {
                parent_corpus_id, ..
            } => parent_corpus_id.clone(),
        };

        // An internal projection is an implementation detail of the corpus
        // that owns it, reachable only as one of that corpus's spaces.
        if parent_corpus_id.is_some() {
            return Err(ConsumerError::new(
                ConsumerErrorCode::ManagedWorkspaceNotFound,
                format!("{id} is an internal projection of another corpus, not a corpus"),
            ));
        }

        let Some(pin) = pin else {
            let status = self
                .managed_workspace_status(&id)
                .await
                .map_err(|error| ConsumerError::from_anyhow(&error))?;
            // A corpus with no index has no coordinate system to disagree
            // about yet, which is the one case where an unpinned managed
            // request is answerable.
            if let Some(existing) = status.embedding_space_id {
                return Err(ConsumerError::new(
                    ConsumerErrorCode::EmbeddingSpaceMismatch,
                    format!("corpus={existing}, request=none"),
                ));
            }
            let context = self
                .context_for(&id)
                .await
                .map_err(|error| ConsumerError::from_anyhow(&error))?;
            return Ok(ConsumerIndex {
                context,
                space: IndexSpace::Absent,
            });
        };

        let context = self
            .managed_space_context(&id, pin)
            .await
            .map_err(|error| ConsumerError::from_anyhow(&error))?;
        let actual = context.ensure_managed_runtime().await?;
        if actual != pin {
            return Err(ConsumerError::new(
                ConsumerErrorCode::EmbeddingSpaceMismatch,
                format!("runtime={actual}, request={pin}"),
            ));
        }
        Ok(ConsumerIndex {
            context,
            space: IndexSpace::Exact(actual),
        })
    }

    /// The workspace a request that names none is answered from.
    pub fn active_workspace_id(&self) -> anyhow::Result<String> {
        let _guard = self.registry_lock.lock();
        Ok(load_registry(&self.app_data_dir)?.active_workspace_id)
    }

    pub async fn backup_managed_corpus(
        self: &Arc<Self>,
        corpus_id: &str,
        embedding_space_id: &str,
    ) -> anyhow::Result<crate::context::ManagedCorpusBackup> {
        let projection = self
            .managed_space_context(corpus_id, embedding_space_id)
            .await?;
        let canonical = self.context_for(corpus_id).await?;
        if Arc::ptr_eq(&canonical, &projection) {
            return canonical
                .backup_managed_corpus(corpus_id.to_string(), embedding_space_id.to_string())
                .await
                .map_err(anyhow::Error::msg);
        }
        canonical
            .backup_managed_corpus_projection(
                &projection,
                corpus_id.to_string(),
                embedding_space_id.to_string(),
            )
            .await
            .map_err(anyhow::Error::msg)
    }

    /// What a corpus can route to, and nothing else: its membership generation
    /// and its projections.
    ///
    /// Every routed request — each search, each chunk lookup — used to answer
    /// this with [`Self::managed_workspace_status`], which also walks every
    /// retained source directory to report byte totals nobody routing reads.
    /// On a 6,032-document corpus that walk was most of a second per request.
    /// The spaces here are the same ones the status reports, built by the same
    /// [`Self::projection_spaces`], so the two cannot disagree about which
    /// space is ready.
    pub async fn managed_spaces(&self, corpus_id: &str) -> anyhow::Result<ManagedSpaces> {
        let manifest = read_manifest(&workspace_manifest_path(&self.app_data_dir, corpus_id))?;
        match &manifest.kind {
            WorkspaceKind::ApplicationManaged {
                purpose,
                parent_corpus_id: None,
                ..
            } if purpose == "semantic-corpus" => {}
            _ => consumer_bail!(
                ConsumerErrorCode::ManagedWorkspaceNotFound,
                "this id names no managed semantic corpus",
            ),
        }
        let index_root = workspace_root(&self.app_data_dir, corpus_id);
        let corpus_generation = if index_root.join("semantic_index.db").exists() {
            SemanticIndex::open_for_maintenance(&index_root)?.managed_snapshot_sha256()?
        } else {
            wilkes_core::embed::identity::sha256_bytes(&[])
        };
        let spaces = self.projection_spaces(corpus_id, &corpus_generation)?;
        Ok(ManagedSpaces {
            corpus_generation,
            spaces,
        })
    }

    /// The projections of one canonical corpus that hold an index, ordered by
    /// space id. A projection is ready when every chunk it holds is embedded
    /// and its membership is the corpus's.
    fn projection_spaces(
        &self,
        corpus_id: &str,
        corpus_generation: &str,
    ) -> anyhow::Result<Vec<ManagedEmbeddingSpaceStatus>> {
        let registry = {
            let _guard = self.registry_lock.lock();
            load_registry(&self.app_data_dir)?
        };
        let mut spaces = Vec::new();
        for workspace_id in registry.workspace_ids {
            if workspace_id == corpus_id {
                continue;
            }
            let child = read_manifest(&workspace_manifest_path(&self.app_data_dir, &workspace_id))?;
            let belongs = matches!(
                child.kind,
                WorkspaceKind::ApplicationManaged {
                    parent_corpus_id: Some(ref parent),
                    ..
                } if parent == corpus_id
            );
            if !belongs {
                continue;
            }
            let child_root = workspace_root(&self.app_data_dir, &workspace_id);
            let Ok(index) = SemanticIndex::open_for_maintenance(&child_root) else {
                continue;
            };
            let child_identity = index.embedding_space_identity()?;
            let indexed_generation = index.managed_snapshot_sha256()?;
            let counts = index.managed_completeness()?;
            spaces.push(ManagedEmbeddingSpaceStatus {
                embedding_space_id: child_identity.id().0,
                embedding_space_identity: child_identity,
                ready: counts.1 == counts.2 && indexed_generation == corpus_generation,
                indexed_generation,
                workspace_id,
                primary: false,
            });
        }
        spaces.sort_by(|left, right| left.embedding_space_id.cmp(&right.embedding_space_id));
        Ok(spaces)
    }

    pub async fn managed_workspace_status(
        &self,
        corpus_id: &str,
    ) -> anyhow::Result<ManagedWorkspaceStatus> {
        let manifest = read_manifest(&workspace_manifest_path(&self.app_data_dir, corpus_id))?;
        let parent_corpus_id = match &manifest.kind {
            WorkspaceKind::ApplicationManaged {
                purpose,
                parent_corpus_id,
                ..
            } if purpose == "semantic-corpus" => parent_corpus_id.clone(),
            _ => consumer_bail!(
                ConsumerErrorCode::ManagedWorkspaceNotFound,
                "this id names no managed semantic corpus",
            ),
        };
        let semantic = manifest.semantic.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::ManagedWorkspaceConfigurationMismatch,
                "semantic configuration is absent",
            )
        })?;
        let index_root = workspace_root(&self.app_data_dir, corpus_id);
        let opened =
            match wilkes_core::embed::index::SemanticIndex::open_for_maintenance(&index_root)
                .and_then(|index| {
                    Ok((
                        index.embedding_space_identity()?,
                        index.managed_completeness()?,
                        index.managed_embedding_work_totals()?,
                        index.managed_snapshot_sha256()?,
                    ))
                }) {
                Ok(opened) => Some(opened),
                Err(error) => {
                    // Absent before the first build, so this is not on its own an
                    // error. Logged because the same arm covers a corrupt or
                    // unreadable index, which the caller only sees as a corpus
                    // that reports no embedding space.
                    tracing::info!(
                        "managed_workspace_status: no readable index at {}: {error:#}",
                        index_root.display()
                    );
                    None
                }
            };
        let stored_identity = opened.as_ref().map(|(identity, _, _, _)| identity.clone());
        let completeness = opened
            .as_ref()
            .map(|(_, counts, _, _)| *counts)
            .unwrap_or((0, 0, 0));
        let embedding_work = opened
            .as_ref()
            .map(|(_, _, totals, _)| *totals)
            .unwrap_or((0, 0));
        let corpus_generation = opened
            .as_ref()
            .map(|(_, _, _, generation)| generation.clone())
            .unwrap_or_else(|| wilkes_core::embed::identity::sha256_bytes(&[]));
        // The canonical corpus is not a coordinate system.
        //
        // It owns retained sources, extraction, chunking and stable passage
        // identity; its projections own vectors, one model each. So its
        // readiness is about membership — is the index open, are the admitted
        // documents chunked — and never about coordinates, which it no longer
        // computes. Measuring it by `required == embedded` would hold it
        // permanently unready, since embedded is now zero by design.
        //
        // And it reports no space. It used to publish one derived from
        // `semantic.selected`, a field fixed when the corpus was created and
        // reachable from no setting afterwards, which is how a store came to
        // pin a space it could not change and a model it never chose.
        let is_canonical = parent_corpus_id.is_none();
        let ready = if is_canonical {
            stored_identity.is_some()
        } else {
            stored_identity.as_ref().is_some_and(|identity| {
                identity.engine == semantic.selected.engine
                    && identity.model_id == semantic.selected.model.model_id()
                    && identity.dimension == semantic.selected.dimension
            }) && completeness.1 == completeness.2
        };
        // A projection with no index has no coordinate system yet. Reporting
        // one derived from the manifest would advertise an id that no index
        // will ever carry, so callers get nothing to echo back until vectors
        // exist — and the canonical has none to advertise at all.
        let identity = if is_canonical { None } else { stored_identity };
        fn directory_bytes(path: &Path) -> u64 {
            let mut bytes = 0;
            let mut pending = vec![path.to_path_buf()];
            while let Some(directory) = pending.pop() {
                let Ok(entries) = std::fs::read_dir(directory) else {
                    continue;
                };
                for entry in entries.filter_map(Result::ok) {
                    match entry.metadata() {
                        Ok(metadata) if metadata.is_dir() => pending.push(entry.path()),
                        Ok(metadata) => bytes += metadata.len(),
                        Err(_) => {}
                    }
                }
            }
            bytes
        }
        let managed_sources = index_root.join("managed_sources");
        let all_managed_source_bytes = directory_bytes(&managed_sources);
        let temporary_bytes = directory_bytes(&managed_sources.join(".imports"));
        let managed_source_bytes = all_managed_source_bytes.saturating_sub(temporary_bytes);
        let index_bytes = [
            "semantic_index.db",
            "semantic_index.db-wal",
            "semantic_index.db-shm",
        ]
        .iter()
        .map(|name| {
            std::fs::metadata(index_root.join(name))
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        })
        .sum();
        let primary_space = identity
            .as_ref()
            .map(|identity| ManagedEmbeddingSpaceStatus {
                embedding_space_id: identity.id().0,
                embedding_space_identity: identity.clone(),
                ready,
                indexed_generation: corpus_generation.clone(),
                workspace_id: corpus_id.to_string(),
                primary: parent_corpus_id.is_none(),
            });
        let mut status = ManagedWorkspaceStatus {
            corpus_id: corpus_id.to_string(),
            embedding_space_id: identity.as_ref().map(|identity| identity.id().0),
            embedding_space_identity: identity,
            // The recipe this runtime would produce under, analyzer included:
            // a corpus reported without it would claim compatibility with
            // readings it cannot reproduce.
            extraction_recipe_id: ExtractionRecipe::for_runtime(
                &wilkes_core::extract::production_registry(),
                semantic.chunk_size,
                semantic.chunk_overlap,
            )
            .id(),
            ready,
            indexed_documents: completeness.0,
            // The canonical corpus indexes chunks and embeds none of them, so
            // these two part company there rather than being the same number
            // under two names.
            indexed_chunks: if is_canonical {
                completeness.1
            } else {
                completeness.2
            },
            required_chunks: completeness.1,
            embedded_chunks: completeness.2,
            reused_chunks: embedding_work.0,
            computed_chunks: embedding_work.1,
            managed_source_bytes,
            temporary_bytes,
            index_bytes,
            total_managed_bytes: managed_source_bytes + temporary_bytes + index_bytes,
            integrity_checked_at_ms: chrono::Utc::now().timestamp_millis(),
            pending_imports: 0,
            pending_builds: 0,
            corpus_generation,
            spaces: primary_space.into_iter().collect(),
        };

        if parent_corpus_id.is_none() {
            status
                .spaces
                .extend(self.projection_spaces(corpus_id, &status.corpus_generation)?);
            // Every projection build of this corpus in flight, for its whole
            // life: a build waiting for a worker has no index yet, and so no
            // space to show it by.
            status.pending_builds = self.projection_builds_in_flight(corpus_id);
        }
        Ok(status)
    }

    /// Restores a self-verifying backup from Wilkes's own managed-backup
    /// directory. The caller names only one directory leaf, never an arbitrary
    /// path. A pre-created corpus for the same owner and store key may be
    /// replaced only while it is empty; an established corpus is never
    /// overwritten.
    pub async fn restore_managed_workspace(
        self: &Arc<Self>,
        backup_name: &str,
        expected_corpus_id: &str,
        expected_embedding_space_id: &str,
        expected_corpus_key: &str,
    ) -> anyhow::Result<ManagedWorkspaceStatus> {
        anyhow::ensure!(
            !backup_name.is_empty()
                && backup_name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '-' | '_' | '.')),
            "invalid managed backup name"
        );
        let backup_root = self.app_data_dir.join("managed_backups").join(backup_name);
        let backup: ManagedCorpusBackup =
            serde_json::from_slice(&std::fs::read(backup_root.join("backup-manifest.json"))?)?;
        anyhow::ensure!(
            backup.format == "wilkes-managed-corpus-backup/v1",
            "unsupported managed backup format"
        );
        anyhow::ensure!(
            backup.corpus_id == expected_corpus_id,
            "backup corpus mismatch"
        );
        anyhow::ensure!(
            backup.embedding_space_id == expected_embedding_space_id,
            "backup embedding-space mismatch"
        );
        verify_managed_backup_directory(&backup_root, &backup)?;
        let mut manifest = read_manifest(&backup_root.join("workspace.json"))?;
        anyhow::ensure!(
            manifest.id == expected_corpus_id,
            "workspace manifest corpus mismatch"
        );
        // The owner is read out of the backup rather than compared against a
        // compiled-in name: a backup carries the corpus it was taken from, and
        // what has to hold is that the corpus being replaced belongs to that
        // same application under that same store key.
        let owner = match &manifest.kind {
            WorkspaceKind::ApplicationManaged {
                owner,
                purpose,
                corpus_key,
                ..
            } if purpose == "semantic-corpus" && corpus_key == expected_corpus_key => owner.clone(),
            _ => anyhow::bail!("backup belongs to a different managed owner or corpus key"),
        };

        let _switch_guard = self.switch_lock.lock().await;
        let existing = {
            let _registry_guard = self.registry_lock.lock();
            let registry = load_registry(&self.app_data_dir)?;
            registry.workspace_ids.iter().find_map(|id| {
                let found = read_manifest(&workspace_manifest_path(&self.app_data_dir, id)).ok()?;
                matches!(
                    &found.kind,
                    WorkspaceKind::ApplicationManaged { owner: found_owner, purpose, corpus_key, .. }
                        if found_owner == &owner
                            && purpose == "semantic-corpus"
                            && corpus_key == expected_corpus_key
                )
                .then(|| id.clone())
            })
        };
        if let Some(existing_id) = existing.as_deref() {
            let root = workspace_root(&self.app_data_dir, existing_id);
            let documents = SemanticIndex::open_for_maintenance(&root)
                .ok()
                .and_then(|index| index.managed_completeness().ok())
                .map(|counts| counts.0)
                .unwrap_or(0);
            if documents > 0 {
                anyhow::ensure!(
                    existing_id == expected_corpus_id,
                    "RESTORE_TARGET_NOT_EMPTY"
                );
                drop(_switch_guard);
                let status = self.managed_workspace_status(existing_id).await?;
                anyhow::ensure!(
                    status.ready
                        && status.embedding_space_id.as_deref()
                            == Some(expected_embedding_space_id),
                    "existing restored corpus is not ready in the expected embedding space"
                );
                return Ok(status);
            }
            // Its directory is about to be replaced: a window showing it, or
            // the default, would be left reading files that are gone.
            anyhow::ensure!(
                !self.views.read().holds(existing_id),
                "the corpus being restored over is open in a window"
            );
            let context = { self.contexts.lock().remove(existing_id) };
            if let Some(context) = context {
                context.shutdown().await;
            }
            std::fs::remove_dir_all(&root)?;
        }

        let final_root = workspace_root(&self.app_data_dir, expected_corpus_id);
        anyhow::ensure!(!final_root.exists(), "restored corpus id already exists");
        let staging = final_root.with_extension(format!("restore-{}", uuid::Uuid::new_v4()));
        copy_workspace_backup(&backup_root, &staging)?;
        let managed_sources = final_root.join("managed_sources");
        let semantic = manifest.semantic.as_mut().ok_or_else(|| {
            anyhow::anyhow!("managed workspace manifest has no semantic configuration")
        })?;
        semantic.index_path = Some(final_root.join("semantic_index.db"));
        manifest.favorites = vec![managed_sources.clone()];
        manifest.active_root = Some(managed_sources);
        manifest.recent_roots.clear();
        write_manifest(&staging.join("workspace.json"), &manifest)?;

        let restored_index = SemanticIndex::open_for_maintenance(&staging)?;
        anyhow::ensure!(
            restored_index.embedding_space_identity()?.id().0 == expected_embedding_space_id,
            "restored index embedding-space mismatch"
        );
        let completeness = restored_index.managed_completeness()?;
        anyhow::ensure!(
            completeness.1 == completeness.2,
            "restored managed index is incomplete"
        );
        drop(restored_index);
        std::fs::rename(&staging, &final_root)?;

        {
            let _registry_guard = self.registry_lock.lock();
            let mut registry = load_registry(&self.app_data_dir)?;
            if let Some(existing_id) = existing {
                registry.workspace_ids.retain(|id| id != &existing_id);
            }
            if !registry
                .workspace_ids
                .iter()
                .any(|id| id == expected_corpus_id)
            {
                registry.workspace_ids.push(expected_corpus_id.to_string());
            }
            atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
        }
        drop(_switch_guard);
        self.managed_workspace_status(expected_corpus_id).await
    }
}

fn verify_managed_backup_directory(
    root: &Path,
    backup: &ManagedCorpusBackup,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        backup.files.len() == backup.file_count,
        "backup file count mismatch"
    );
    let mut expected = backup.files.clone();
    expected.sort_by(|a, b| a.path.cmp(&b.path));
    anyhow::ensure!(
        expected.windows(2).all(|pair| pair[0].path != pair[1].path),
        "duplicate path in managed backup manifest"
    );
    for file in &expected {
        let relative = Path::new(&file.path);
        anyhow::ensure!(
            !relative.is_absolute()
                && !relative
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "unsafe path in managed backup"
        );
        let path = root.join(relative);
        let metadata = std::fs::metadata(&path)?;
        anyhow::ensure!(metadata.len() == file.byte_len, "backup size mismatch");
        anyhow::ensure!(
            wilkes_core::embed::identity::sha256_file(&path)? == file.sha256,
            "backup digest mismatch"
        );
    }
    let mut actual = crate::context::managed_backup_files(root)?;
    actual.retain(|file| file.path != "backup-manifest.json");
    actual.sort_by(|a, b| a.path.cmp(&b.path));
    anyhow::ensure!(
        actual == expected,
        "backup contents do not match the manifest inventory"
    );
    Ok(())
}

fn copy_workspace_backup(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)?;
    for name in ["workspace.json", "semantic_index.db"] {
        std::fs::copy(source.join(name), destination.join(name))?;
    }
    copy_workspace_tree(
        &source.join("managed_sources"),
        &destination.join("managed_sources"),
    )
}

fn copy_workspace_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_workspace_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target)?;
        } else {
            anyhow::bail!(
                "managed restore refuses non-file entry {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
