use parking_lot::{Mutex as PLMutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
use crate::context::{AppContext, EventEmitter, IndexSpace, ManagedCorpusBackup};

const REGISTRY_VERSION: u32 = 1;
const MANIFEST_VERSION: u32 = 1;

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
    pub active_workspace_id: String,
    pub workspaces: Vec<WorkspaceSummary>,
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
    active_workspace_id: String,
    workspace_ids: Vec<String>,
}

pub(crate) fn workspace_root(app_data_dir: &Path, id: &str) -> PathBuf {
    app_data_dir.join("workspaces").join(id)
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
    let (id, manifest) = if plan_path.exists() {
        let plan: MigrationPlan = serde_json::from_slice(&std::fs::read(&plan_path)?)?;
        anyhow::ensure!(
            plan.manifest.id == plan.workspace_id,
            "interrupted workspace migration records a manifest for a different workspace"
        );
        (plan.workspace_id, plan.manifest)
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        let legacy = read_legacy_roots(settings_path)?;
        let mut manifest = WorkspaceManifest::new(id.clone(), "Default".to_string());
        manifest.favorites = legacy.favorites;
        manifest.recent_roots = legacy.recent_roots;
        manifest.active_root = legacy.active_root;
        manifest.semantic = legacy.semantic;
        (id, manifest)
    };

    let workspace_dir = workspace_root(app_data_dir, &id);
    let moves = plan_legacy_moves(app_data_dir, settings_path, &workspace_dir)?;
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
    })
}

pub struct WorkspaceManager {
    app_data_dir: PathBuf,
    settings_path: PathBuf,
    events: Arc<dyn EventEmitter>,
    active_event_workspace_id: Arc<RwLock<String>>,
    active: RwLock<Arc<AppContext>>,
    /// Contexts opened for workspaces that are *not* active, so a request can
    /// name one without the registry — and the user's window — moving.
    ///
    /// Kept rather than opened per request because a context owns the index
    /// and metadata handles for its workspace: reopening them for every
    /// exported document would pay that cost per file. The invariant that
    /// makes the map safe is that a workspace appears here only while it is
    /// not active — [`Self::switch`] retires the entry for the workspace it
    /// activates, so its databases are never held by two contexts at once.
    scoped: PLMutex<std::collections::HashMap<String, Arc<AppContext>>>,
    registry_lock: PLMutex<()>,
    switch_lock: Mutex<()>,
}

struct WorkspaceEventEmitter {
    workspace_id: String,
    active_workspace_id: Arc<RwLock<String>>,
    inner: Arc<dyn EventEmitter>,
}

impl EventEmitter for WorkspaceEventEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        if *self.active_workspace_id.read() == self.workspace_id {
            self.inner.emit(name, payload);
        }
    }
}

