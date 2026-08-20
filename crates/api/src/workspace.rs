use parking_lot::{Mutex as PLMutex, RwLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::{mpsc, Mutex};
use wilkes_core::types::{SelectedEmbedder, SemanticSettings};
use wilkes_core::worker::manager::{ManagerEvent, WorkerPaths};

use crate::commands::settings::get_scoped_settings;
use crate::context::{AppContext, EventEmitter};
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceState {
    pub active_workspace_id: String,
    pub workspaces: Vec<WorkspaceSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceManifest {
    #[serde(default = "manifest_version")]
    version: u32,
    pub id: String,
    pub name: String,
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
            favorites: Vec::new(),
            recent_roots: Vec::new(),
            active_root: None,
            semantic: None,
        }
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
        WorkspaceSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            roots,
            active_root: self.active_root.clone(),
            embedding,
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
    pub async fn context_for(self: &Arc<Self>, id: &str) -> anyhow::Result<Arc<AppContext>> {
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
}
