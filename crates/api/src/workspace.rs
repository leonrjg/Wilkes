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

use crate::commands::settings::get_scoped_settings;
use crate::context::{AppContext, EventEmitter, ManagedCorpusBackup};
use crate::startup::{StartupAction, StartupBlocker};

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
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnsureManagedWorkspace {
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

/// Workspace's contribution to the generic application startup gate.
/// Future breaking features can contribute their own blockers alongside this
/// one without changing either the desktop shell or the frontend splash.
pub fn startup_blockers(
    app_data_dir: &Path,
    settings_path: &Path,
) -> anyhow::Result<Vec<StartupBlocker>> {
    if registry_path(app_data_dir).exists() {
        return Ok(Vec::new());
    }
    if !contains_legacy_library(app_data_dir) && !settings_contain_legacy_roots(settings_path)? {
        return Ok(Vec::new());
    }

    Ok(vec![StartupBlocker {
        id: "workspaces.alpha-library-migration".to_string(),
        feature: "Workspaces".to_string(),
        title: "Your library needs a one-time workspace migration".to_string(),
        message: "This alpha installation predates workspaces. Quit Wilkes, migrate the existing library into its Default workspace, then reopen the app. Your index and opened roots will be preserved."
            .to_string(),
        actions: vec![
            StartupAction {
                label: "Preview migration".to_string(),
                description: "From the Wilkes source directory, inspect the files that will move."
                    .to_string(),
                command: Some("python3 scripts/migrate_workspace.py --dry-run".to_string()),
            },
            StartupAction {
                label: "Run migration".to_string(),
                description: "After quitting Wilkes, run the one-off migration."
                    .to_string(),
                command: Some("python3 scripts/migrate_workspace.py".to_string()),
            },
        ],
    }])
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

/// Creates a brand-new registry and reports the id of the workspace it made
/// active. This is intentionally only for fresh installs and tests; existing
/// pre-workspace installations are migrated by the explicit one-off script
/// rather than by a second compatibility path in the app.
///
/// The id rather than a [`WorkspaceState`]: describing the registry is
/// [`read_workspace_state`]'s job, and it has to read every manifest and the
/// settings that merge over them to do it. Creating a registry needs none of
/// that, and callers here are starting a manager, not rendering a list.
pub fn initialize_workspace_registry(app_data_dir: &Path) -> anyhow::Result<String> {
    let path = registry_path(app_data_dir);
    if path.exists() {
        return Ok(load_registry(app_data_dir)?.active_workspace_id);
    }
    anyhow::ensure!(
        !contains_legacy_library(app_data_dir),
        "This installation contains a pre-workspace library. Run scripts/migrate_workspace.py before starting Wilkes."
    );
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
        let blockers = startup_blockers(&app_data_dir, &settings_path)?;
        if let Some(blocker) = blockers.first() {
            anyhow::bail!(blocker.message.clone());
        }
        let id = if registry_path(&app_data_dir).exists() {
            load_registry(&app_data_dir)?.active_workspace_id
        } else {
            initialize_workspace_registry(&app_data_dir)?
        };
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
                anyhow::ensure!(
                    !manifest.is_application_managed(),
                    "MANAGED_WORKSPACE_PROTECTED: managed workspaces cannot be renamed"
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

    /// Create or retrieve Underdog's protected corpus workspace. Configuration
    /// is immutable after the first successful ensure.
    pub async fn ensure_underdog_workspace(
        &self,
        request: EnsureManagedWorkspace,
    ) -> anyhow::Result<ManagedWorkspaceStatus> {
        let corpus_key = request.corpus_key.trim();
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
                    WorkspaceKind::ApplicationManaged { owner, purpose, corpus_key: key }
                        if owner == "underdog" && purpose == "semantic-corpus" && key == corpus_key
                ) {
                    existing = Some(manifest);
                    break;
                }
            }
            if let Some(manifest) = existing {
                let configured = manifest.semantic.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("MANAGED_WORKSPACE_CONFIGURATION_MISMATCH: semantic configuration is absent")
                })?;
                anyhow::ensure!(
                    configured.selected == request.embedding
                        && configured.chunk_size == request.chunk_size
                        && configured.chunk_overlap == request.chunk_overlap,
                    "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH: existing corpus uses a different embedding or extraction configuration"
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
                    name: "Underdog semantic corpus".to_string(),
                    kind: WorkspaceKind::ApplicationManaged {
                        owner: "underdog".to_string(),
                        purpose: "semantic-corpus".to_string(),
                        corpus_key: corpus_key.to_string(),
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
        self.underdog_workspace_status(&id).await
    }

    pub async fn underdog_workspace_status(
        &self,
        corpus_id: &str,
    ) -> anyhow::Result<ManagedWorkspaceStatus> {
        let manifest = read_manifest(&workspace_manifest_path(&self.app_data_dir, corpus_id))?;
        anyhow::ensure!(
            matches!(
                manifest.kind,
                WorkspaceKind::ApplicationManaged { ref owner, ref purpose, .. }
                    if owner == "underdog" && purpose == "semantic-corpus"
            ),
            "MANAGED_WORKSPACE_NOT_FOUND"
        );
        let semantic = manifest.semantic.ok_or_else(|| {
            anyhow::anyhow!(
                "MANAGED_WORKSPACE_CONFIGURATION_MISMATCH: semantic configuration is absent"
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
                    ))
                }) {
                Ok(opened) => Some(opened),
                Err(error) => {
                    // Absent before the first build, so this is not on its own an
                    // error. Logged because the same arm covers a corrupt or
                    // unreadable index, which the caller only sees as a corpus
                    // that reports no embedding space.
                    tracing::info!(
                        "underdog_workspace_status: no readable index at {}: {error:#}",
                        index_root.display()
                    );
                    None
                }
            };
        let stored_identity = opened.as_ref().map(|(identity, _, _)| identity.clone());
        let completeness = opened
            .as_ref()
            .map(|(_, counts, _)| *counts)
            .unwrap_or((0, 0, 0));
        let embedding_work = opened
            .as_ref()
            .map(|(_, _, totals)| *totals)
            .unwrap_or((0, 0));
        let ready = stored_identity.as_ref().is_some_and(|identity| {
            identity.engine == semantic.selected.engine
                && identity.model_id == semantic.selected.model.model_id()
                && identity.dimension == semantic.selected.dimension
        }) && completeness.1 == completeness.2;
        // A corpus with no index has no coordinate system yet. Reporting one
        // derived from the manifest would advertise an id that no index will
        // ever carry, so callers get nothing to echo back until vectors exist.
        let identity = stored_identity;
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
        Ok(ManagedWorkspaceStatus {
            corpus_id: corpus_id.to_string(),
            embedding_space_id: identity.as_ref().map(|identity| identity.id().0),
            embedding_space_identity: identity,
            extraction_recipe_id: ExtractionRecipe::new(
                semantic.chunk_size,
                semantic.chunk_overlap,
            )
            .id(),
            ready,
            indexed_documents: completeness.0,
            indexed_chunks: completeness.2,
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
        })
    }

    /// Restores a self-verifying backup from Wilkes's own managed-backup
    /// directory. The caller names only one directory leaf, never an arbitrary
    /// path. A pre-created corpus for the same Underdog store may be replaced
    /// only while it is empty; an established corpus is never overwritten.
    pub async fn restore_underdog_workspace(
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
        anyhow::ensure!(
            matches!(
                &manifest.kind,
                WorkspaceKind::ApplicationManaged { owner, purpose, corpus_key }
                    if owner == "underdog"
                        && purpose == "semantic-corpus"
                        && corpus_key == expected_corpus_key
            ),
            "backup belongs to a different managed owner or corpus key"
        );

        let _switch_guard = self.switch_lock.lock().await;
        let existing = {
            let _registry_guard = self.registry_lock.lock();
            let registry = load_registry(&self.app_data_dir)?;
            registry.workspace_ids.iter().find_map(|id| {
                let found = read_manifest(&workspace_manifest_path(&self.app_data_dir, id)).ok()?;
                matches!(
                    &found.kind,
                    WorkspaceKind::ApplicationManaged { owner, purpose, corpus_key }
                        if owner == "underdog"
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
                let status = self.underdog_workspace_status(existing_id).await?;
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
        self.underdog_workspace_status(expected_corpus_id).await
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

    #[tokio::test]
    async fn a_corpus_without_an_index_reports_no_embedding_space() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("global-settings.json");
        let events: Arc<dyn EventEmitter> = Arc::new(NoopEmitter);
        let (manager, _events, _worker_loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings, Arc::clone(&events)).unwrap();
        let embedding = SelectedEmbedder::default();
        let status = manager
            .ensure_underdog_workspace(EnsureManagedWorkspace {
                corpus_key: "store-empty".to_string(),
                embedding: embedding.clone(),
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap();
        // Configured, but holding no vectors: there is no coordinate system to
        // name yet, and the manifest cannot predict the one a build will make.
        assert_eq!(status.embedding_space_id, None);
        assert!(status.embedding_space_identity.is_none());
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

        // Once an index exists the corpus reports that index's own id, and
        // keeps reporting it: the value never changes under the caller.
        let status = manager
            .underdog_workspace_status(&status.corpus_id)
            .await
            .unwrap();
        assert_eq!(status.embedding_space_id, Some(space));
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
            .ensure_underdog_workspace(EnsureManagedWorkspace {
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
            .ensure_underdog_workspace(EnsureManagedWorkspace {
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
            .restore_underdog_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
            .await
            .unwrap_err();
        assert!(inventory_error
            .to_string()
            .contains("contents do not match the manifest inventory"));
        std::fs::remove_file(target_backup.join("unlisted.txt")).unwrap();

        let restored = target_manager
            .restore_underdog_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
            .await
            .unwrap();
        assert_eq!(restored.corpus_id, source_status.corpus_id);
        assert_eq!(restored.embedding_space_id, Some(space.clone()));
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
            .restore_underdog_workspace("inbox", &source_status.corpus_id, &space, "store-restore")
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
        let id = initialize_workspace_registry(dir.path()).unwrap();
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
            corpus_key: "store-1".to_string(),
            embedding: SelectedEmbedder::default(),
            chunk_size: 600,
            chunk_overlap: 128,
        };
        let first = manager
            .ensure_underdog_workspace(request.clone())
            .await
            .unwrap();
        let second = manager.ensure_underdog_workspace(request).await.unwrap();
        assert_eq!(first.corpus_id, second.corpus_id);
        let state = manager.state().await.unwrap();
        assert_eq!(state.workspaces.len(), 2, "the corpus is listed beside the user's own workspace");
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
        assert!(manager
            .active()
            .ensure_writable()
            .unwrap_err()
            .contains("MANAGED_WORKSPACE_PROTECTED"));

        let mut mismatch = SelectedEmbedder::default();
        mismatch.dimension += 1;
        assert!(manager
            .ensure_underdog_workspace(EnsureManagedWorkspace {
                corpus_key: "store-1".to_string(),
                embedding: mismatch,
                chunk_size: 600,
                chunk_overlap: 128,
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("MANAGED_WORKSPACE_CONFIGURATION_MISMATCH"));
        manager.shutdown_all().await;
    }

    #[test]
    fn legacy_library_requires_the_explicit_migration() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("research.db"), b"legacy").unwrap();
        let error = initialize_workspace_registry(dir.path()).unwrap_err();
        assert!(error.to_string().contains("migrate_workspace.py"));
        assert!(!dir.path().join("workspaces.json").exists());
    }

    #[test]
    fn legacy_roots_produce_a_structured_startup_blocker() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "favorites": [dir.path().join("library")]
            }))
            .unwrap(),
        )
        .unwrap();

        let blockers = startup_blockers(dir.path(), &settings_path).unwrap();

        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].id, "workspaces.alpha-library-migration");
        assert_eq!(blockers[0].feature, "Workspaces");
        assert!(blockers[0].actions.iter().any(
            |action| action.command.as_deref() == Some("python3 scripts/migrate_workspace.py")
        ));
        assert!(!dir.path().join("workspaces.json").exists());
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
}