fn scoped_events(
    workspace_id: &str,
    active_workspace_id: Arc<RwLock<String>>,
    events: Arc<dyn EventEmitter>,
) -> Arc<dyn EventEmitter> {
    Arc::new(WorkspaceEventEmitter {
        workspace_id: workspace_id.to_string(),
        active_workspace_id,
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
        events: Arc<dyn EventEmitter>,
    ) -> anyhow::Result<(
        Arc<Self>,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    )> {
        let id = initialize_workspace_registry(&app_data_dir, &settings_path)?;
        let active_event_workspace_id = Arc::new(RwLock::new(id.clone()));
        let data_dir = workspace_root(&app_data_dir, &id);
        let manifest_path = workspace_manifest_path(&app_data_dir, &id);
        let paths = WorkerPaths::resolve(&app_data_dir);
        let (ctx, event_rx, loop_fut) = AppContext::new_scoped(
            data_dir,
            app_data_dir.clone(),
            settings_path.clone(),
            manifest_path,
            paths,
            scoped_events(
                &id,
                Arc::clone(&active_event_workspace_id),
                Arc::clone(&events),
            ),
        );
        Ok((
            Arc::new(Self {
                app_data_dir,
                settings_path,
                events,
                active_event_workspace_id,
                active: RwLock::new(ctx),
                scoped: PLMutex::new(std::collections::HashMap::new()),
                registry_lock: PLMutex::new(()),
                switch_lock: Mutex::new(()),
            }),
            event_rx,
            loop_fut,
        ))
    }

    pub fn active(&self) -> Arc<AppContext> {
        Arc::clone(&self.active.read())
    }

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

    /// The context serving one workspace, opening it if it is not the active
    /// one. Activating nothing: the registry, the desktop window and the
    /// active context are left exactly as they were.
    ///
    /// This is what lets a caller work across workspaces at all. Every
    /// document endpoint reads the index of the context it is given, and each
    /// workspace has its own — so before this, reaching a document meant
    /// [`Self::switch`], which rewrites the registry on disk, shuts the
    /// previous context down and moves the user's window. A read had to
    /// perform a write to happen.
    pub async fn context_for(&self, id: &str) -> anyhow::Result<Arc<AppContext>> {
        // The same lock `switch` takes: while this decides whether `id` is
        // active and opens a context for it, the active workspace cannot move
        // out from under the decision.
        let _guard = self.switch_lock.lock().await;
        {
            let _registry_guard = self.registry_lock.lock();
            let registry = load_registry(&self.app_data_dir)?;
            anyhow::ensure!(
                registry.workspace_ids.iter().any(|item| item == id),
                "Unknown workspace"
            );
            if registry.active_workspace_id == id {
                return Ok(self.active());
            }
        }
        if let Some(ctx) = self.scoped.lock().get(id) {
            return Ok(Arc::clone(ctx));
        }

        let (ctx, event_rx, loop_fut) = AppContext::new_scoped(
            workspace_root(&self.app_data_dir, id),
            self.app_data_dir.clone(),
            self.settings_path.clone(),
            workspace_manifest_path(&self.app_data_dir, id),
            WorkerPaths::resolve(&self.app_data_dir),
            // The same scoped emitter the active context gets: it drops every
            // event while its workspace is not active, so opening one of these
            // cannot make the window report progress from a workspace the user
            // is not looking at.
            scoped_events(
                id,
                Arc::clone(&self.active_event_workspace_id),
                Arc::clone(&self.events),
            ),
        );
        ctx.clone().spawn_background_tasks(event_rx, loop_fut);
        self.scoped.lock().insert(id.to_string(), Arc::clone(&ctx));
        Ok(ctx)
    }

    /// Shuts down every context this manager owns — the active one and any
    /// opened for other workspaces.
    ///
    /// Exists because `active()` is no longer the whole answer: a context
    /// opened by [`Self::context_for`] owns worker processes, a directory
    /// watcher and index handles exactly like the active one, so shutting only
    /// the active workspace down at exit leaves those running past the signal
    /// that asked the process to stop.
    pub async fn shutdown_all(&self) {
        let scoped: Vec<Arc<AppContext>> = self.scoped.lock().drain().map(|(_, ctx)| ctx).collect();
        for ctx in scoped {
            ctx.shutdown().await;
        }
        self.active().shutdown().await;
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
            write_manifest(&manifest_path, &manifest)?;
            registry.workspace_ids.push(id);
            atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
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

    pub async fn switch(self: &Arc<Self>, id: &str) -> anyhow::Result<WorkspaceState> {
        let _guard = self.switch_lock.lock().await;
        let mut already_active = false;
        let mut retired: Option<Arc<AppContext>> = None;
        {
            let _registry_guard = self.registry_lock.lock();
            let registry = load_registry(&self.app_data_dir)?;
            anyhow::ensure!(
                registry.workspace_ids.iter().any(|item| item == id),
                "Unknown workspace"
            );
            if registry.active_workspace_id == id {
                already_active = true;
            }
        }
        if already_active {
            return read_workspace_state(&self.app_data_dir, &self.settings_path).await;
        }
        // A workspace this manager opened as a non-active context is retired
        // before it becomes the active one: its databases must be held by one
        // context, and from here on that is the context installed below.
        if let Some(previous_scoped) = self.scoped.lock().remove(id) {
            retired = Some(previous_scoped);
        }
        if let Some(previous_scoped) = retired {
            previous_scoped.shutdown().await;
        }

        let data_dir = workspace_root(&self.app_data_dir, id);
        let manifest_path = workspace_manifest_path(&self.app_data_dir, id);
        let paths = WorkerPaths::resolve(&self.app_data_dir);
        let (ctx, event_rx, loop_fut) = AppContext::new_scoped(
            data_dir,
            self.app_data_dir.clone(),
            self.settings_path.clone(),
            manifest_path,
            paths,
            scoped_events(
                id,
                Arc::clone(&self.active_event_workspace_id),
                Arc::clone(&self.events),
            ),
        );
        let previous = {
            let _registry_guard = self.registry_lock.lock();
            let mut registry = load_registry(&self.app_data_dir)?;
            anyhow::ensure!(
                registry.workspace_ids.iter().any(|item| item == id),
                "Unknown workspace"
            );
            registry.active_workspace_id = id.to_string();
            atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
            *self.active_event_workspace_id.write() = id.to_string();
            ctx.clone().spawn_background_tasks(event_rx, loop_fut);
            std::mem::replace(&mut *self.active.write(), ctx)
        };
        previous.shutdown().await;
        let state = self.state().await?;
        self.events.emit(
            "workspace-changed",
            serde_json::to_value(&state).unwrap_or_default(),
        );
        Ok(state)
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
        // The same lock `switch` and `context_for` take: no workspace may
        // become active, and no context may be opened for one, between the
        // decision below and the removal that follows it.
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
            anyhow::ensure!(
                !removals.contains(&registry.active_workspace_id),
                "The active workspace cannot be deleted. Switch to another workspace first."
            );
            removals
        };

        // Contexts first, registry second, files last. A context holds this
        // workspace's index and metadata databases open, and the registry is
        // what makes the workspace real: an interruption after the registry
        // is written leaves files nothing names, which is recoverable, while
        // the opposite order would leave the registry naming a workspace
        // whose manifest is gone — and every listing reads every manifest.
        for removal in &removals {
            let scoped = { self.scoped.lock().remove(removal) };
            if let Some(context) = scoped {
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
                let id = uuid::Uuid::new_v4().to_string();
                let managed_sources =
                    workspace_root(&self.app_data_dir, &id).join("managed_sources");
                std::fs::create_dir_all(&managed_sources)?;
                semantic.index_path =
                    Some(workspace_root(&self.app_data_dir, &id).join("semantic_index.db"));
                let manifest = WorkspaceManifest {
                    version: MANIFEST_VERSION,
                    id: id.clone(),
                    name: format!("{owner} semantic corpus"),
                    kind: WorkspaceKind::ApplicationManaged {
                        owner: owner.to_string(),
                        purpose: "semantic-corpus".to_string(),
                        corpus_key: corpus_key.to_string(),
                        parent_corpus_id: None,
                    },
                    favorites: vec![managed_sources.clone()],
                    recent_roots: Vec::new(),
                    active_root: Some(managed_sources),
                    semantic: Some(semantic),
                };
                write_manifest(&workspace_manifest_path(&self.app_data_dir, &id), &manifest)?;
                registry.workspace_ids.push(id.clone());
                atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
                id
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
        let id = uuid::Uuid::new_v4().to_string();
        let root = workspace_root(&self.app_data_dir, &id);
        let managed_sources = root.join("managed_sources");
        std::fs::create_dir_all(&managed_sources)?;
        let mut semantic = parent_semantic.clone();
        semantic.selected = embedding.clone();
        semantic.index_path = Some(root.join("semantic_index.db"));
        let manifest = WorkspaceManifest {
            version: MANIFEST_VERSION,
            id: id.clone(),
            name: format!("{owner} embedding projection"),
            kind: WorkspaceKind::ApplicationManaged {
                owner: owner.to_string(),
                purpose: "semantic-corpus".to_string(),
                corpus_key: corpus_key.to_string(),
                parent_corpus_id: Some(corpus_id.to_string()),
            },
            favorites: vec![managed_sources.clone()],
            recent_roots: Vec::new(),
            active_root: Some(managed_sources),
            semantic: Some(semantic),
        };
        write_manifest(&workspace_manifest_path(&self.app_data_dir, &id), &manifest)?;
        registry.workspace_ids.push(id.clone());
        atomic_write_json(&registry_path(&self.app_data_dir), &registry)?;
        Ok(id)
    }

    /// Adds one derived embedding projection to an existing managed corpus.
    /// The child workspace is an implementation detail: callers continue to
    /// address the canonical `corpus_id` plus the returned opaque space id.
    pub async fn ensure_managed_space(
        &self,
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
        let status = self.managed_workspace_status(&request.corpus_id).await?;
        if let Some(level) = status
            .spaces
            .iter()
            .find(|space| space.workspace_id == id)
            .filter(|space| space.ready && space.indexed_generation == status.corpus_generation)
        {
            return Ok(level.clone());
        }

        self.catch_up_projection(&request.corpus_id, &id, None)
            .await?;

        let canonical = self.managed_workspace_status(&request.corpus_id).await?;
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
        child
            .ensure_managed_runtime()
            .await
            .map_err(anyhow::Error::msg)?;
        let canonical_context = self.context_for(corpus_id).await?;
        canonical_context
            .ensure_managed_index()
            .await
            .map_err(anyhow::Error::msg)?;

        // The projection computes vectors only: it neither copies sources nor
        // repeats extraction or chunking. Its passages are the canonical
        // admitted rendition's passages, which is what keeps every space's
        // chunk refs identical.
        let sources = canonical_context
            .managed_admitted_sources()
            .map_err(anyhow::Error::msg)?;
        for (source, rendition) in sources {
            // Keyed by the rendition, not by the source bytes. The key is
            // bound to a rendition for the life of the index and nothing can
            // rebind it, so a key that names less than what it is bound to
            // becomes a tombstone the moment the unnamed part moves: the
            // source digest survives a re-extraction, the rendition does not,
            // and a corpus whose recipe had changed could never bring a
            // projection level again.
            child
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
        &self,
        corpus_id: &str,
        source_workspace: Option<Arc<AppContext>>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let status = self.managed_workspace_status(corpus_id).await?;
        let mut failures = Vec::new();
        for space in status.spaces.iter().filter(|space| !space.primary) {
            if space.ready && space.indexed_generation == status.corpus_generation {
                continue;
            }
            if let Err(error) = self
                .catch_up_projection(corpus_id, &space.workspace_id, source_workspace.clone())
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
        let status = self.managed_workspace_status(corpus_id).await?;
        let space = status
            .spaces
            .iter()
            .find(|space| space.embedding_space_id == embedding_space_id)
            .ok_or_else(|| {
                consumer_error(
                    ConsumerErrorCode::EmbeddingSpaceMismatch,
                    format!("the corpus holds no embedding space {embedding_space_id}"),
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
            let registry = {
                let _guard = self.registry_lock.lock();
                load_registry(&self.app_data_dir)?
            };
            for workspace_id in registry.workspace_ids {
                if workspace_id == corpus_id {
                    continue;
                }
                let child =
                    read_manifest(&workspace_manifest_path(&self.app_data_dir, &workspace_id))?;
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
                status.spaces.push(ManagedEmbeddingSpaceStatus {
                    embedding_space_id: child_identity.id().0,
                    embedding_space_identity: child_identity,
                    ready: counts.1 == counts.2 && indexed_generation == status.corpus_generation,
                    indexed_generation,
                    workspace_id,
                    primary: false,
                });
            }
            status.spaces.sort_by(|left, right| {
                right
                    .primary
                    .cmp(&left.primary)
                    .then_with(|| left.embedding_space_id.cmp(&right.embedding_space_id))
            });
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
            let scoped_context = { self.scoped.lock().remove(existing_id) };
            if let Some(context) = scoped_context {
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
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use wilkes_core::types::{EmbedderModel, EmbeddingEngine};

    struct NoopEmitter;

    impl EventEmitter for NoopEmitter {
        fn emit(&self, _name: &str, _payload: serde_json::Value) {}
    }

    struct RecordingEmitter(Arc<StdMutex<Vec<String>>>);

    impl EventEmitter for RecordingEmitter {
        fn emit(&self, name: &str, _payload: serde_json::Value) {
            self.0.lock().unwrap().push(name.to_string());
        }
    }

    /// Deleting is not writing into a corpus, so a managed one is deletable
    /// exactly like a user workspace — and its hidden projections go with it,
    /// because a projection of a corpus that no longer exists is nothing.
    #[tokio::test]
    async fn deleting_a_managed_corpus_removes_its_projections_and_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-delete".to_string(),
                embedding: SelectedEmbedder::default(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let parent_semantic = read_manifest(&workspace_manifest_path(
            dir.path(),
            &corpus.corpus_id,
        ))
        .unwrap()
        .semantic
        .unwrap();
        let projection = manager
            .ensure_projection_workspace(
                &corpus.corpus_id,
                "underdog",
                "store-delete",
                &parent_semantic,
                &SelectedEmbedder {
                    engine: EmbeddingEngine::Candle,
                    model: EmbedderModel("projection-model".to_string()),
                    dimension: 2,
                },
            )
            .unwrap();
        assert_ne!(projection, corpus.corpus_id);

        let state = manager.delete(&corpus.corpus_id).await.unwrap();

        assert!(
            !state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == corpus.corpus_id),
            "the corpus is gone from the listing"
        );
        let registry = load_registry(dir.path()).unwrap();
        assert!(!registry.workspace_ids.contains(&corpus.corpus_id));
        assert!(
            !registry.workspace_ids.contains(&projection),
            "the projection went with the corpus that owned it"
        );
        assert!(!workspace_root(dir.path(), &corpus.corpus_id).exists());
        assert!(!workspace_root(dir.path(), &projection).exists());

        manager.shutdown_all().await;
    }

    /// The registry's own invariant is that the active workspace is one it
    /// names. Deleting it would move the user's window as a side effect of a
    /// delete, so it is refused and the caller switches first.
    #[tokio::test]
    async fn the_active_workspace_is_refused_and_left_intact() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let active = manager.active_workspace_id().unwrap();

        let error = manager.delete(&active).await.unwrap_err().to_string();
        assert!(
            error.contains("active workspace cannot be deleted"),
            "unexpected refusal: {error}"
        );
        assert!(workspace_root(dir.path(), &active).exists());
        assert_eq!(manager.active_workspace_id().unwrap(), active);

        // And unknown ids are refused rather than quietly succeeding.
        assert!(manager.delete("not-a-workspace").await.is_err());

        manager.shutdown_all().await;
    }

    /// A workspace deleted while the caller was reading it: the context this
    /// manager opened for it is shut down before its files go, and the
    /// manager will not hand out another one afterwards.
    #[tokio::test]
    async fn deleting_a_workspace_retires_the_context_opened_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let created = manager.create("Second".to_string()).await.unwrap();
        manager.context_for(&created.id).await.unwrap();
        assert!(manager.scoped.lock().contains_key(&created.id));

        manager.delete(&created.id).await.unwrap();

        assert!(!manager.scoped.lock().contains_key(&created.id));
        assert!(!workspace_root(dir.path(), &created.id).exists());
        assert!(
            manager.context_for(&created.id).await.is_err(),
            "a deleted workspace cannot be opened again"
        );

        manager.shutdown_all().await;
    }

    /// The canonical corpus opens its index and loads no model.
    ///
    /// This is the payoff, and it is the symptom that started the whole
    /// investigation: a Wilkes log showing `load_embedder start:
    /// model=AllMiniLML6V2` on a store whose settings named a different model
    /// entirely. The canonical corpus was being ensured with an embedder it
    /// had no use for, chosen when the corpus was created and unreachable
    /// from any setting since.
    #[tokio::test]
    async fn the_canonical_corpus_opens_an_index_and_loads_no_model() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-index-only".to_string(),
                embedding: SelectedEmbedder::default(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();

        let context = manager.context_for(&corpus.corpus_id).await.unwrap();
        context
            .ensure_managed_index()
            .await
            .expect("an index with no model behind it");

        // An index, so passages have somewhere to live.
        assert!(
            dir.path()
                .join("workspaces")
                .join(&corpus.corpus_id)
                .join("semantic_index.db")
                .exists()
                || workspace_root(dir.path(), &corpus.corpus_id)
                    .join("semantic_index.db")
                    .exists(),
            "the canonical corpus has an index"
        );
        // And no model. Nothing installed it, nothing loaded it, and no worker
        // was asked for one — which is why this test can run at all without a
        // downloadable embedder.
        assert!(
            !context.has_embedder(),
            "the canonical corpus holds no embedder"
        );

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn a_corpus_without_an_index_reports_no_embedding_space() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, Arc::clone(&events)).unwrap();
        let embedding = SelectedEmbedder::default();
        let status = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-empty".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        // The canonical corpus holds no vectors and names no coordinate
        // system. It owns sources, extraction, chunking and passage identity;
        // its projections own the spaces, one model each.
        assert_eq!(status.embedding_space_id, None);
        assert!(status.embedding_space_identity.is_none());
        assert!(status.spaces.is_empty());
        // Not ready only because nothing has opened an index for it yet.
        // Readiness here is membership, never coordinates.
        assert!(!status.ready);

        // The absent space is reported as an explicit null, not by dropping
        // the key: a client reading this API can tell "no space yet" apart
        // from a field it forgot to handle.
        let wire = serde_json::to_value(&status).unwrap();
        assert_eq!(wire["embedding_space_id"], serde_json::Value::Null);
        assert_eq!(wire["embedding_space_identity"], serde_json::Value::Null);

        let root = workspace_root(dir.path(), &status.corpus_id);
        let index = SemanticIndex::create(
            &root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            None,
        )
        .unwrap();
        let space = index.embedding_space_identity().unwrap().id().0;
        drop(index);

        // And it still reports none once an index exists. The index is where
        // passages live; the space it was created under is an artifact of the
        // schema, not something this corpus serves, and publishing it is what
        // made a consumer pin a coordinate system it could not change and a
        // model it never chose.
        let status = manager
            .managed_workspace_status(&status.corpus_id)
            .await
            .unwrap();
        assert_eq!(status.embedding_space_id, None);
        assert!(status.spaces.is_empty());
        assert!(status.ready, "an open index is a ready canonical corpus");
        let _ = space;
    }

    /// Writes one managed document into a corpus index the way an admission
    /// does, so a test can move a corpus's membership generation without a
    /// downloadable embedding model.
    fn admit(
        index: &mut wilkes_core::embed::index::SemanticIndex,
        path: &Path,
        text: &str,
        vector: Vec<f32>,
        recipe: &wilkes_core::embed::ExtractionRecipe,
    ) {
        std::fs::write(path, text).unwrap();
        index
            .write_file_with_recipe(
                wilkes_core::embed::index::db::PreparedFile {
                    retained: Default::default(),
                    path: path.to_path_buf(),
                    full_text: text.to_string(),
                    chunks: vec![(
                        wilkes_core::embed::index::chunk::Chunk {
                            file_path: path.to_path_buf(),
                            text: text.to_string(),
                            byte_range: wilkes_core::types::ByteRange {
                                start: 0,
                                end: text.len(),
                            },
                            origin: wilkes_core::types::SourceOrigin::TextFile { line: 1, col: 1 },
                        },
                        vector,
                    )],
                },
                recipe,
                None,
                None,
                true,
                false,
                Some(&format!("admission-{}", path.display())),
            )
            .unwrap();
    }

    /// The invariant the whole multi-space design exists to hold: one corpus
    /// owns membership, its projections are internal, and a projection that
    /// has not indexed the corpus's current membership cannot be routed to.
    #[tokio::test]
    async fn a_projection_is_internal_and_may_not_serve_membership_it_lacks() {
        let dir = tempfile::tempdir().unwrap();
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) = WorkspaceManager::new(
            dir.path().to_path_buf(),
            dir.path().join("global-settings.json"),
            Arc::clone(&events),
        )
        .unwrap();
        let embedding = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("primary-model".to_string()),
            dimension: 2,
        };
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-spaces".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let corpus_id = corpus.corpus_id.clone();
        let canonical_root = workspace_root(dir.path(), &corpus_id);
        let canonical_sources = canonical_root.join("managed_sources");
        let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
        let document = canonical_sources.join("document.txt");
        let mut canonical = SemanticIndex::create(
            &canonical_root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            Some(&canonical_sources),
        )
        .unwrap();
        admit(
            &mut canonical,
            &document,
            "canonical passage",
            vec![1.0, 0.0],
            &recipe,
        );

        // The projection's manifest is created by the same code the ensure
        // endpoint uses; only its vectors are supplied here, because computing
        // them for real needs a model this test cannot download.
        let secondary = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("secondary-model".to_string()),
            dimension: 2,
        };
        let manifest = read_manifest(&workspace_manifest_path(dir.path(), &corpus_id)).unwrap();
        let projection_id = manager
            .ensure_projection_workspace(
                &corpus_id,
                "underdog",
                "store-spaces",
                manifest.semantic.as_ref().unwrap(),
                &secondary,
            )
            .unwrap();
        assert_eq!(
            manager
                .ensure_projection_workspace(
                    &corpus_id,
                    "underdog",
                    "store-spaces",
                    manifest.semantic.as_ref().unwrap(),
                    &secondary,
                )
                .unwrap(),
            projection_id,
            "one embedder under one corpus is one projection, however often it is ensured"
        );
        let projection_root = workspace_root(dir.path(), &projection_id);
        let projection_sources = projection_root.join("managed_sources");
        let mut projection = SemanticIndex::create(
            &projection_root,
            secondary.model.model_id(),
            secondary.dimension,
            secondary.engine,
            Some(&projection_sources),
        )
        .unwrap();

        // A projection is an implementation detail: it never appears beside
        // the user's own workspaces, and it is not a corpus of its own.
        let state = manager.state().await.unwrap();
        assert!(
            !state
                .workspaces
                .iter()
                .any(|workspace| workspace.id == projection_id),
            "an embedding projection is not a workspace the user has"
        );

        let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
        let projection_space = status
            .spaces
            .iter()
            .find(|space| space.workspace_id == projection_id)
            .expect("the projection is listed as a space of its corpus")
            .clone();
        // Every space is a projection now: the canonical corpus contributes
        // none, because it holds no vectors to name one with.
        assert!(
            status.spaces.iter().all(|space| !space.primary),
            "the canonical corpus is not one of its own spaces"
        );
        assert!(
            status
                .spaces
                .iter()
                .all(|space| space.workspace_id != corpus_id),
            "the corpus does not appear in its own space list"
        );
        assert_ne!(
            projection_space.indexed_generation, status.corpus_generation,
            "an empty projection has not indexed the document the corpus admitted"
        );
        assert!(!projection_space.ready);

        // Refused by generation, not by absence: the space exists, is named
        // correctly, and still may not answer a query.
        let error = manager
            .managed_space_context(&corpus_id, &projection_space.embedding_space_id)
            .await
            .err()
            .expect("a projection behind the corpus must not serve");
        assert!(
            format!("{error:#}").contains("EMBEDDING_SPACE_STALE"),
            "{error:#}"
        );

        // Catching up is the canonical rendition re-embedded, not re-extracted:
        // the chunk structure comes from the corpus, only the coordinates are
        // the projection's own.
        let (mut prepared, canonical_recipe_id) = canonical
            .managed_file_structure_for_reembedding(&document, &document)
            .unwrap()
            .expect("the canonical rendition is admitted");
        assert_eq!(
            canonical_recipe_id,
            recipe.id(),
            "the recipe comes from the rendition, not from the projection"
        );
        prepared.chunks[0].1 = vec![0.0, 1.0];
        projection
            .write_file_with_recipe_id(
                prepared,
                &canonical_recipe_id,
                None,
                None,
                true,
                false,
                Some("projected"),
            )
            .unwrap();

        let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
        let caught_up = status
            .spaces
            .iter()
            .find(|space| space.workspace_id == projection_id)
            .unwrap();
        assert_eq!(caught_up.indexed_generation, status.corpus_generation);
        assert!(caught_up.ready);
        // Two models are two coordinate systems under one corpus. Compared
        // against the embedders themselves rather than against a canonical
        // space, which no longer exists to compare with.
        let space_of = |selected: &SelectedEmbedder| {
            wilkes_core::embed::identity::EmbeddingSpaceIdentity::for_test(
                selected.engine,
                selected.model.model_id(),
                selected.dimension,
            )
            .id()
            .0
        };
        assert_eq!(caught_up.embedding_space_id, space_of(&secondary));
        assert_ne!(caught_up.embedding_space_id, space_of(&embedding));
        assert!(
            manager
                .managed_space_context(&corpus_id, &caught_up.embedding_space_id)
                .await
                .is_ok(),
            "a projection at the corpus generation serves"
        );

        // Admitting one more document puts the corpus ahead again, and the
        // projection stops serving until it follows.
        admit(
            &mut canonical,
            &canonical_sources.join("second.txt"),
            "a second canonical passage",
            vec![0.5, 0.5],
            &recipe,
        );
        let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
        let lagging = status
            .spaces
            .iter()
            .find(|space| space.workspace_id == projection_id)
            .unwrap();
        assert!(!lagging.ready);
        assert!(
            manager
                .managed_space_context(&corpus_id, &lagging.embedding_space_id)
                .await
                .is_err(),
            "membership that only the corpus has is membership no projection may answer for"
        );

        // A projection that is behind is work owed, and the pass that owes it
        // reports per space: this environment has no embedder to load, so the
        // lagging space comes back as a named failure rather than as an error
        // that would have stopped every other space from catching up too.
        let failures = manager.catch_up_corpus(&corpus_id, None).await.unwrap();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].0, lagging.embedding_space_id);
    }

    /// Ensuring a space that is already level is a status read and nothing
    /// else.
    ///
    /// The endpoint behind this is idempotent and callers put to it freely —
    /// on a timer, in one case. Every call used to sweep the corpus, offering
    /// each admitted source to the projection in turn, and the projection took
    /// nothing from any of them. This environment can load no model, so a
    /// sweep is an error: `Ok` here is the proof that none was attempted.
    #[tokio::test]
    async fn ensuring_a_level_space_sweeps_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) = WorkspaceManager::new(
            dir.path().to_path_buf(),
            dir.path().join("global-settings.json"),
            Arc::clone(&events),
        )
        .unwrap();
        let embedding = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("primary-model".to_string()),
            dimension: 2,
        };
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-ensure-level".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let corpus_id = corpus.corpus_id.clone();
        let canonical_root = workspace_root(dir.path(), &corpus_id);
        let canonical_sources = canonical_root.join("managed_sources");
        let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
        let document = canonical_sources.join("document.txt");
        let mut canonical = SemanticIndex::create(
            &canonical_root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            Some(&canonical_sources),
        )
        .unwrap();
        admit(
            &mut canonical,
            &document,
            "canonical passage",
            vec![1.0, 0.0],
            &recipe,
        );

        // The projection is built and brought level by hand, because computing
        // its vectors for real needs a model this test cannot download.
        let secondary = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("secondary-model".to_string()),
            dimension: 2,
        };
        let manifest = read_manifest(&workspace_manifest_path(dir.path(), &corpus_id)).unwrap();
        let projection_id = manager
            .ensure_projection_workspace(
                &corpus_id,
                "underdog",
                "store-ensure-level",
                manifest.semantic.as_ref().unwrap(),
                &secondary,
            )
            .unwrap();
        let projection_root = workspace_root(dir.path(), &projection_id);
        let mut projection = SemanticIndex::create(
            &projection_root,
            secondary.model.model_id(),
            secondary.dimension,
            secondary.engine,
            Some(&projection_root.join("managed_sources")),
        )
        .unwrap();
        let (mut prepared, canonical_recipe_id) = canonical
            .managed_file_structure_for_reembedding(&document, &document)
            .unwrap()
            .expect("the canonical rendition is admitted");
        prepared.chunks[0].1 = vec![0.0, 1.0];
        projection
            .write_file_with_recipe_id(
                prepared,
                &canonical_recipe_id,
                None,
                None,
                true,
                false,
                Some("projected"),
            )
            .unwrap();

        let ensured = manager
            .ensure_managed_space(EnsureManagedEmbeddingSpace {
                corpus_id: corpus_id.clone(),
                embedding: secondary.clone(),
            })
            .await
            .expect("a level space is ensured without catching anything up");
        assert_eq!(ensured.workspace_id, projection_id);
        assert!(ensured.ready);
        let status = manager.managed_workspace_status(&corpus_id).await.unwrap();
        assert_eq!(ensured.indexed_generation, status.corpus_generation);

        // And a space that is genuinely behind still owes the sweep, which
        // this environment cannot perform: the skip is the level check, not a
        // blanket refusal to catch up.
        admit(
            &mut canonical,
            &canonical_sources.join("second.txt"),
            "a second canonical passage",
            vec![0.5, 0.5],
            &recipe,
        );
        assert!(
            manager
                .ensure_managed_space(EnsureManagedEmbeddingSpace {
                    corpus_id: corpus_id.clone(),
                    embedding: secondary,
                })
                .await
                .is_err(),
            "a projection behind its corpus is work owed, not work skipped"
        );
    }

    /// The cheap path has to stay cheap, because it is the one that runs after
    /// every import: a corpus whose projections are all level must decide that
    /// from the membership digests alone, without loading a single model.
    #[tokio::test]
    async fn catching_up_a_level_corpus_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) = WorkspaceManager::new(
            dir.path().to_path_buf(),
            dir.path().join("global-settings.json"),
            Arc::clone(&events),
        )
        .unwrap();
        let embedding = SelectedEmbedder {
            engine: EmbeddingEngine::Candle,
            model: EmbedderModel("primary-model".to_string()),
            dimension: 2,
        };
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-level".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let root = workspace_root(dir.path(), &corpus.corpus_id);
        let recipe = wilkes_core::embed::ExtractionRecipe::new(600, 128);
        let mut canonical = SemanticIndex::create(
            &root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            Some(&root.join("managed_sources")),
        )
        .unwrap();
        admit(
            &mut canonical,
            &root.join("managed_sources").join("document.txt"),
            "canonical passage",
            vec![1.0, 0.0],
            &recipe,
        );
        // No projection exists, so nothing is behind — and a corpus with one
        // that is level takes the same path.
        assert!(manager
            .catch_up_corpus(&corpus.corpus_id, None)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn managed_backup_restores_over_only_an_empty_same_store_corpus() {
        let source = tempfile::tempdir().unwrap();
        let source_settings = source.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (source_manager, _events, _worker_loop) = WorkspaceManager::new(
            source.path().to_path_buf(),
            source_settings,
            Arc::clone(&events),
        )
        .unwrap();
        let embedding = SelectedEmbedder::default();
        let source_status = source_manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-restore".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let source_root = workspace_root(source.path(), &source_status.corpus_id);
        std::fs::write(
            source_root.join("managed_sources").join("retained.txt"),
            b"retained across restore",
        )
        .unwrap();
        let index = SemanticIndex::create(
            &source_root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            None,
        )
        .unwrap();
        let space = index.embedding_space_identity().unwrap().id().0;
        drop(index);
        let source_backup = source.path().join("managed_backups").join("exported");
        crate::context::backup_managed_directory(
            &source_root,
            &source_status.corpus_id,
            &space,
            &source_backup,
        )
        .unwrap();

        let target = tempfile::tempdir().unwrap();
        let (target_manager, _events, _worker_loop) = WorkspaceManager::new(
            target.path().to_path_buf(),
            target.path().join("global-settings.json"),
            events,
        )
        .unwrap();
        let empty = target_manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-restore".to_string(),
                embedding,
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        assert_ne!(empty.corpus_id, source_status.corpus_id);
        let target_backup = target.path().join("managed_backups").join("inbox");
        std::fs::create_dir_all(target_backup.join("managed_sources")).unwrap();
        for name in [
            "workspace.json",
            "semantic_index.db",
            "backup-manifest.json",
        ] {
            std::fs::copy(source_backup.join(name), target_backup.join(name)).unwrap();
        }
        std::fs::copy(
            source_backup.join("managed_sources").join("retained.txt"),
            target_backup.join("managed_sources").join("retained.txt"),
        )
        .unwrap();
        std::fs::write(target_backup.join("unlisted.txt"), b"not in manifest").unwrap();
        let inventory_error = target_manager
            .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
            .await
            .unwrap_err();
        assert!(inventory_error
            .to_string()
            .contains("contents do not match the manifest inventory"));
        std::fs::remove_file(target_backup.join("unlisted.txt")).unwrap();

        let restored = target_manager
            .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
            .await
            .unwrap();
        assert_eq!(restored.corpus_id, source_status.corpus_id);
        // The restored corpus names no space, like every canonical corpus: the
        // backup carries its passages and their identity, which is what it is
        // for. `space` is still what addresses the backup itself.
        assert_eq!(restored.embedding_space_id, None);
        assert!(restored.ready);
        // Restore replaces the pre-created corpus rather than adding a second
        // one: the listing shows the user's own workspace and exactly one
        // managed corpus.
        let listed = target_manager.state().await.unwrap().workspaces;
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed
                .iter()
                .filter(|workspace| workspace.read_only)
                .count(),
            1
        );
        let retried = target_manager
            .restore_managed_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
            .await
            .unwrap();
        assert_eq!(retried.corpus_id, restored.corpus_id);
        assert!(retried.ready);
        assert_eq!(
            std::fs::read(
                workspace_root(target.path(), &source_status.corpus_id)
                    .join("managed_sources")
                    .join("retained.txt")
            )
            .unwrap(),
            b"retained across restore"
        );
        source_manager.shutdown_all().await;
        target_manager.shutdown_all().await;
    }

    #[test]
    fn retired_workspace_events_are_suppressed() {
        let active = Arc::new(RwLock::new("a".to_string()));
        let names = Arc::new(StdMutex::new(Vec::new()));
        let inner: Arc<dyn EventEmitter> = Arc::new(RecordingEmitter(Arc::clone(&names)));
        let a = scoped_events("a", Arc::clone(&active), Arc::clone(&inner));
        let b = scoped_events("b", Arc::clone(&active), inner);

        a.emit("a-live", serde_json::Value::Null);
        *active.write() = "b".to_string();
        a.emit("a-late", serde_json::Value::Null);
        b.emit("b-live", serde_json::Value::Null);

        assert_eq!(*names.lock().unwrap(), vec!["a-live", "b-live"]);
    }

    #[tokio::test]
    async fn fresh_registry_creates_one_empty_default_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let id = initialize_workspace_registry(dir.path(), &settings_path).unwrap();
        let state = read_workspace_state(dir.path(), &settings_path)
            .await
            .unwrap();
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].name, "Default");
        assert!(state.workspaces[0].roots.is_empty());
        assert_eq!(state.active_workspace_id, id);
        assert_eq!(state.active_workspace_id, state.workspaces[0].id);
        // A workspace that declares no semantic block still reports what it
        // would embed with, inherited from global settings.
        assert_eq!(
            state.workspaces[0].embedding,
            wilkes_core::types::Settings::default().semantic.selected
        );
    }

    /// A managed corpus is listed, says it is read-only, and can be activated
    /// so the user can search it. Only the writes stay refused.
    #[tokio::test]
    async fn managed_workspace_is_idempotent_listed_and_protected() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let request = EnsureManagedWorkspace {
            owner: "underdog".to_string(),
            corpus_key: "store-1".to_string(),
            embedding: SelectedEmbedder::default(),
            chunk_size: 600,
            chunk_overlap: 128,
        };
        let first = manager
            .ensure_managed_workspace(request.clone())
            .await
            .unwrap();
        let second = manager.ensure_managed_workspace(request).await.unwrap();
        assert_eq!(first.corpus_id, second.corpus_id);
        let state = manager.state().await.unwrap();
        assert_eq!(
            state.workspaces.len(),
            2,
            "the corpus is listed beside the user's own workspace"
        );
        let corpus = state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == first.corpus_id)
            .expect("the managed corpus is listed");
        assert!(corpus.read_only);
        assert_eq!(corpus.managed_by.as_deref(), Some("underdog"));
        assert!(state
            .workspaces
            .iter()
            .any(|workspace| !workspace.read_only && workspace.managed_by.is_none()));

        assert!(manager
            .rename(&first.corpus_id, "Visible".to_string())
            .await
            .unwrap_err()
            .to_string()
            .contains("MANAGED_WORKSPACE_PROTECTED"));

        // Activation is what makes it searchable: every read path the UI has
        // answers for the active workspace.
        let switched = manager.switch(&first.corpus_id).await.unwrap();
        assert_eq!(switched.active_workspace_id, first.corpus_id);
        assert!(manager.active().is_read_only());
        assert_eq!(
            manager.active().ensure_writable().unwrap_err().code(),
            Some(wilkes_core::consumer::ConsumerErrorCode::ManagedWorkspaceProtected)
        );

        // A different embedder is not a mismatch. The canonical corpus holds
        // no vectors, so its recorded embedder decides nothing about what a
        // consumer may ask for; models are chosen per projection. Comparing it
        // here meant this endpoint refused, for the life of the corpus, any
        // request naming a model other than the one it happened to be created
        // with — and a consumer has only its live setting to send.
        let mut other_model = SelectedEmbedder::default();
        other_model.dimension += 1;
        manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-1".to_string(),
                embedding: other_model,
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .expect("a different model is a different projection, not a refusal");

        // Different chunking still is a mismatch, and always will be: every
        // `chunk_ref` a consumer has cited derives from it, and re-chunking is
        // the one thing here nobody can recompute their way out of.
        assert!(manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-1".to_string(),
                embedding: SelectedEmbedder::default(),
                chunk_size: 900,
                chunk_overlap: 128,
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("MANAGED_WORKSPACE_CONFIGURATION_MISMATCH"));
        manager.shutdown_all().await;
    }

    /// The whole alpha library — its databases, their companion files and the
    /// roots the user had open — arrives in one Default workspace, and the
    /// global settings stop being a second answer to what is open.
    #[tokio::test]
    async fn an_alpha_library_is_adopted_into_a_default_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let library = dir.path().join("library");
        std::fs::create_dir_all(&library).unwrap();
        let library = library.canonicalize().unwrap();
        for entry in ["research.db", "research.db-wal", "semantic_index.db"] {
            std::fs::write(dir.path().join(entry), b"legacy").unwrap();
        }
        std::fs::create_dir_all(dir.path().join("uploads")).unwrap();
        std::fs::write(dir.path().join("uploads").join("paper.pdf"), b"pdf").unwrap();
        let mut legacy_settings =
            serde_json::to_value(wilkes_core::types::Settings::default()).unwrap();
        legacy_settings["favorites"] = serde_json::json!([&library]);
        legacy_settings["last_directory"] = serde_json::json!(&library);
        legacy_settings["semantic"]["chunk_size"] = serde_json::json!(777);
        legacy_settings["theme"] = serde_json::json!("Dark");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&legacy_settings).unwrap(),
        )
        .unwrap();

        let id = initialize_workspace_registry(dir.path(), &settings_path).unwrap();

        let workspace_dir = workspace_root(dir.path(), &id);
        for entry in ["research.db", "research.db-wal", "semantic_index.db"] {
            assert!(workspace_dir.join(entry).exists(), "{entry} did not move");
            assert!(!dir.path().join(entry).exists(), "{entry} was left behind");
        }
        assert!(workspace_dir.join("uploads").join("paper.pdf").exists());
        assert!(!dir.path().join(".workspace-migration.json").exists());

        let state = read_workspace_state(dir.path(), &settings_path)
            .await
            .unwrap();
        assert_eq!(state.active_workspace_id, id);
        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(state.workspaces[0].name, "Default");
        assert_eq!(state.workspaces[0].roots, vec![library.clone()]);
        assert_eq!(state.workspaces[0].active_root, Some(library));

        let manifest = read_manifest(&workspace_manifest_path(dir.path(), &id)).unwrap();
        assert_eq!(manifest.semantic.unwrap().chunk_size, 777);

        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(settings.get("theme").unwrap(), "Dark");
        for key in ["favorites", "last_directory", "semantic"] {
            assert!(settings.get(key).is_none(), "{key} was left in settings");
        }
    }

    /// A migration killed between two moves is finished by the next start,
    /// into the workspace it had already committed to.
    #[test]
    fn an_interrupted_migration_resumes_into_the_same_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let id = "11111111-2222-3333-4444-555555555555".to_string();
        let mut manifest = WorkspaceManifest::new(id.clone(), "Default".to_string());
        manifest.active_root = Some(dir.path().join("library"));
        let workspace_dir = workspace_root(dir.path(), &id);
        std::fs::create_dir_all(&workspace_dir).unwrap();
        // Half moved before the interruption, half still in place.
        std::fs::write(workspace_dir.join("research.db"), b"legacy").unwrap();
        std::fs::write(dir.path().join("semantic_index.db"), b"legacy").unwrap();
        atomic_write_json(
            &migration_plan_path(dir.path()),
            &MigrationPlan {
                workspace_id: id.clone(),
                manifest,
            },
        )
        .unwrap();

        let resumed = initialize_workspace_registry(dir.path(), &settings_path).unwrap();

        assert_eq!(resumed, id);
        assert!(workspace_dir.join("semantic_index.db").exists());
        assert!(!dir.path().join("semantic_index.db").exists());
        assert!(!dir.path().join(".workspace-migration.json").exists());
        assert_eq!(
            read_manifest(&workspace_manifest_path(dir.path(), &id))
                .unwrap()
                .active_root,
            Some(dir.path().join("library"))
        );
    }

    /// Two libraries and no way to tell which one the user means: the
    /// migration stops rather than picking, and nothing has moved when it does.
    #[test]
    fn ambiguous_legacy_copies_refuse_to_migrate() {
        let data_dir = tempfile::tempdir().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let settings_path = config_dir.path().join("settings.json");
        std::fs::write(data_dir.path().join("research.db"), b"data").unwrap();
        std::fs::write(config_dir.path().join("research.db"), b"config").unwrap();

        let error = initialize_workspace_registry(data_dir.path(), &settings_path).unwrap_err();

        assert!(error.to_string().contains("refusing to choose"));
        assert!(!data_dir.path().join("workspaces.json").exists());
        assert!(data_dir.path().join("research.db").exists());
        assert!(config_dir.path().join("research.db").exists());
    }

    /// The manager starts on an alpha install instead of refusing to: the
    /// adoption is the first thing it does.
    #[tokio::test]
    async fn the_manager_starts_on_an_alpha_install() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        std::fs::write(dir.path().join("research.db"), b"legacy").unwrap();
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);

        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();

        let state = manager.state().await.unwrap();
        assert_eq!(state.workspaces.len(), 1);
        assert!(workspace_root(dir.path(), &state.active_workspace_id)
            .join("research.db")
            .exists());
        manager.active().shutdown().await;
    }

    #[tokio::test]
    async fn renaming_a_workspace_persists_its_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let id = manager.state().await.unwrap().active_workspace_id;

        let renamed = manager
            .rename(&id, "  Research  ".to_string())
            .await
            .unwrap();

        assert_eq!(renamed.name, "Research");
        assert_eq!(
            manager.state().await.unwrap().workspaces[0].name,
            "Research"
        );
        assert_eq!(
            read_manifest(&workspace_manifest_path(dir.path(), &id))
                .unwrap()
                .name,
            "Research"
        );
        manager.active().shutdown().await;
    }

    #[tokio::test]
    async fn switching_restores_workspace_roots_and_database_directory() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let first = manager.state().await.unwrap().active_workspace_id;
        let first_root = dir.path().join("library-a");
        std::fs::create_dir_all(&first_root).unwrap();
        let first_root = first_root.canonicalize().unwrap();
        manager
            .active()
            .update_settings(serde_json::json!({
                "favorites": [first_root.clone()],
                "last_directory": first_root.clone(),
                "semantic": {
                    "enabled": false,
                    "index_path": null,
                    "chunk_size": 777,
                },
            }))
            .await
            .unwrap();
        std::fs::write(
            manager.active().data_dir.join("research.db"),
            b"workspace-a",
        )
        .unwrap();
        std::fs::write(
            manager.active().data_dir.join("semantic_index.db"),
            b"workspace-a-index",
        )
        .unwrap();

        let second = manager.create("Second".to_string()).await.unwrap();
        manager.switch(&second.id).await.unwrap();
        assert!(manager.active().get_settings().await.favorites.is_empty());
        assert!(!manager.active().data_dir.join("research.db").exists());
        assert!(!manager.active().data_dir.join("semantic_index.db").exists());

        manager.switch(&first).await.unwrap();
        let restored = manager.active().get_settings().await;
        assert_eq!(restored.favorites, vec![first_root.clone()]);
        assert_eq!(restored.last_directory, Some(first_root));
        assert_eq!(restored.semantic.chunk_size, 777);
        assert_eq!(
            std::fs::read(manager.active().data_dir.join("research.db")).unwrap(),
            b"workspace-a"
        );
        assert_eq!(
            std::fs::read(manager.active().data_dir.join("semantic_index.db")).unwrap(),
            b"workspace-a-index"
        );
        manager.active().shutdown().await;
    }

    #[tokio::test]
    async fn a_non_active_workspace_is_reachable_without_activating_it() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let first = manager.state().await.unwrap().active_workspace_id;
        let second = manager.create("Second".to_string()).await.unwrap();

        // The point of the whole mechanism: a context for a workspace nobody
        // activated, reading that workspace's own directory.
        let scoped = manager.context_for(&second.id).await.unwrap();
        assert_eq!(scoped.data_dir, workspace_root(dir.path(), &second.id));
        assert_ne!(scoped.data_dir, manager.active().data_dir);

        // And nothing moved: the registry still names the first workspace, and
        // the active context is the one it always was.
        assert_eq!(
            manager.state().await.unwrap().active_workspace_id,
            first,
            "opening a workspace must not activate it"
        );
        assert_eq!(
            manager.active().data_dir,
            workspace_root(dir.path(), &first)
        );

        // Asked twice, the same context — the index handles are opened once.
        let again = manager.context_for(&second.id).await.unwrap();
        assert!(Arc::ptr_eq(&scoped, &again));

        // Naming the active workspace is not a second context for it: that
        // would put two contexts on one set of databases.
        let active = manager.context_for(&first).await.unwrap();
        assert!(Arc::ptr_eq(&active, &manager.active()));

        // Activating the workspace retires the context opened for it, leaving
        // the installed one as its only owner.
        manager.switch(&second.id).await.unwrap();
        assert!(!Arc::ptr_eq(&manager.active(), &scoped));
        assert_eq!(
            manager.active().data_dir,
            workspace_root(dir.path(), &second.id)
        );
        assert!(manager.scoped.lock().is_empty());

        manager.active().shutdown().await;
    }

    #[tokio::test]
    async fn shutting_down_releases_every_workspace_this_manager_opened() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let second = manager.create("Second".to_string()).await.unwrap();
        let scoped = manager.context_for(&second.id).await.unwrap();

        manager.shutdown_all().await;

        // Both, not just the active one: a context opened for another
        // workspace owns workers and watchers of its own.
        assert!(scoped.is_shutting_down());
        assert!(manager.active().is_shutting_down());
        assert!(manager.scoped.lock().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_workspace_is_an_error_rather_than_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();

        let error = match manager.context_for("no-such-workspace").await {
            Ok(_) => panic!("an unknown workspace must not open a context"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("Unknown workspace"));

        manager.active().shutdown().await;
    }

    #[tokio::test]
    async fn the_catalog_lists_every_workspace_and_resolves_each_without_switching() {
        use wilkes_agent::search::WorkspaceCatalog;

        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, events).unwrap();
        let active_id = manager.state().await.unwrap().active_workspace_id;
        let second = manager.create("Second".to_string()).await.unwrap();

        let listed = manager.workspaces().await.unwrap();
        assert_eq!(listed.len(), 2);
        let active = listed
            .iter()
            .find(|workspace| workspace.active)
            .expect("exactly one workspace is active");
        assert_eq!(active.id, active_id);
        assert!(listed
            .iter()
            .any(|workspace| workspace.id == second.id && !workspace.active));

        // An unnamed call reads the active workspace; a named one reads that
        // workspace and leaves the active one where it was.
        let unnamed = manager.search_for(None).await.unwrap();
        assert!(
            std::ptr::addr_eq(
                Arc::as_ptr(&unnamed).cast::<u8>(),
                Arc::as_ptr(&manager.active()).cast::<u8>()
            ),
            "an unnamed call must read the active workspace's own context"
        );
        let named = manager.search_for(Some(&second.id)).await.unwrap();
        assert!(!std::ptr::addr_eq(
            Arc::as_ptr(&named).cast::<u8>(),
            Arc::as_ptr(&manager.active()).cast::<u8>()
        ));
        assert_eq!(
            manager.state().await.unwrap().active_workspace_id,
            active_id,
            "resolving a workspace must not activate it"
        );

        let error = manager.search_for(Some("no-such-workspace")).await;
        assert!(error
            .err()
            .is_some_and(|error| error.contains("no-such-workspace")));

        manager.shutdown_all().await;
    }

    /// The scope table, at the resolver rather than at each route.
    ///
    /// One object addresses every consumer request, so the cases a caller can
    /// put to it are worth stating once, here, rather than being discovered
    /// per route.
    #[tokio::test]
    async fn an_unnamed_scope_answers_from_the_active_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let active = manager.state().await.unwrap().active_workspace_id;

        let index = manager
            .consumer_index(&ConsumerScope::default())
            .await
            .unwrap();

        assert_eq!(index.context().data_dir, manager.active().data_dir);
        assert_eq!(manager.active_workspace_id().unwrap(), active);
        // No index has been built, so there is no space to report — and a
        // route that is about to name passages says why rather than
        // returning an empty answer that reads like a complete one.
        assert_eq!(index.embedding_space_id(), None);
        assert!(index.addressable_space_id().is_err());

        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn a_pin_no_index_can_prove_is_refused_rather_than_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();

        let error = manager
            .consumer_index(&ConsumerScope {
                workspace_id: None,
                expected_embedding_space_id: Some("space-the-caller-imagined".to_string()),
            })
            .await
            .unwrap_err();

        assert_eq!(
            error.code(),
            Some(ConsumerErrorCode::EmbeddingSpaceMismatch)
        );
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn an_unknown_id_names_no_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();

        let error = manager
            .consumer_index(&ConsumerScope {
                workspace_id: Some("no-such-workspace".to_string()),
                expected_embedding_space_id: None,
            })
            .await
            .unwrap_err();

        assert_eq!(
            error.code(),
            Some(ConsumerErrorCode::ManagedWorkspaceNotFound)
        );
        manager.shutdown_all().await;
    }

    /// A corpus with nothing in it has no coordinate system to disagree
    /// about; once it has one, a request that declines to name a space is
    /// asking to be served whatever happens to be there, which is the thing
    /// a pinned consumer surface exists to prevent.
    /// A canonical corpus answers unpinned, because it names no space to
    /// disagree about.
    ///
    /// It used to stop answering the moment it held an index: the index's own
    /// space became the corpus's, and every unpinned request was then refused
    /// with `EMBEDDING_SPACE_MISMATCH: corpus=…, request=none`. That was a
    /// coordinate system asserting itself over questions that never touch
    /// coordinates — listing a library's files, resolving a passage's text —
    /// and it is gone with the vectors. A projection still requires its pin,
    /// because a projection is nothing but a space.
    #[tokio::test]
    async fn a_canonical_corpus_answers_unpinned_because_it_names_no_space() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let embedding = SelectedEmbedder::default();
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-scope".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let scope = ConsumerScope {
            workspace_id: Some(corpus.corpus_id.clone()),
            expected_embedding_space_id: None,
        };

        assert!(manager.consumer_index(&scope).await.is_ok());

        let root = workspace_root(dir.path(), &corpus.corpus_id);
        let index = SemanticIndex::create(
            &root,
            embedding.model.model_id(),
            embedding.dimension,
            embedding.engine,
            None,
        )
        .unwrap();
        drop(index);

        // Still answers, and answers with no space, which is the truth about
        // what it holds.
        let resolved = manager
            .consumer_index(&scope)
            .await
            .expect("an unpinned canonical request is answerable");
        // Debug is where this type says which space it turned out to be, and
        // for a canonical corpus the answer is none.
        assert!(format!("{resolved:?}").contains("Absent"), "{resolved:?}");

        manager.shutdown_all().await;
    }

    /// A projection is how a corpus holds a second space, not a second thing
    /// to address. Reachable directly, it would be a way to be handed vectors
    /// without having named the space they are in.
    #[tokio::test]
    async fn an_internal_projection_is_not_addressable_as_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, events).unwrap();
        let embedding = SelectedEmbedder::default();
        let corpus = manager
            .ensure_managed_workspace(EnsureManagedWorkspace {
                owner: "underdog".to_string(),
                corpus_key: "store-projection".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        let parent_semantic =
            read_manifest(&workspace_manifest_path(dir.path(), &corpus.corpus_id))
                .unwrap()
                .semantic
                .unwrap();
        let projection = manager
            .ensure_projection_workspace(
                &corpus.corpus_id,
                "underdog",
                "store-projection",
                &parent_semantic,
                &embedding,
            )
            .unwrap();

        let error = manager
            .consumer_index(&ConsumerScope {
                workspace_id: Some(projection),
                expected_embedding_space_id: None,
            })
            .await
            .unwrap_err();

        assert_eq!(
            error.code(),
            Some(ConsumerErrorCode::ManagedWorkspaceNotFound)
        );
        manager.shutdown_all().await;
    }
}
