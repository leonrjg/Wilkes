use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info};
use wilkes_agent::session::{ChatConfigOption, ChatEvent, ChatSession};
use wilkes_api::commands::chat::{
    BackendStatus, ChatActiveDocRecord, ChatContextFileRecord, ChatConversationRecord,
    ChatMessageRecord, ChatReplayContentBlock, ChatReplayToolCall, ChatTurnEnvironmentRecord,
};
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_api::startup::StartupStatus;
use wilkes_api::workspace::{WorkspaceManager, WorkspaceState, WorkspaceSummary};
use wilkes_core::types::{
    AddOutcome, AgentBackend, Bookmark, BookmarkClustersQuery, BookmarkClustersResult,
    ChunkTopicsQuery, ChunkTopicsResult, CitationResult, CollectionValidation, DataPaths,
    DocumentMetadata, DocumentTagUpdate, EmbedderCapabilityManifest, EmbeddingEngine,
    ExternalMcpSettings, FileListResponse, HttpApiSettings, IndexStatus, IntegrationStatus,
    NewBookmark, NewSmartCollection, NewTag, OpenAlexWork, SearchLogEntry, SelectedEmbedder,
    SemanticScholarPaper, Settings, SmartCollection, Tag, UpdateSmartCollection, UpdateTag,
};
use wilkes_core::worker::manager::WorkerStatus;

mod platform;

use platform::{
    build_startup_plan, validate_open_target, validate_reveal_target, DesktopPlatform,
    DesktopStartupPlan, SystemDesktopPlatform, TauriPlatform,
};

#[derive(Default)]
struct DesktopStartupState(std::sync::RwLock<StartupStatus>);

impl DesktopStartupState {
    fn status(&self) -> StartupStatus {
        self.0.read().unwrap().clone()
    }

    fn replace(&self, status: StartupStatus) {
        *self.0.write().unwrap() = status;
    }
}

#[tauri::command]
fn get_startup_status(app: AppHandle) -> StartupStatus {
    app.state::<Arc<DesktopStartupState>>().status()
}

/// The single registration point for feature preflights. A future breaking
/// feature adds its provider here and the shell/UI need no corresponding
/// special case.
///
/// No feature contributes a blocker today: the pre-workspace library migration
/// that used to stop startup is performed by the application itself. The gate
/// stays because the shape of the question — is there anything only the user
/// can resolve before this build may open their data — outlives the one
/// feature that first had to ask it, and an unexpected startup failure is
/// still reported through it.
fn collect_startup_status(
    _data_dir: &std::path::Path,
    _settings_path: &std::path::Path,
) -> anyhow::Result<StartupStatus> {
    Ok(StartupStatus::ready())
}

fn app_context(app: &AppHandle) -> Arc<AppContext> {
    if let Some(manager) = app.try_state::<Arc<WorkspaceManager>>() {
        manager.inner().active()
    } else {
        app.state::<Arc<AppContext>>().inner().clone()
    }
}

fn workspace_manager(app: &AppHandle) -> Arc<WorkspaceManager> {
    app.state::<Arc<WorkspaceManager>>().inner().clone()
}

fn active_searches_state(app: &AppHandle) -> Arc<ActiveSearches> {
    app.state::<Arc<ActiveSearches>>().inner().clone()
}

fn data_paths_from(app_data: String, workspace: String) -> DataPaths {
    DataPaths {
        app_data,
        workspace,
    }
}

async fn list_files_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    collection_id: Option<String>,
    tag_ids: Vec<String>,
    collection_expression: Option<String>,
) -> Result<FileListResponse, String> {
    ctx.list_files_filtered(
        root.into(),
        collection_id.as_deref(),
        &tag_ids,
        collection_expression.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

async fn open_file_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<wilkes_core::types::PreviewData, String> {
    ctx.open_file(path.into()).await.map_err(|e| e.to_string())
}

async fn rename_file_for_path(
    ctx: Arc<AppContext>,
    path: String,
    new_name: String,
) -> Result<String, String> {
    ctx.rename_file(path.into(), new_name)
        .await
        .map(|path| path.display().to_string())
        .map_err(|e| e.to_string())
}

/// On macOS the `trash` crate defaults to driving Finder over AppleScript, which fails unless the
/// user grants the app Apple-events automation permission. `NSFileManager::trashItemAtURL` performs
/// the same move into the volume's Trash without that permission.
#[cfg(target_os = "macos")]
fn move_to_trash(path: &std::path::Path) -> Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};

    let mut context = trash::TrashContext::default();
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path)
}

#[cfg(not(target_os = "macos"))]
fn move_to_trash(path: &std::path::Path) -> Result<(), trash::Error> {
    trash::delete(path)
}

async fn trash_file_for_path(path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("Cannot trash {}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!("Cannot trash non-file path: {}", path.display()));
        }
        move_to_trash(&path)
            .map_err(|error| format!("Failed to move {} to Trash: {error}", path.display()))
    })
    .await
    .map_err(|error| format!("Trash operation failed: {error}"))?
}

async fn import_files_into_current_root_for_ctx(
    ctx: Arc<AppContext>,
    paths: Vec<String>,
    root: String,
    mode: wilkes_api::commands::files::FileImportMode,
) -> Result<Vec<String>, String> {
    let paths = paths.into_iter().map(PathBuf::from).collect();
    ctx.import_files_into_current_root(paths, root.into(), mode)
        .await
        .map(|paths| {
            paths
                .into_iter()
                .map(|path| path.display().to_string())
                .collect()
        })
        .map_err(|e| e.to_string())
}

async fn move_file_to_root_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
    target_root: String,
) -> Result<String, String> {
    ctx.ensure_writable().map_err(|error| error.to_string())?;
    let supported_extensions = ctx.get_settings().await.supported_extensions;
    let old = PathBuf::from(path);
    let mut moved = wilkes_api::commands::files::move_files_into_root(
        vec![old.clone()],
        target_root.into(),
        supported_extensions,
    )
    .await
    .map_err(|e| e.to_string())?;
    let new = moved.pop().unwrap_or_default();
    ctx.rekey_research_path(&old, &new)
        .map_err(|e| e.to_string())?;
    ctx.rekey_index_path(&old, &new)
        .map_err(|e| e.to_string())?;
    Ok(new.display().to_string())
}

async fn get_file_metadata_for_path(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    ctx.get_file_metadata(path.into())
        .await
        .map_err(|e| e.to_string())
}

async fn get_settings_for_ctx(ctx: Arc<AppContext>) -> Result<Settings, String> {
    Ok(ctx.get_settings().await)
}

async fn update_settings_for_ctx(
    ctx: Arc<AppContext>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    ctx.update_settings(patch).await.map_err(|e| e.to_string())
}

async fn list_bookmarks_for_ctx(ctx: Arc<AppContext>) -> Result<Vec<Bookmark>, String> {
    ctx.list_bookmarks().await.map_err(|e| e.to_string())
}

async fn add_bookmark_for_ctx(
    ctx: Arc<AppContext>,
    bookmark: NewBookmark,
) -> Result<Bookmark, String> {
    ctx.add_bookmark(bookmark).await.map_err(|e| e.to_string())
}

async fn remove_bookmark_for_ctx(ctx: Arc<AppContext>, id: String) -> Result<(), String> {
    ctx.remove_bookmark(&id).await.map_err(|e| e.to_string())
}

async fn update_bookmark_note_for_ctx(
    ctx: Arc<AppContext>,
    id: String,
    note: Option<String>,
) -> Result<Bookmark, String> {
    ctx.update_bookmark_note(&id, note)
        .await
        .map_err(|e| e.to_string())
}

async fn cluster_bookmarks_for_ctx(
    ctx: Arc<AppContext>,
    query: BookmarkClustersQuery,
) -> Result<BookmarkClustersResult, String> {
    ctx.cluster_bookmarks(query).await
}

async fn chunk_topics_for_ctx(
    ctx: Arc<AppContext>,
    request_id: String,
    query: ChunkTopicsQuery,
) -> Result<ChunkTopicsResult, String> {
    ctx.chunk_topics(request_id, query).await
}

fn cancel_chunk_topics_for_ctx(ctx: Arc<AppContext>, request_id: &str) {
    ctx.cancel_chunk_topics(request_id);
}

async fn zotero_status_for_ctx(ctx: Arc<AppContext>) -> Result<IntegrationStatus, String> {
    ctx.zotero_status().await.map_err(|e| e.to_string())
}

async fn semantic_scholar_status_for_ctx(
    ctx: Arc<AppContext>,
) -> Result<IntegrationStatus, String> {
    ctx.semantic_scholar_status()
        .await
        .map_err(|e| e.to_string())
}

async fn semantic_scholar_lookup_for_ctx(
    ctx: Arc<AppContext>,
    doi: String,
) -> Result<SemanticScholarPaper, String> {
    ctx.semantic_scholar_lookup(doi)
        .await
        .map_err(|e| e.to_string())
}

async fn openalex_status_for_ctx(ctx: Arc<AppContext>) -> Result<IntegrationStatus, String> {
    ctx.openalex_status().await.map_err(|e| e.to_string())
}

async fn openalex_lookup_for_ctx(
    ctx: Arc<AppContext>,
    doi: String,
) -> Result<OpenAlexWork, String> {
    ctx.openalex_lookup(doi).await.map_err(|e| e.to_string())
}

async fn resolve_file_metadata_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    ctx.resolve_file_metadata(path.into())
        .await
        .map_err(|e| e.to_string())
}

async fn refresh_file_metadata_for_ctx(
    ctx: Arc<AppContext>,
    path: Option<String>,
) -> Result<(), String> {
    ctx.refresh_file_metadata(path.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

async fn zotero_add_item_for_ctx(ctx: Arc<AppContext>, path: String) -> Result<AddOutcome, String> {
    ctx.zotero_add_item(path.into())
        .await
        .map_err(|e| e.to_string())
}

async fn zotero_generate_citation_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<CitationResult, String> {
    ctx.zotero_generate_citation(path.into())
        .await
        .map_err(|e| e.to_string())
}

fn is_semantic_ready_for_ctx(ctx: Arc<AppContext>) -> bool {
    ctx.is_semantic_ready()
}

async fn download_model_for_ctx(
    ctx: Arc<AppContext>,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::download_model_for_ctx: engine={}, model={}",
        selected.engine.as_str(),
        selected.model.model_id()
    );
    ctx.start_download_model(selected).await
}

async fn build_index_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
    selected: SelectedEmbedder,
) -> Result<(), String> {
    info!(
        "desktop::build_index_for_ctx: root={}, engine={}, model={}",
        root,
        selected.engine.as_str(),
        selected.model.model_id()
    );
    Arc::clone(&ctx).start_build_index(root, selected).await
}

/// What this build can embed with, as one answer.
///
/// The picker used to assemble this from two calls — the engine list, then a
/// model list per engine — which meant the UI decided what a model *was* by
/// joining two replies. The manifest is that join, made once and on the side
/// that knows.
async fn embedder_capabilities_for_ctx(
    ctx: Arc<AppContext>,
) -> Result<EmbedderCapabilityManifest, String> {
    let settings = ctx.get_settings().await;
    Ok(wilkes_core::embed::dispatch::model_capabilities(
        &ctx.model_dir,
        &settings.semantic.custom_models,
    ))
}

async fn cancel_embed_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    info!("desktop::cancel_embed_for_ctx");
    ctx.cancel_embed().await;
    Ok(())
}

async fn get_index_status_for_ctx(
    ctx: Arc<AppContext>,
    root: Option<String>,
) -> Result<IndexStatus, String> {
    ctx.get_index_status(root.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

async fn delete_index_for_ctx(ctx: Arc<AppContext>, root: Option<String>) -> Result<(), String> {
    ctx.delete_index(root.map(Into::into))
        .await
        .map_err(|e| e.to_string())
}

fn get_worker_status_for_ctx(ctx: Arc<AppContext>) -> WorkerStatus {
    ctx.get_worker_status()
}

/// Every worker, one row per role. Two processes can die independently, so a
/// single status would misreport a dead generation worker as healthy.
fn get_worker_statuses_for_ctx(ctx: Arc<AppContext>) -> Vec<WorkerStatus> {
    ctx.get_worker_statuses()
}

async fn kill_worker_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    ctx.kill_worker();
    Ok(())
}

async fn set_worker_timeout_for_ctx(ctx: Arc<AppContext>, secs: u64) -> Result<(), String> {
    ctx.set_worker_timeout(secs)
        .await
        .map_err(|e| e.to_string())
}

// ── External MCP lifecycle ────────────────────────────────────────────────────

const EXTERNAL_MCP_TOKEN_FILENAME: &str = "external-mcp-token";

struct ManagedExternalMcp {
    bind_address: std::net::IpAddr,
    port: u16,
    runtime: wilkes_agent::mcp::ExternalMcpRuntime,
}

struct ExternalMcpManager {
    token_path: PathBuf,
    context: wilkes_agent::mcp::ExternalMcpContext,
    runtime: tokio::sync::Mutex<Option<ManagedExternalMcp>>,
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
struct ExternalMcpStatus {
    enabled: bool,
    running: bool,
    require_token: bool,
    bind_address: std::net::IpAddr,
    port: u16,
    url: Option<String>,
    token: Option<String>,
    error: Option<String>,
}

impl ExternalMcpManager {
    fn new(data_dir: PathBuf) -> Self {
        Self {
            token_path: data_dir.join(EXTERNAL_MCP_TOKEN_FILENAME),
            context: wilkes_agent::mcp::ExternalMcpContext::default(),
            runtime: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    fn set_active_document(&self, path: Option<String>, page: Option<u32>) {
        self.context.set_active_document(path, page);
    }

    /// The listener is given the workspace manager rather than one workspace's
    /// context, so it answers for whichever workspace is active — and for any
    /// workspace a call names — without being restarted.
    async fn apply(
        &self,
        settings: &ExternalMcpSettings,
        workspaces: Arc<WorkspaceManager>,
    ) -> Result<(), String> {
        if !settings.enabled {
            let managed = self.runtime.lock().await.take();
            if let Some(managed) = managed {
                managed.runtime.shutdown().await;
            }
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }
        if settings.port == 0 {
            let message = "External MCP port must be between 1 and 65535".to_string();
            *self.last_error.lock().unwrap() = Some(message.clone());
            return Err(message);
        }

        let token = if settings.require_token {
            match load_or_create_external_mcp_token(&self.token_path) {
                Ok(token) => Some(token),
                Err(error) => {
                    let message = format!("Could not load the external MCP credential: {error:#}");
                    *self.last_error.lock().unwrap() = Some(message.clone());
                    return Err(message);
                }
            }
        } else {
            None
        };

        let mut runtime = self.runtime.lock().await;
        if let Some(managed) = runtime.as_ref().filter(|managed| {
            managed.bind_address == settings.bind_address && managed.port == settings.port
        }) {
            managed.runtime.set_token(token);
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        let workspaces: Arc<dyn wilkes_agent::search::WorkspaceCatalog> = workspaces;
        let start = || {
            wilkes_agent::mcp::start_external(
                settings.bind_address,
                settings.port,
                token.clone(),
                Arc::clone(&workspaces),
                self.context.clone(),
            )
        };
        let mut next = start().await;
        let previous = if next.is_err()
            && runtime
                .as_ref()
                .is_some_and(|managed| managed.port == settings.port)
        {
            let previous = runtime.take().expect("checked above");
            let previous_address = previous.bind_address;
            let previous_port = previous.port;
            let previous_token = previous.runtime.token();
            previous.runtime.shutdown().await;
            next = start().await;
            Some((previous_address, previous_port, previous_token))
        } else {
            None
        };

        match next {
            Ok(next) => {
                *runtime = Some(ManagedExternalMcp {
                    bind_address: settings.bind_address,
                    port: settings.port,
                    runtime: next,
                });
                *self.last_error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                if let Some((previous_address, previous_port, previous_token)) = previous {
                    match wilkes_agent::mcp::start_external(
                        previous_address,
                        previous_port,
                        previous_token,
                        workspaces,
                        self.context.clone(),
                    )
                    .await
                    {
                        Ok(previous_runtime) => {
                            *runtime = Some(ManagedExternalMcp {
                                bind_address: previous_address,
                                port: previous_port,
                                runtime: previous_runtime,
                            });
                        }
                        Err(restore_error) => {
                            let message = format!(
                                "Could not start Wilkes MCP on {}:{}: {error:#}; the previous listener could not be restored: {restore_error:#}",
                                settings.bind_address, settings.port
                            );
                            *self.last_error.lock().unwrap() = Some(message.clone());
                            return Err(message);
                        }
                    }
                }
                let message = format!(
                    "Could not start Wilkes MCP on {}:{}: {error:#}",
                    settings.bind_address, settings.port
                );
                *self.last_error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    async fn status(&self, settings: &ExternalMcpSettings) -> ExternalMcpStatus {
        let runtime = self.runtime.lock().await;
        let token = runtime
            .as_ref()
            .filter(|_| settings.require_token)
            .and_then(|managed| managed.runtime.token());
        ExternalMcpStatus {
            enabled: settings.enabled,
            running: runtime.is_some(),
            require_token: settings.require_token,
            bind_address: settings.bind_address,
            port: settings.port,
            url: runtime
                .as_ref()
                .map(|managed| managed.runtime.url().to_string()),
            token,
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    async fn rotate_token(&self, settings: &ExternalMcpSettings) -> Result<(), String> {
        if !settings.require_token {
            return Err("External MCP bearer authentication is not enabled".to_string());
        }
        let token = generate_external_mcp_token();
        write_external_mcp_token(&self.token_path, &token).map_err(|error| error.to_string())?;
        if let Some(runtime) = self.runtime.lock().await.as_ref() {
            runtime.runtime.set_token(Some(token));
        }
        Ok(())
    }

    async fn stop(&self) {
        let managed = self.runtime.lock().await.take();
        if let Some(managed) = managed {
            managed.runtime.shutdown().await;
        }
    }
}

fn external_mcp_manager_state(app: &AppHandle) -> Arc<ExternalMcpManager> {
    app.state::<Arc<ExternalMcpManager>>().inner().clone()
}

// ── HTTP API lifecycle ────────────────────────────────────────────────────────

struct ManagedHttpApi {
    bind_address: std::net::IpAddr,
    port: u16,
    runtime: wilkes_server::ApiRuntime,
}

/// Starts and stops the Wilkes HTTP API over the workspace this app already
/// owns.
///
/// The point is not convenience. A workspace has exactly one owner, and a
/// second process that opens the same directory races this one for
/// `settings.json` and the semantic index. Serving the API from inside the
/// owner is what lets another program read the library without becoming that
/// second owner.
struct HttpApiManager {
    /// Built once and reused across restarts: it resolves the active workspace
    /// per request, so toggling the listener must not re-derive which
    /// workspace is current.
    state: Arc<wilkes_server::http::state::AppState>,
    runtime: tokio::sync::Mutex<Option<ManagedHttpApi>>,
    last_error: Mutex<Option<String>>,
}

#[derive(Clone, Debug, Serialize)]
struct HttpApiStatus {
    enabled: bool,
    running: bool,
    bind_address: std::net::IpAddr,
    port: u16,
    url: Option<String>,
    error: Option<String>,
}

impl HttpApiManager {
    fn new(state: Arc<wilkes_server::http::state::AppState>) -> Self {
        Self {
            state,
            runtime: tokio::sync::Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    /// Brings the listener into the state `settings` describes.
    ///
    /// A failed start leaves nothing listening and says why. It deliberately
    /// does not resurrect the previous listener: the settings would then
    /// describe a port that is not the one in use, and a silently different
    /// answer to "where is the API" is worse than none.
    async fn apply(&self, settings: &HttpApiSettings) -> Result<(), String> {
        let mut runtime = self.runtime.lock().await;

        if !settings.enabled {
            if let Some(managed) = runtime.take() {
                managed.runtime.shutdown().await;
            }
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        if runtime.as_ref().is_some_and(|managed| {
            managed.bind_address == settings.bind_address && managed.port == settings.port
        }) {
            *self.last_error.lock().unwrap() = None;
            return Ok(());
        }

        // The old listener goes down first and is awaited, so the new one
        // cannot lose a race with it for the same port.
        if let Some(managed) = runtime.take() {
            managed.runtime.shutdown().await;
        }

        // The standalone server creates this before it serves, and the upload
        // routes assume it. Without it the same request would succeed against
        // one shell and fail against the other.
        let uploads_dir = self.state.context().data_dir.join("uploads");
        if let Err(error) = tokio::fs::create_dir_all(&uploads_dir).await {
            // Not fatal: it fails only the upload routes, which the desktop
            // does not use itself, and refusing to serve the rest over it
            // would be the larger breakage.
            error!(
                "could not create {} for the HTTP API: {error:#}",
                uploads_dir.display()
            );
        }

        match wilkes_server::start_api(
            settings.bind_address,
            settings.port,
            Arc::clone(&self.state),
        )
        .await
        {
            Ok(started) => {
                info!("Wilkes HTTP API listening on {}", started.url());
                *runtime = Some(ManagedHttpApi {
                    bind_address: settings.bind_address,
                    port: settings.port,
                    runtime: started,
                });
                *self.last_error.lock().unwrap() = None;
                Ok(())
            }
            Err(error) => {
                let message = format!(
                    "Could not start the Wilkes HTTP API on {}:{}: {error:#}",
                    settings.bind_address, settings.port
                );
                error!("{message}");
                *self.last_error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    async fn status(&self, settings: &HttpApiSettings) -> HttpApiStatus {
        let runtime = self.runtime.lock().await;
        HttpApiStatus {
            enabled: settings.enabled,
            running: runtime.is_some(),
            bind_address: settings.bind_address,
            port: settings.port,
            url: runtime.as_ref().map(|managed| managed.runtime.url()),
            error: self.last_error.lock().unwrap().clone(),
        }
    }

    async fn stop(&self) {
        let managed = self.runtime.lock().await.take();
        if let Some(managed) = managed {
            managed.runtime.shutdown().await;
        }
    }
}

fn http_api_manager_state(app: &AppHandle) -> Arc<HttpApiManager> {
    app.state::<Arc<HttpApiManager>>().inner().clone()
}

fn generate_external_mcp_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
}

fn read_external_mcp_token(path: &std::path::Path) -> anyhow::Result<String> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut token = String::new();
    file.read_to_string(&mut token)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("External MCP token file is empty: {}", path.display());
    }
    Ok(token)
}

fn load_or_create_external_mcp_token(path: &std::path::Path) -> anyhow::Result<String> {
    match read_external_mcp_token(path) {
        Ok(token) => return Ok(token),
        Err(error) if path.exists() => return Err(error),
        Err(_) => {}
    }
    let token = generate_external_mcp_token();
    write_external_mcp_token(path, &token)?;
    Ok(token)
}

fn write_external_mcp_token(path: &std::path::Path, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Applies a settings patch, keeping the two network listeners in step with it.
///
/// Both are settings whose effect is a bound port rather than a stored value,
/// so the listener is moved *before* the write and moved back if the write
/// fails — a saved setting that describes a listener which never started is
/// the state this ordering exists to prevent.
async fn update_settings_with_listeners(
    ctx: Arc<AppContext>,
    workspaces: Arc<WorkspaceManager>,
    external_mcp: Arc<ExternalMcpManager>,
    http_api: Arc<HttpApiManager>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    let before = ctx.get_settings().await;
    let requested_external = patch
        .get("external_mcp")
        .cloned()
        .map(serde_json::from_value::<ExternalMcpSettings>)
        .transpose()
        .map_err(|error| format!("Invalid external MCP settings: {error}"))?;
    let requested_http = patch
        .get("http_api")
        .cloned()
        .map(serde_json::from_value::<HttpApiSettings>)
        .transpose()
        .map_err(|error| format!("Invalid HTTP API settings: {error}"))?;

    if requested_external.is_none() && requested_http.is_none() {
        return update_settings_for_ctx(ctx, patch).await;
    }

    if let Some(requested) = &requested_external {
        external_mcp
            .apply(requested, Arc::clone(&workspaces))
            .await?;
    }
    if let Some(requested) = &requested_http {
        http_api.apply(requested).await?;
    }

    match ctx.update_settings(patch).await {
        Ok(updated) => Ok(updated),
        Err(error) => {
            if requested_external.is_some() {
                let _ = external_mcp
                    .apply(&before.external_mcp, Arc::clone(&workspaces))
                    .await;
            }
            if requested_http.is_some() {
                let _ = http_api.apply(&before.http_api).await;
            }
            Err(error.to_string())
        }
    }
}

fn handle_exit_event(app_handle: &AppHandle, event: tauri::RunEvent) {
    if matches!(
        event,
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
    ) {
        let Some(workspaces) = app_handle.try_state::<Arc<WorkspaceManager>>() else {
            return;
        };
        let workspaces = workspaces.inner().clone();
        // Kill any in-flight turn and the chat subprocesses themselves before
        // the process tree goes away, rather than leaving orphaned CLIs behind.
        if let Some(chat_manager) = app_handle.try_state::<Arc<ChatManager>>() {
            chat_manager.close_all();
        }
        let external_mcp = app_handle
            .try_state::<Arc<ExternalMcpManager>>()
            .map(|manager| manager.inner().clone());
        // Stopped before the workspace shuts down: the API answers *from* this
        // workspace, so a listener outliving it would serve requests against a
        // context that is already going away.
        let http_api = app_handle
            .try_state::<Arc<HttpApiManager>>()
            .map(|manager| manager.inner().clone());
        tauri::async_runtime::spawn(async move {
            if let Some(external_mcp) = external_mcp {
                external_mcp.stop().await;
            }
            if let Some(http_api) = http_api {
                http_api.stop().await;
            }
            // Every workspace this manager opened, not only the active one:
            // the HTTP API can have opened contexts for others.
            workspaces.shutdown_all().await;
        });
    }
}

// ── Tauri EventEmitter impl ───────────────────────────────────────────────────

struct TauriEmitter(AppHandle);

impl EventEmitter for TauriEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        let platform = TauriPlatform(self.0.clone());
        platform.emit(name, payload);
    }
}

/// Workspace events, to the webview and to any HTTP subscriber.
///
/// `/api/events` streams from a broadcast channel, so serving the API without
/// this fan-out would give HTTP consumers a route that connects and then
/// reports nothing forever — an endpoint that exists on the standalone server
/// and is silently dead here. Chat keeps using [`TauriEmitter`] directly: its
/// events belong to one webview turn, not to the workspace.
struct WorkspaceEmitter {
    webview: TauriEmitter,
    http: broadcast::Sender<(String, serde_json::Value)>,
}

impl EventEmitter for WorkspaceEmitter {
    fn emit(&self, name: &str, payload: serde_json::Value) {
        // No subscribers is the normal case (the API is off), and `send`
        // reporting that is not a failure worth logging.
        let _ = self.http.send((name.to_string(), payload.clone()));
        self.webview.emit(name, payload);
    }
}

// ── Chat (ACP) state ─────────────────────────────────────────────────────────

/// Open chat sessions, keyed by a Wilkes-generated session id. Mirrors
/// `ActiveSearches`: session lifetime and event forwarding are owned here,
/// not by `wilkes_api`/`wilkes_agent`, which stay UI-framework-agnostic.
struct ManagedChatSession {
    session: Arc<ChatSession>,
    conversation_id: Mutex<Option<String>>,
    cwd: PathBuf,
    context_files: Mutex<Vec<ChatContextFileRecord>>,
    active_doc: Mutex<Option<ChatActiveDocRecord>>,
    messages: Mutex<Vec<ChatMessageRecord>>,
    branch_history_pending: Mutex<bool>,
}

struct ChatManager(Mutex<HashMap<String, Arc<ManagedChatSession>>>);

impl ChatManager {
    fn insert(&self, id: String, session: Arc<ManagedChatSession>) {
        self.0.lock().unwrap().insert(id, session);
    }

    fn get(&self, id: &str) -> Option<Arc<ManagedChatSession>> {
        self.0.lock().unwrap().get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<Arc<ManagedChatSession>> {
        self.0.lock().unwrap().remove(id)
    }

    fn close_all(&self) {
        for (_, managed) in self.0.lock().unwrap().drain() {
            managed.session.close();
        }
    }
}

fn chat_manager_state(app: &AppHandle) -> Arc<ChatManager> {
    app.state::<Arc<ChatManager>>().inner().clone()
}

fn chat_session_or_err(
    manager: &ChatManager,
    session_id: &str,
) -> Result<Arc<ChatSession>, String> {
    manager
        .get(session_id)
        .map(|managed| Arc::clone(&managed.session))
        .ok_or_else(|| format!("chat session not found: {session_id}"))
}

fn managed_chat_session_or_err(
    manager: &ChatManager,
    session_id: &str,
) -> Result<Arc<ManagedChatSession>, String> {
    manager
        .get(session_id)
        .ok_or_else(|| format!("chat session not found: {session_id}"))
}

fn record_chat_event(managed: &ManagedChatSession, event: &ChatEvent) {
    apply_chat_event(&mut managed.messages.lock().unwrap(), event);
}

fn apply_chat_event(messages: &mut Vec<ChatMessageRecord>, event: &ChatEvent) {
    let turn_id = match event {
        ChatEvent::TextDelta { turn_id, .. }
        | ChatEvent::ThoughtDelta { turn_id, .. }
        | ChatEvent::ToolCall { turn_id, .. } => turn_id,
        _ => return,
    };
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "assistant" && message.turn_id.as_deref() == Some(turn_id))
    else {
        return;
    };

    match event {
        ChatEvent::TextDelta { delta, .. } => {
            if let Some(ChatReplayContentBlock::Text { text }) = message.content.last_mut() {
                text.push_str(delta);
            } else {
                message.content.push(ChatReplayContentBlock::Text {
                    text: delta.clone(),
                });
            }
        }
        ChatEvent::ThoughtDelta { delta, .. } => message.thought.push_str(delta),
        ChatEvent::ToolCall {
            tool_call_id,
            title,
            status,
            locations,
            content,
            raw_input,
            raw_output,
            ..
        } => {
            let existing = message.content.iter_mut().find_map(|block| match block {
                ChatReplayContentBlock::Tool { tool } if tool.tool_call_id == *tool_call_id => {
                    Some(tool)
                }
                _ => None,
            });
            if let Some(tool) = existing {
                if let Some(title) = title {
                    tool.title = title.clone();
                }
                if let Some(status) = status {
                    tool.status = status.clone();
                }
                if let Some(locations) = locations {
                    tool.locations = locations.clone();
                }
                if let Some(content) = content {
                    tool.content = content.clone();
                }
                if raw_input.is_some() {
                    tool.raw_input = raw_input.clone();
                }
                if raw_output.is_some() {
                    tool.raw_output = raw_output.clone();
                }
            } else {
                message.content.push(ChatReplayContentBlock::Tool {
                    tool: ChatReplayToolCall {
                        tool_call_id: tool_call_id.clone(),
                        title: title.clone().unwrap_or_else(|| "Tool call".to_string()),
                        status: status.clone().unwrap_or_else(|| "pending".to_string()),
                        locations: locations.clone().unwrap_or_default(),
                        content: content.clone().unwrap_or_default(),
                        raw_input: raw_input.clone(),
                        raw_output: raw_output.clone(),
                    },
                });
            }
        }
        _ => {}
    }
}

/// Forward every `ChatEvent` for the life of a session -- not just one turn --
/// through `EventEmitter` as `chat/update-<turn_id>` (spec §7.8). Runs until
/// the subprocess's connection closes (session close, crash, or app exit).
fn spawn_chat_event_forwarder(
    app: AppHandle,
    session_id: String,
    managed: Arc<ManagedChatSession>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ChatEvent>,
) {
    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        while let Some(event) = events.recv().await {
            record_chat_event(&managed, &event);
            match event {
                ChatEvent::TextDelta { turn_id, delta } => {
                    emitter.emit(
                        &format!("chat/update-{turn_id}"),
                        serde_json::json!({ "kind": "text", "delta": delta }),
                    );
                }
                ChatEvent::ThoughtDelta { turn_id, delta } => {
                    emitter.emit(
                        &format!("chat/update-{turn_id}"),
                        serde_json::json!({ "kind": "thought", "delta": delta }),
                    );
                }
                ChatEvent::ToolCall {
                    turn_id,
                    tool_call_id,
                    title,
                    status,
                    locations,
                    content,
                    raw_input,
                    raw_output,
                } => {
                    let locations = locations.map(|locs| {
                        locs.into_iter()
                            .map(|l| serde_json::json!({ "path": l.path, "line": l.line }))
                            .collect::<Vec<_>>()
                    });
                    emitter.emit(
                        &format!("chat/update-{turn_id}"),
                        serde_json::json!({
                            "kind": "tool",
                            "tool_call_id": tool_call_id,
                            "title": title,
                            "status": status,
                            "locations": locations,
                            "content": content,
                            "raw_input": raw_input,
                            "raw_output": raw_output,
                        }),
                    );
                }
                ChatEvent::PermissionRequest {
                    turn_id,
                    request_id,
                    tool_call_id,
                    title,
                    options,
                } => {
                    emitter.emit(
                        &format!("chat/update-{turn_id}"),
                        serde_json::json!({
                            "kind": "permission",
                            "request_id": request_id,
                            "tool_call_id": tool_call_id,
                            "title": title,
                            "options": options,
                        }),
                    );
                }
                ChatEvent::SessionError { message } => {
                    error!("chat session {session_id} error: {message}");
                    emitter.emit(
                        &format!("chat/session-error-{session_id}"),
                        serde_json::json!({ "message": message }),
                    );
                }
                ChatEvent::ConfigOptionsUpdated { options } => {
                    emitter.emit(
                        &format!("chat/config-{session_id}"),
                        serde_json::json!(options),
                    );
                }
            }
        }
    });
}

// ── Desktop-specific state ────────────────────────────────────────────────────

struct ActiveSearches(Mutex<HashMap<String, JoinHandle<()>>>);

impl ActiveSearches {
    fn cancel_all(&self) {
        for (_, handle) in self.0.lock().unwrap().drain() {
            handle.abort();
        }
    }
}

trait SearchEventSink: Send + Sync + Clone + 'static {
    fn emit_event(&self, name: &str, payload: serde_json::Value);
}

impl SearchEventSink for AppHandle {
    fn emit_event(&self, name: &str, payload: serde_json::Value) {
        let _ = Emitter::emit(self, name, &payload);
    }
}

async fn search_for_ctx<E>(
    ctx: Arc<AppContext>,
    active_searches: Arc<ActiveSearches>,
    emitter: E,
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
) -> Result<String, String>
where
    E: SearchEventSink,
{
    let handle = Arc::clone(&ctx).start_search_as(query, "ui").await?;

    let search_id = search_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let id = search_id.clone();
    let emitter_for_task = emitter.clone();
    let active_searches_for_task = Arc::clone(&active_searches);

    let forwarder: JoinHandle<()> = tokio::spawn(async move {
        let stats = handle
            .run(|fm| {
                let emitter = emitter_for_task.clone();
                let id = id.clone();
                async move {
                    let payload = serde_json::to_value(&fm).unwrap_or_default();
                    emitter.emit_event(&format!("search-result-{}", id), payload);
                    true
                }
            })
            .await;

        emitter_for_task.emit_event(
            &format!("search-complete-{}", id),
            serde_json::to_value(&stats).unwrap_or_default(),
        );

        active_searches_for_task.0.lock().unwrap().remove(&id);
    });

    active_searches
        .0
        .lock()
        .unwrap()
        .insert(search_id.clone(), forwarder);

    Ok(search_id)
}

async fn get_model_size_for_ctx_with<F, Fut>(
    engine: EmbeddingEngine,
    model_id: String,
    fetcher: F,
) -> Result<u64, String>
where
    F: FnOnce(EmbeddingEngine, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<u64>>,
{
    fetcher(engine, model_id).await.map_err(|e| e.to_string())
}

fn cancel_search_for_ctx(active_searches: Arc<ActiveSearches>, search_id: &str) {
    if let Some(handle) = active_searches.0.lock().unwrap().remove(search_id) {
        handle.abort();
    }
}

// ── Desktop-only commands ─────────────────────────────────────────────────────

#[tauri::command]
async fn get_data_paths(app: AppHandle) -> Result<DataPaths, String> {
    let ctx = app_context(&app);
    Ok(data_paths_from(
        ctx.shared_data_dir.display().to_string(),
        ctx.data_dir.display().to_string(),
    ))
}

#[tauri::command]
async fn list_workspaces(app: AppHandle) -> Result<WorkspaceState, String> {
    workspace_manager(&app)
        .state()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn create_workspace(app: AppHandle, name: String) -> Result<WorkspaceSummary, String> {
    workspace_manager(&app)
        .create(name)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn rename_workspace(
    app: AppHandle,
    workspace_id: String,
    name: String,
) -> Result<WorkspaceSummary, String> {
    workspace_manager(&app)
        .rename(&workspace_id, name)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn switch_workspace(app: AppHandle, workspace_id: String) -> Result<WorkspaceState, String> {
    let manager = workspace_manager(&app);
    let current = manager.state().await.map_err(|error| error.to_string())?;
    if current.active_workspace_id == workspace_id {
        return Ok(current);
    }
    if !current
        .workspaces
        .iter()
        .any(|workspace| workspace.id == workspace_id)
    {
        return Err("Unknown workspace".to_string());
    }

    active_searches_state(&app).cancel_all();
    chat_manager_state(&app).close_all();

    // The external MCP listener is not stopped and restarted around the
    // switch: it resolves its workspace through the manager on every call, so
    // it follows the active workspace on its own. Restarting it here would
    // only drop connected clients.
    manager
        .switch(&workspace_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_python_info() -> Result<String, String> {
    wilkes_core::path::resolve_python()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    validate_open_target(&path)?;
    SystemDesktopPlatform.open_target(&path)?;
    Ok(())
}

#[tauri::command]
async fn reveal_path(path: String) -> Result<(), String> {
    validate_reveal_target(&path)?;
    SystemDesktopPlatform.reveal_target(&path)?;
    Ok(())
}

#[tauri::command]
async fn trash_file(path: String, app: AppHandle) -> Result<(), String> {
    // Path-based rather than routed through the context, so the read-only gate
    // has to be asked for here: the workspace it would delete from is the
    // active one either way.
    app_context(&app)
        .ensure_writable()
        .map_err(|error| error.to_string())?;
    trash_file_for_path(path).await
}

#[tauri::command]
async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| {
            let selected = p.to_string();
            std::fs::canonicalize(&selected)
                .unwrap_or_else(|_| selected.clone().into())
                .display()
                .to_string()
        }));
    });
    Ok(rx.await.unwrap_or(None))
}

// ── Search ────────────────────────────────────────────────────────────────────

#[tauri::command]
async fn search(
    query: wilkes_core::types::SearchQuery,
    search_id: Option<String>,
    app: AppHandle,
) -> Result<String, String> {
    let ctx = app_context(&app);
    let active_searches = active_searches_state(&app);
    search_for_ctx(ctx, active_searches, app.clone(), query, search_id).await
}

#[tauri::command]
async fn cancel_search(search_id: String, app: AppHandle) -> Result<(), String> {
    cancel_search_for_ctx(active_searches_state(&app), &search_id);
    Ok(())
}

#[tauri::command]
async fn related_documents(
    query: wilkes_core::types::RelatedDocumentsQuery,
    app: AppHandle,
) -> Result<Vec<wilkes_core::types::RelatedDocument>, String> {
    app_context(&app).related_documents(query).await
}

#[tauri::command]
async fn citation_links(
    query: wilkes_core::types::CitationLinksQuery,
    app: AppHandle,
) -> Result<wilkes_core::types::CitationLinks, String> {
    app_context(&app).citation_links(query).await
}

// ── Chat commands ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatStartResult {
    session_id: String,
    conversation_id: Option<String>,
    backend_session_id: Option<String>,
    /// Initial ACP session config options (model, mode, ...), if the agent
    /// supports `session/set_config_option`. Later changes -- ours or the
    /// agent's own -- arrive via `chat/config-<sessionId>`.
    config_options: Vec<ChatConfigOption>,
    messages: Vec<ChatMessageRecord>,
    context_files: Vec<ChatContextFileRecord>,
    active_doc: Option<ChatActiveDocRecord>,
}

#[derive(Debug, Serialize)]
struct ChatSendResult {
    conversation_id: Option<String>,
}

fn ensure_chat_conversation(
    ctx: &AppContext,
    managed: &ManagedChatSession,
) -> Result<Option<String>, String> {
    if !wilkes_api::commands::chat::is_durable_backend(managed.session.backend) {
        return Ok(None);
    }

    let config_options = managed.session.config_options();
    let context_files = managed.context_files.lock().unwrap();
    let active_doc = managed.active_doc.lock().unwrap();
    let mut conversation_id = managed.conversation_id.lock().unwrap();
    if conversation_id.is_some() {
        return Ok(conversation_id.clone());
    }

    let record = wilkes_api::commands::chat::create_conversation(
        &ctx.data_dir,
        managed.session.backend,
        &managed.cwd,
        managed.session.backend_session_id().to_string(),
        &config_options,
        context_files.clone(),
        active_doc.clone(),
    )
    .map_err(|e| e.to_string())?;
    *conversation_id = record.map(|record| record.conversation_id);
    Ok(conversation_id.clone())
}

#[tauri::command]
async fn chat_list_backends(refresh: bool, _app: AppHandle) -> Vec<BackendStatus> {
    wilkes_api::commands::chat::list_backends(refresh)
}

#[tauri::command]
async fn chat_install_backend(
    backend: AgentBackend,
    _app: AppHandle,
) -> Result<BackendStatus, String> {
    wilkes_api::commands::chat::install_backend(backend)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_list_conversations(app: AppHandle) -> Result<Vec<ChatConversationRecord>, String> {
    let ctx = app_context(&app);
    wilkes_api::commands::chat::list_conversations(&ctx.data_dir).map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_forget_conversation(conversation_id: String, app: AppHandle) -> Result<(), String> {
    let ctx = app_context(&app);
    wilkes_api::commands::chat::forget_conversation(&ctx.data_dir, &conversation_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn chat_start(
    backend: AgentBackend,
    search_root: Option<String>,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let integrations = ctx.get_settings().await.integrations;
    let spawned = wilkes_api::commands::chat::start(
        backend,
        cwd.clone(),
        Some(workspace_manager(&app)),
        integrations,
    )
    .await
    .map_err(|e| e.to_string())?;
    let wilkes_agent::session::SpawnedChatSession {
        session,
        events,
        replay_messages: _,
    } = spawned;
    let session = Arc::new(session);
    let current_root = match search_root {
        Some(root) => Some(root),
        None => ctx
            .get_settings()
            .await
            .last_directory
            .map(|path| path.to_string_lossy().into_owned()),
    };
    session.set_search_root(current_root);

    let session_id = uuid::Uuid::new_v4().to_string();
    // Restore the config (model, thought level, mode) last chosen for this
    // backend before the first-send snapshot, so the eventual conversation
    // record starts from the persisted default.
    let desired_config = ctx
        .get_settings()
        .await
        .chat_config
        .into_iter()
        .find(|entry| entry.backend == backend)
        .map(|entry| entry.values)
        .unwrap_or_default();
    let config_options = wilkes_api::commands::chat::apply_config(&session, &desired_config).await;
    let backend_session_id = session.backend_session_id().to_string();
    let managed = Arc::new(ManagedChatSession {
        session,
        conversation_id: Mutex::new(None),
        cwd,
        context_files: Mutex::new(Vec::new()),
        active_doc: Mutex::new(None),
        messages: Mutex::new(Vec::new()),
        branch_history_pending: Mutex::new(false),
    });
    spawn_chat_event_forwarder(
        app.clone(),
        session_id.clone(),
        Arc::clone(&managed),
        events,
    );
    chat_manager_state(&app).insert(session_id.clone(), managed);
    Ok(ChatStartResult {
        session_id,
        conversation_id: None,
        backend_session_id: Some(backend_session_id),
        config_options,
        messages: Vec::new(),
        context_files: Vec::new(),
        active_doc: None,
    })
}

#[tauri::command]
async fn chat_open_conversation(
    conversation_id: String,
    search_root: Option<String>,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let record = wilkes_api::commands::chat::get_conversation(&ctx.data_dir, &conversation_id)
        .map_err(|e| e.to_string())?;
    let integrations = ctx.get_settings().await.integrations;
    let spawned =
        wilkes_api::commands::chat::open(&record, Some(workspace_manager(&app)), integrations)
            .await
            .map_err(|e| e.to_string())?;
    let wilkes_agent::session::SpawnedChatSession {
        session,
        events,
        replay_messages: _,
    } = spawned;
    let session = Arc::new(session);
    session.set_search_root(search_root);
    for file in &record.context_files {
        session.add_context(file.path.clone(), file.pages);
    }
    if let Some(active_doc) = &record.active_doc {
        session.set_active_doc(Some(active_doc.path.clone()), active_doc.page);
    }
    if record.branch_history_pending {
        session.set_branch_history(Some(wilkes_api::commands::chat::branch_history_text(
            &record.messages,
        )));
    }
    wilkes_api::commands::chat::touch_conversation(&ctx.data_dir, &conversation_id, None)
        .map_err(|e| e.to_string())?;

    let session_id = uuid::Uuid::new_v4().to_string();
    // Restore the config this conversation was last using -- not the backend's
    // global default -- so reopening lands on the same model/mode as before.
    let config_options =
        wilkes_api::commands::chat::apply_config(&session, &record.config_values).await;
    let managed = Arc::new(ManagedChatSession {
        session,
        conversation_id: Mutex::new(Some(conversation_id.clone())),
        cwd: std::path::PathBuf::from(&record.cwd),
        context_files: Mutex::new(record.context_files.clone()),
        active_doc: Mutex::new(record.active_doc.clone()),
        messages: Mutex::new(record.messages.clone()),
        branch_history_pending: Mutex::new(record.branch_history_pending),
    });
    spawn_chat_event_forwarder(
        app.clone(),
        session_id.clone(),
        Arc::clone(&managed),
        events,
    );
    chat_manager_state(&app).insert(session_id.clone(), managed);
    Ok(ChatStartResult {
        session_id,
        conversation_id: Some(conversation_id),
        backend_session_id: Some(record.backend_session_id),
        config_options,
        messages: record.messages,
        context_files: record.context_files,
        active_doc: record.active_doc,
    })
}

#[tauri::command]
async fn chat_fork_conversation(
    conversation_id: String,
    message_id: String,
    include_message: bool,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let source = wilkes_api::commands::chat::get_conversation(&ctx.data_dir, &conversation_id)
        .map_err(|e| e.to_string())?;
    if !wilkes_api::commands::chat::is_durable_backend(source.backend) {
        return Err("This chat backend does not support durable forks".to_string());
    }
    let environment = wilkes_api::commands::chat::environment_at_message(&source, &message_id)
        .map_err(|e| e.to_string())?;

    let integrations = ctx.get_settings().await.integrations;
    let spawned = wilkes_api::commands::chat::start(
        source.backend,
        PathBuf::from(&source.cwd),
        Some(workspace_manager(&app)),
        integrations,
    )
    .await
    .map_err(|e| e.to_string())?;
    let wilkes_agent::session::SpawnedChatSession {
        session,
        events,
        replay_messages: _,
    } = spawned;
    let session = Arc::new(session);
    session.set_search_root(environment.search_root.clone());
    for file in &environment.context_files {
        session.add_context(file.path.clone(), file.pages);
    }
    if let Some(active_doc) = &environment.active_doc {
        session.set_active_doc(Some(active_doc.path.clone()), active_doc.page);
    }
    let config_options =
        wilkes_api::commands::chat::apply_config(&session, &environment.config_values).await;
    let backend_session_id = session.backend_session_id().to_string();
    let record = match wilkes_api::commands::chat::create_fork_conversation(
        &ctx.data_dir,
        &conversation_id,
        &message_id,
        include_message,
        backend_session_id.clone(),
    ) {
        Ok(record) => record,
        Err(error) => {
            session.close();
            return Err(error.to_string());
        }
    };
    if record.branch_history_pending {
        session.set_branch_history(Some(wilkes_api::commands::chat::branch_history_text(
            &record.messages,
        )));
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let managed = Arc::new(ManagedChatSession {
        session,
        conversation_id: Mutex::new(Some(record.conversation_id.clone())),
        cwd: PathBuf::from(&record.cwd),
        context_files: Mutex::new(record.context_files.clone()),
        active_doc: Mutex::new(record.active_doc.clone()),
        messages: Mutex::new(record.messages.clone()),
        branch_history_pending: Mutex::new(record.branch_history_pending),
    });
    spawn_chat_event_forwarder(
        app.clone(),
        session_id.clone(),
        Arc::clone(&managed),
        events,
    );
    chat_manager_state(&app).insert(session_id.clone(), managed);

    Ok(ChatStartResult {
        session_id,
        conversation_id: Some(record.conversation_id),
        backend_session_id: Some(backend_session_id),
        config_options,
        messages: record.messages,
        context_files: record.context_files,
        active_doc: record.active_doc,
    })
}

#[tauri::command]
async fn chat_set_config_option(
    session_id: String,
    config_id: String,
    value: String,
    app: AppHandle,
) -> Result<Vec<ChatConfigOption>, String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    let options = managed
        .session
        .set_config_option(config_id, value)
        .await
        .map_err(|e| e.to_string())?;
    let conversation_id = managed.conversation_id.lock().unwrap().clone();
    if let Some(conversation_id) = conversation_id {
        wilkes_api::commands::chat::update_conversation_config(
            &ctx.data_dir,
            &conversation_id,
            &options,
        )
        .map_err(|e| e.to_string())?;
    }
    // Remember this as the backend's default so the *next* new chat with it
    // starts from the same config, even for non-durable backends that keep no
    // conversation record.
    let backend = managed.session.backend;
    let values = wilkes_api::commands::chat::config_values_from_options(&options);
    let chat_config = wilkes_api::commands::chat::upsert_backend_config(
        ctx.get_settings().await.chat_config,
        backend,
        values,
    );
    ctx.update_settings(serde_json::json!({ "chat_config": chat_config }))
        .await
        .map_err(|e| e.to_string())?;
    Ok(options)
}

#[tauri::command]
async fn chat_add_context(
    session_id: String,
    path: String,
    pages: Option<u32>,
    app: AppHandle,
) -> Result<(), String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    managed.session.add_context(path.clone(), pages);
    let context_files = {
        let mut files = managed.context_files.lock().unwrap();
        if !files.iter().any(|file| file.path == path) {
            files.push(ChatContextFileRecord { path, pages });
        }
        files.clone()
    };
    let conversation_id = managed.conversation_id.lock().unwrap().clone();
    if let Some(conversation_id) = conversation_id {
        wilkes_api::commands::chat::update_conversation_context(
            &ctx.data_dir,
            &conversation_id,
            Some(context_files),
            None,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn chat_remove_context(
    session_id: String,
    path: String,
    app: AppHandle,
) -> Result<(), String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    managed.session.remove_context(&path);
    let context_files = {
        let mut files = managed.context_files.lock().unwrap();
        files.retain(|file| file.path != path);
        files.clone()
    };
    let conversation_id = managed.conversation_id.lock().unwrap().clone();
    if let Some(conversation_id) = conversation_id {
        wilkes_api::commands::chat::update_conversation_context(
            &ctx.data_dir,
            &conversation_id,
            Some(context_files),
            None,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn chat_set_active_doc(
    session_id: String,
    path: Option<String>,
    page: Option<u32>,
    app: AppHandle,
) -> Result<(), String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    managed.session.set_active_doc(path.clone(), page);
    let active_doc = path.map(|path| ChatActiveDocRecord { path, page });
    *managed.active_doc.lock().unwrap() = active_doc.clone();
    let conversation_id = managed.conversation_id.lock().unwrap().clone();
    if let Some(conversation_id) = conversation_id {
        wilkes_api::commands::chat::update_conversation_context(
            &ctx.data_dir,
            &conversation_id,
            None,
            Some(active_doc),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn chat_send(
    session_id: String,
    turn_id: String,
    user_message_id: String,
    text: String,
    search_root: Option<String>,
    app: AppHandle,
) -> Result<ChatSendResult, String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    managed.session.set_search_root(search_root.clone());
    let session = Arc::clone(&managed.session);
    // Read this at send time so changes from Settings apply immediately to
    // already-open conversations, not just sessions created afterwards.
    let custom_instructions = ctx.get_settings().await.chat_custom_instructions;
    let conversation_id = ensure_chat_conversation(&ctx, &managed)?;
    {
        let mut messages = managed.messages.lock().unwrap();
        messages.push(ChatMessageRecord {
            message_id: user_message_id,
            turn_id: Some(turn_id.clone()),
            role: "user".to_string(),
            thought: String::new(),
            content: vec![ChatReplayContentBlock::Text { text: text.clone() }],
            error: None,
            environment: Some(ChatTurnEnvironmentRecord {
                context_files: managed.context_files.lock().unwrap().clone(),
                active_doc: managed.active_doc.lock().unwrap().clone(),
                search_root: search_root.clone(),
                config_values: wilkes_api::commands::chat::config_values_from_options(
                    &managed.session.config_options(),
                ),
            }),
        });
        messages.push(ChatMessageRecord {
            message_id: turn_id.clone(),
            turn_id: Some(turn_id.clone()),
            role: "assistant".to_string(),
            thought: String::new(),
            content: Vec::new(),
            error: None,
            environment: None,
        });
    }
    let task_conversation_id = conversation_id.clone();
    let title_hint = text.clone();

    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        let result = session
            .send_with_custom_instructions(turn_id.clone(), text, custom_instructions)
            .await;
        let payload = match result {
            Ok(stop_reason) => {
                if let Some(conversation_id) = &task_conversation_id {
                    if *managed.branch_history_pending.lock().unwrap() {
                        *managed.branch_history_pending.lock().unwrap() = false;
                        session.set_branch_history(None);
                        if let Err(e) = wilkes_api::commands::chat::mark_branch_history_seeded(
                            &ctx.data_dir,
                            conversation_id,
                        ) {
                            error!("chat: failed to mark branch history seeded: {e:#}");
                        }
                    }
                    if let Err(e) = wilkes_api::commands::chat::touch_conversation(
                        &ctx.data_dir,
                        conversation_id,
                        Some(&title_hint),
                    ) {
                        error!("chat: failed to update conversation metadata: {e:#}");
                    }
                }
                serde_json::json!({ "stop_reason": stop_reason })
            }
            Err(e) => {
                error!("chat turn {turn_id} failed: {e:#}");
                if let Some(message) = managed
                    .messages
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|message| message.message_id == turn_id)
                {
                    message.error = Some(e.to_string());
                }
                emitter.emit(
                    &format!("chat/update-{turn_id}"),
                    serde_json::json!({ "kind": "error", "message": e.to_string() }),
                );
                serde_json::json!({ "stop_reason": "error" })
            }
        };
        if let Some(conversation_id) = &task_conversation_id {
            let messages = managed.messages.lock().unwrap().clone();
            if let Err(e) = wilkes_api::commands::chat::replace_conversation_messages(
                &ctx.data_dir,
                conversation_id,
                messages,
            ) {
                error!("chat: failed to persist conversation transcript: {e:#}");
            }
        }
        emitter.emit(&format!("chat/done-{turn_id}"), payload);
    });

    Ok(ChatSendResult { conversation_id })
}

#[tauri::command]
async fn chat_cancel(session_id: String, turn_id: String, app: AppHandle) -> Result<(), String> {
    info!("chat_cancel: session={session_id} turn={turn_id}");
    let session = chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    session.cancel().map_err(|e| e.to_string())
}

/// Answer a surfaced permission prompt with the user's choice. `option_id` is
/// `None` when the user dismisses/denies without picking an offered option.
#[tauri::command]
async fn chat_answer_permission(
    session_id: String,
    request_id: String,
    option_id: Option<String>,
    app: AppHandle,
) -> Result<(), String> {
    let session = chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    session.answer_permission(&request_id, option_id);
    Ok(())
}

#[tauri::command]
async fn chat_close(session_id: String, app: AppHandle) -> Result<(), String> {
    if let Some(managed) = chat_manager_state(&app).remove(&session_id) {
        managed.session.close();
    }
    Ok(())
}

// ── Delegating commands ───────────────────────────────────────────────────────

#[tauri::command]
async fn preview(
    app: AppHandle,
    match_ref: wilkes_core::types::MatchRef,
) -> Result<wilkes_core::types::PreviewData, String> {
    app_context(&app)
        .preview(match_ref)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_files(
    root: String,
    collection_id: Option<String>,
    tag_ids: Option<Vec<String>>,
    collection_expression: Option<String>,
    app: AppHandle,
) -> Result<FileListResponse, String> {
    list_files_for_ctx(
        app_context(&app),
        root,
        collection_id,
        tag_ids.unwrap_or_default(),
        collection_expression,
    )
    .await
}

#[tauri::command]
async fn open_file(
    path: String,
    app: AppHandle,
) -> Result<wilkes_core::types::PreviewData, String> {
    open_file_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn rename_file(path: String, new_name: String, app: AppHandle) -> Result<String, String> {
    rename_file_for_path(app_context(&app), path, new_name).await
}

#[tauri::command]
async fn import_files(
    paths: Vec<String>,
    root: String,
    mode: wilkes_api::commands::files::FileImportMode,
    app: AppHandle,
) -> Result<Vec<String>, String> {
    import_files_into_current_root_for_ctx(app_context(&app), paths, root, mode).await
}

#[tauri::command]
async fn read_clipboard_files() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| match clipboard_files::read() {
        Ok(paths) => paths
            .into_iter()
            .map(|path| {
                path.into_os_string()
                    .into_string()
                    .map_err(|_| "A copied file path is not valid UTF-8".to_string())
            })
            .collect(),
        Err(clipboard_files::Error::NoFiles) => Ok(Vec::new()),
        Err(clipboard_files::Error::SystemError(error)) => {
            Err(format!("Could not read copied files: {error}"))
        }
    })
    .await
    .map_err(|error| format!("Could not read copied files: {error}"))?
}

#[tauri::command]
async fn move_file(path: String, target_root: String, app: AppHandle) -> Result<String, String> {
    move_file_to_root_for_ctx(app_context(&app), path, target_root).await
}

#[tauri::command]
async fn create_directory(parent: String, name: String, app: AppHandle) -> Result<String, String> {
    app_context(&app)
        .ensure_writable()
        .map_err(|error| error.to_string())?;
    wilkes_api::commands::files::create_directory(parent.into(), name)
        .await
        .map(|path| path.display().to_string())
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn list_directories(path: String) -> Result<Vec<String>, String> {
    let mut entries = tokio::fs::read_dir(&path)
        .await
        .map_err(|error| format!("Could not read directory {path}: {error}"))?;
    let mut directories = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("Could not read directory {path}: {error}"))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| format!("Could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() {
            directories.push(entry.path().display().to_string());
        }
    }
    directories.sort_by_key(|directory| directory.to_lowercase());
    Ok(directories)
}

#[tauri::command]
async fn get_file_metadata(path: String, app: AppHandle) -> Result<DocumentMetadata, String> {
    get_file_metadata_for_path(app_context(&app), path).await
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    get_settings_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn update_settings(patch: serde_json::Value, app: AppHandle) -> Result<Settings, String> {
    update_settings_with_listeners(
        app_context(&app),
        workspace_manager(&app),
        external_mcp_manager_state(&app),
        http_api_manager_state(&app),
        patch,
    )
    .await
}

#[tauri::command]
async fn configure_external_mcp(
    enabled: bool,
    require_token: bool,
    bind_address: String,
    port: u16,
    app: AppHandle,
) -> Result<ExternalMcpStatus, String> {
    let bind_address = bind_address.parse::<std::net::IpAddr>().map_err(|_| {
        "External MCP bind address must be a valid IPv4 or IPv6 address".to_string()
    })?;
    let ctx = app_context(&app);
    let manager = external_mcp_manager_state(&app);
    let settings = update_settings_with_listeners(
        Arc::clone(&ctx),
        workspace_manager(&app),
        Arc::clone(&manager),
        http_api_manager_state(&app),
        serde_json::json!({
            "external_mcp": {
                "enabled": enabled,
                "require_token": require_token,
                "bind_address": bind_address,
                "port": port,
            }
        }),
    )
    .await?;
    Ok(manager.status(&settings.external_mcp).await)
}

/// Turns the HTTP API on or off and reports where it ended up listening.
#[tauri::command]
async fn configure_http_api(
    enabled: bool,
    bind_address: String,
    port: u16,
    app: AppHandle,
) -> Result<HttpApiStatus, String> {
    let bind_address = bind_address
        .parse::<std::net::IpAddr>()
        .map_err(|_| "HTTP API bind address must be a valid IPv4 or IPv6 address".to_string())?;
    let manager = http_api_manager_state(&app);
    let settings = update_settings_with_listeners(
        app_context(&app),
        workspace_manager(&app),
        external_mcp_manager_state(&app),
        Arc::clone(&manager),
        serde_json::json!({
            "http_api": {
                "enabled": enabled,
                "bind_address": bind_address,
                "port": port,
            }
        }),
    )
    .await?;
    Ok(manager.status(&settings.http_api).await)
}

#[tauri::command]
async fn get_http_api_status(app: AppHandle) -> Result<HttpApiStatus, String> {
    let settings = app_context(&app).get_settings().await;
    Ok(http_api_manager_state(&app)
        .status(&settings.http_api)
        .await)
}

#[tauri::command]
async fn get_external_mcp_status(app: AppHandle) -> Result<ExternalMcpStatus, String> {
    let settings = app_context(&app).get_settings().await;
    Ok(external_mcp_manager_state(&app)
        .status(&settings.external_mcp)
        .await)
}

#[tauri::command]
fn set_active_document(path: Option<String>, page: Option<u32>, app: AppHandle) {
    external_mcp_manager_state(&app).set_active_document(path, page);
}

#[tauri::command]
async fn rotate_external_mcp_token(app: AppHandle) -> Result<ExternalMcpStatus, String> {
    let ctx = app_context(&app);
    let settings = ctx.get_settings().await;
    let manager = external_mcp_manager_state(&app);
    manager.rotate_token(&settings.external_mcp).await?;
    Ok(manager.status(&settings.external_mcp).await)
}

#[tauri::command]
async fn list_bookmarks(app: AppHandle) -> Result<Vec<Bookmark>, String> {
    list_bookmarks_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn add_bookmark(bookmark: NewBookmark, app: AppHandle) -> Result<Bookmark, String> {
    add_bookmark_for_ctx(app_context(&app), bookmark).await
}

#[tauri::command]
async fn remove_bookmark(id: String, app: AppHandle) -> Result<(), String> {
    remove_bookmark_for_ctx(app_context(&app), id).await
}

#[tauri::command]
async fn update_bookmark_note(
    id: String,
    note: Option<String>,
    app: AppHandle,
) -> Result<Bookmark, String> {
    update_bookmark_note_for_ctx(app_context(&app), id, note).await
}

#[tauri::command]
async fn cluster_bookmarks(
    query: BookmarkClustersQuery,
    app: AppHandle,
) -> Result<BookmarkClustersResult, String> {
    cluster_bookmarks_for_ctx(app_context(&app), query).await
}

#[tauri::command]
async fn chunk_topics(
    request_id: String,
    query: ChunkTopicsQuery,
    app: AppHandle,
) -> Result<ChunkTopicsResult, String> {
    chunk_topics_for_ctx(app_context(&app), request_id, query).await
}

#[tauri::command]
async fn cancel_chunk_topics(request_id: String, app: AppHandle) -> Result<(), String> {
    cancel_chunk_topics_for_ctx(app_context(&app), &request_id);
    Ok(())
}

#[tauri::command]
async fn list_tags(app: AppHandle) -> Result<Vec<Tag>, String> {
    app_context(&app).list_tags().map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_tag(tag: NewTag, app: AppHandle) -> Result<Tag, String> {
    app_context(&app).create_tag(tag).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_tag(id: String, tag: UpdateTag, app: AppHandle) -> Result<Tag, String> {
    app_context(&app)
        .update_tag(&id, tag)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_tag(id: String, app: AppHandle) -> Result<(), String> {
    app_context(&app).delete_tag(&id).map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_document_tags(update: DocumentTagUpdate, app: AppHandle) -> Result<(), String> {
    app_context(&app)
        .update_document_tags(update)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_smart_collections(app: AppHandle) -> Result<Vec<SmartCollection>, String> {
    app_context(&app)
        .list_collections()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_smart_collection(
    collection: NewSmartCollection,
    app: AppHandle,
) -> Result<SmartCollection, String> {
    app_context(&app)
        .create_collection(collection)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_smart_collection(
    id: String,
    collection: UpdateSmartCollection,
    app: AppHandle,
) -> Result<SmartCollection, String> {
    app_context(&app)
        .update_collection(&id, collection)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_smart_collection(id: String, app: AppHandle) -> Result<(), String> {
    app_context(&app)
        .delete_collection(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn validate_smart_collection(expression: String) -> Result<CollectionValidation, String> {
    Ok(wilkes_api::research::ResearchStore::validate_collection(
        &expression,
    ))
}

#[tauri::command]
async fn list_search_log(
    limit: Option<usize>,
    app: AppHandle,
) -> Result<Vec<SearchLogEntry>, String> {
    app_context(&app)
        .list_search_log(limit.unwrap_or(100))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_search_log(id: String, app: AppHandle) -> Result<(), String> {
    app_context(&app)
        .delete_search_log(&id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn clear_search_log(app: AppHandle) -> Result<(), String> {
    app_context(&app)
        .clear_search_log()
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn zotero_status(app: AppHandle) -> Result<IntegrationStatus, String> {
    zotero_status_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn resolve_file_metadata(path: String, app: AppHandle) -> Result<DocumentMetadata, String> {
    resolve_file_metadata_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn refresh_file_metadata(path: Option<String>, app: AppHandle) -> Result<(), String> {
    refresh_file_metadata_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn zotero_add_item(path: String, app: AppHandle) -> Result<AddOutcome, String> {
    zotero_add_item_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn zotero_generate_citation(path: String, app: AppHandle) -> Result<CitationResult, String> {
    zotero_generate_citation_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn semantic_scholar_status(app: AppHandle) -> Result<IntegrationStatus, String> {
    semantic_scholar_status_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn semantic_scholar_lookup(
    doi: String,
    app: AppHandle,
) -> Result<SemanticScholarPaper, String> {
    semantic_scholar_lookup_for_ctx(app_context(&app), doi).await
}

#[tauri::command]
async fn openalex_status(app: AppHandle) -> Result<IntegrationStatus, String> {
    openalex_status_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn openalex_lookup(doi: String, app: AppHandle) -> Result<OpenAlexWork, String> {
    openalex_lookup_for_ctx(app_context(&app), doi).await
}

#[tauri::command]
fn is_semantic_ready(app: AppHandle) -> bool {
    is_semantic_ready_for_ctx(app_context(&app))
}

#[tauri::command]
async fn get_logs() -> Result<Vec<String>, String> {
    Ok(wilkes_api::commands::logs::get_logs())
}

#[tauri::command]
async fn clear_logs() -> Result<(), String> {
    wilkes_api::commands::logs::clear_logs();
    Ok(())
}

// ── Embed commands (delegating to AppContext) ─────────────────────────────────

#[tauri::command]
async fn download_model(selected: SelectedEmbedder, app: AppHandle) -> Result<(), String> {
    download_model_for_ctx(app_context(&app), selected).await
}

#[tauri::command]
async fn build_index(
    root: String,
    selected: SelectedEmbedder,
    app: AppHandle,
) -> Result<(), String> {
    build_index_for_ctx(app_context(&app), root, selected).await
}

#[tauri::command]
async fn embedder_capabilities(app: AppHandle) -> Result<EmbedderCapabilityManifest, String> {
    embedder_capabilities_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn get_model_size(engine: EmbeddingEngine, model_id: String) -> Result<u64, String> {
    get_model_size_for_ctx_with(engine, model_id, |engine, model_id| async move {
        wilkes_api::commands::embed::get_model_size(engine, model_id).await
    })
    .await
}

#[tauri::command]
async fn cancel_embed(app: AppHandle) -> Result<(), String> {
    cancel_embed_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn get_index_status(app: AppHandle, root: Option<String>) -> Result<IndexStatus, String> {
    get_index_status_for_ctx(app_context(&app), root).await
}

#[tauri::command]
async fn delete_index(app: AppHandle, root: Option<String>) -> Result<(), String> {
    delete_index_for_ctx(app_context(&app), root).await
}

#[tauri::command]
fn get_worker_status(app: AppHandle) -> WorkerStatus {
    get_worker_status_for_ctx(app_context(&app))
}

#[tauri::command]
fn get_worker_statuses(app: AppHandle) -> Vec<WorkerStatus> {
    get_worker_statuses_for_ctx(app_context(&app))
}

#[tauri::command]
async fn is_generation_ready(app: AppHandle) -> bool {
    app_context(&app).is_generation_ready().await
}

#[tauri::command]
async fn list_generation_models(
    app: AppHandle,
) -> Result<Vec<wilkes_core::types::GeneratorDescriptor>, String> {
    app_context(&app)
        .list_generation_models()
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn get_generation_model_size(app: AppHandle, model_id: String) -> Result<u64, String> {
    app_context(&app)
        .fetch_generation_model_size(&model_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Download (if needed) and attach the configured generation model. Progress
/// arrives on the generation-progress event stream, terminated by
/// generation-done or generation-error.
#[tauri::command]
async fn load_generation_model(app: AppHandle) -> Result<bool, String> {
    app_context(&app)
        .load_generator()
        .await
        .map(|outcome| outcome.attached())
        .map_err(|e| format!("{e:#}"))
}

// ── Catalogue ────────────────────────────────────────────────────────────────
//
// The mirror of the open teaching catalogues. Installation-wide, not
// workspace-owned: what it holds is what four public catalogues publish, which
// is the same answer whichever workspace is open.

fn catalogue_failed(error: wilkes_api::commands::catalogue::CatalogueError) -> String {
    format!("{error}")
}

/// Every provider this build knows, what the mirror holds for it, and when it
/// last did. Reports a provider that has never synced rather than omitting it.
#[tauri::command]
async fn catalogue_status(
    app: AppHandle,
) -> Result<wilkes_api::commands::catalogue::CatalogueStatusResponse, String> {
    wilkes_api::commands::catalogue::status(&app_context(&app).catalogue_dir)
        .map_err(catalogue_failed)
}

/// Recall over the mirror. Not a ranking — see `wilkes_core::catalogue`.
#[tauri::command]
async fn catalogue_search(
    app: AppHandle,
    queries: Vec<wilkes_api::commands::catalogue::CatalogueProbe>,
    limit: Option<usize>,
) -> Result<wilkes_api::commands::catalogue::CatalogueSearchResponse, String> {
    let dir = app_context(&app).catalogue_dir.clone();
    let limit = limit.unwrap_or(wilkes_api::commands::catalogue::DEFAULT_LIMIT);
    wilkes_api::commands::catalogue::search(&dir, queries, limit).map_err(catalogue_failed)
}

/// Refreshes the named providers, or all of them when none is named. Minutes,
/// for all four — the settings panel names one at a time so it can show which,
/// and each page lands on `catalogue-sync-progress` as it arrives.
#[tauri::command]
async fn catalogue_sync(
    app: AppHandle,
    providers: Option<Vec<String>>,
) -> Result<wilkes_api::commands::catalogue::CatalogueSyncResponse, String> {
    app_context(&app)
        .catalogue_sync(providers)
        .await
        .map_err(catalogue_failed)
}

/// Fetches a candidate into the workspace's uploads directory. Importing it
/// into a library root is a separate step, and the user's: this writes only
/// into Wilkes's own staging area. Bytes are reported on
/// `catalogue-download-progress` as they arrive.
#[tauri::command]
async fn catalogue_acquire(
    app: AppHandle,
    url: String,
    filename: Option<String>,
) -> Result<wilkes_core::acquire::DownloadResponse, String> {
    app_context(&app)
        .catalogue_acquire(url, filename)
        .await
        .map_err(catalogue_failed)
}

/// Every recognizer this build can read with, and the engines it compiled in.
#[tauri::command]
async fn image_recognizer_catalogue(
    app: AppHandle,
) -> wilkes_core::extract::image::dispatch::RecognizerCatalogue {
    app_context(&app).image_recognizer_catalogue()
}

/// What the named image recognizer is, where it came from, and under what
/// licence. Answers whether or not it is installed: it describes the recipe,
/// and the point of it is to be readable before the download rather than after.
#[tauri::command]
async fn image_recognizer_inventory(
    app: AppHandle,
    engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    model_id: String,
) -> Result<wilkes_core::types::RecognizerInventory, String> {
    app_context(&app)
        .image_recognizer_inventory(engine, &model_id)
        .map_err(|e| format!("{e:#}"))
}

/// Download (if needed) and verify the named image recognizer, then attach the
/// analyzer if the settings already name it. Progress arrives on the
/// image-analysis-progress stream, terminated by image-analysis-done or
/// image-analysis-error.
#[tauri::command]
async fn install_image_recognizer(
    app: AppHandle,
    engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    model_id: String,
) -> Result<(), String> {
    app_context(&app)
        .install_image_recognizer(engine, model_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn explain_related_document(
    app: AppHandle,
    request_id: String,
    anchor_path: String,
    path: String,
) -> Result<(), String> {
    app_context(&app)
        .explain_related_document(request_id, anchor_path.into(), path.into())
        .await
}

#[tauri::command]
async fn summarize_document(
    app: AppHandle,
    request_id: String,
    path: String,
) -> Result<(), String> {
    app_context(&app)
        .summarize_document(request_id, path.into())
        .await
}

#[tauri::command]
async fn summarize_search_results(
    app: AppHandle,
    request_id: String,
    input: wilkes_core::generate::tasks::search_results_summary::SearchResultsSummaryInput,
) -> Result<(), String> {
    app_context(&app)
        .summarize_search_results(request_id, input)
        .await
}

#[tauri::command]
async fn request_completion(
    app: AppHandle,
    completion_id: String,
    request: wilkes_core::completion::CompletionRequest,
) -> Result<(), String> {
    app_context(&app)
        .request_completion(completion_id, request)
        .await
}

#[tauri::command]
fn cancel_completion(app: AppHandle, completion_id: String) {
    app_context(&app).cancel_completion(&completion_id);
}

#[tauri::command]
async fn completion_feedback(
    app: AppHandle,
    completion_id: String,
    feedback: wilkes_core::completion::CompletionFeedback,
) -> Result<(), String> {
    app_context(&app)
        .completion_feedback(&completion_id, feedback)
        .await
}

#[tauri::command]
fn get_session_steering(app: AppHandle) -> wilkes_core::completion::SessionSteering {
    app_context(&app).get_session_steering()
}

#[tauri::command]
fn reset_session_steering(app: AppHandle) {
    app_context(&app).reset_session_steering();
}

#[tauri::command]
async fn save_document(app: AppHandle, path: String, text: String) -> Result<(), String> {
    app_context(&app).save_document(path.into(), text).await
}

#[tauri::command]
async fn kill_worker(app: AppHandle) -> Result<(), String> {
    kill_worker_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn set_worker_timeout(secs: u64, app: AppHandle) -> Result<(), String> {
    set_worker_timeout_for_ctx(app_context(&app), secs).await
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run() {
    wilkes_core::logging::init_logging();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let startup = Arc::new(DesktopStartupState::default());
            app.manage(Arc::clone(&startup));
            let handle = app.handle().clone();
            let platform = TauriPlatform(handle.clone());
            let DesktopStartupPlan {
                data_dir,
                settings_path,
            } = match build_startup_plan(&platform) {
                Ok(plan) => plan,
                Err(error) => {
                    error!("desktop startup preflight failed: {error:#}");
                    startup.replace(StartupStatus::unexpected(error));
                    return Ok(());
                }
            };

            let status = match collect_startup_status(&data_dir, &settings_path) {
                Ok(status) => status,
                Err(error) => {
                    error!("workspace startup preflight failed: {error:#}");
                    startup.replace(StartupStatus::unexpected(error));
                    return Ok(());
                }
            };
            if !status.is_ready() {
                startup.replace(status);
                return Ok(());
            }

            let (events_tx, _) = broadcast::channel::<(String, serde_json::Value)>(1024);
            let emitter: Arc<dyn EventEmitter> = Arc::new(WorkspaceEmitter {
                webview: TauriEmitter(handle.clone()),
                http: events_tx.clone(),
            });
            let external_mcp = Arc::new(ExternalMcpManager::new(data_dir.clone()));
            let (workspaces, event_rx, loop_fut) =
                match WorkspaceManager::new(data_dir, settings_path, emitter) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        error!("desktop startup initialization failed: {error:#}");
                        startup.replace(StartupStatus::unexpected(error));
                        return Ok(());
                    }
                };
            let ctx = workspaces.active();

            // Workspace-scoped, not context-scoped: the state resolves the
            // active workspace per request, so a workspace switch is followed
            // by the API without restarting the listener.
            let http_api = Arc::new(HttpApiManager::new(Arc::new(
                wilkes_server::http::state::AppState {
                    ctx: None,
                    workspaces: Some(Arc::clone(&workspaces)),
                    uploads_dir: ctx.data_dir.join("uploads"),
                    events_tx,
                },
            )));

            app.manage(Arc::clone(&workspaces));
            app.manage(Arc::new(ActiveSearches(Mutex::new(HashMap::new()))));
            app.manage(Arc::new(ChatManager(Mutex::new(HashMap::new()))));
            app.manage(Arc::clone(&external_mcp));
            app.manage(Arc::clone(&http_api));

            let ctx_c = Arc::clone(&ctx);
            tauri::async_runtime::spawn(async move {
                ctx_c.spawn_background_tasks(event_rx, loop_fut);
            });
            let ctx_c = Arc::clone(&ctx);
            let workspaces_for_listeners = Arc::clone(&workspaces);
            tauri::async_runtime::spawn(async move {
                let settings = ctx_c.get_settings().await;
                if let Err(error) = external_mcp
                    .apply(&settings.external_mcp, workspaces_for_listeners)
                    .await
                {
                    error!("external MCP startup failed: {error}");
                }
                if let Err(error) = http_api.apply(&settings.http_api).await {
                    error!("HTTP API startup failed: {error}");
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_startup_status,
            search,
            cancel_search,
            related_documents,
            citation_links,
            preview,
            list_files,
            open_file,
            rename_file,
            import_files,
            read_clipboard_files,
            move_file,
            create_directory,
            list_directories,
            trash_file,
            get_file_metadata,
            get_python_info,
            get_settings,
            update_settings,
            configure_external_mcp,
            get_external_mcp_status,
            set_active_document,
            rotate_external_mcp_token,
            configure_http_api,
            get_http_api_status,
            list_bookmarks,
            add_bookmark,
            remove_bookmark,
            update_bookmark_note,
            cluster_bookmarks,
            chunk_topics,
            cancel_chunk_topics,
            list_tags,
            create_tag,
            update_tag,
            delete_tag,
            update_document_tags,
            list_smart_collections,
            create_smart_collection,
            update_smart_collection,
            delete_smart_collection,
            validate_smart_collection,
            list_search_log,
            delete_search_log,
            clear_search_log,
            zotero_status,
            resolve_file_metadata,
            refresh_file_metadata,
            zotero_add_item,
            zotero_generate_citation,
            semantic_scholar_status,
            semantic_scholar_lookup,
            openalex_status,
            openalex_lookup,
            pick_directory,
            download_model,
            build_index,
            embedder_capabilities,
            get_model_size,
            cancel_embed,
            get_index_status,
            delete_index,
            get_logs,
            clear_logs,
            get_data_paths,
            list_workspaces,
            create_workspace,
            rename_workspace,
            switch_workspace,
            open_path,
            reveal_path,
            is_semantic_ready,
            is_generation_ready,
            list_generation_models,
            get_generation_model_size,
            load_generation_model,
            catalogue_status,
            catalogue_search,
            catalogue_sync,
            catalogue_acquire,
            image_recognizer_catalogue,
            image_recognizer_inventory,
            install_image_recognizer,
            explain_related_document,
            summarize_document,
            summarize_search_results,
            request_completion,
            cancel_completion,
            completion_feedback,
            get_session_steering,
            reset_session_steering,
            save_document,
            get_worker_status,
            get_worker_statuses,
            kill_worker,
            set_worker_timeout,
            chat_list_backends,
            chat_install_backend,
            chat_list_conversations,
            chat_forget_conversation,
            chat_start,
            chat_open_conversation,
            chat_fork_conversation,
            chat_set_config_option,
            chat_add_context,
            chat_remove_context,
            chat_set_active_doc,
            chat_send,
            chat_cancel,
            chat_answer_permission,
            chat_close,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| handle_exit_event(&app_handle, event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wilkes_api::context::EventEmitter;
    use wilkes_core::types::SourceOrigin;
    use wilkes_core::worker::manager::WorkerPaths;

    static OPEN_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn desktop_startup_state_can_hold_feature_blockers_without_a_runtime() {
        let state = DesktopStartupState::default();
        assert!(state.status().is_ready());
        state.replace(StartupStatus::unexpected("database upgrade required"));
        let status = state.status();
        assert!(!status.is_ready());
        assert_eq!(status.blockers[0].id, "application.startup-failed");
    }

    /// An alpha install reaches the application instead of the gate: its
    /// library is adopted at startup, so there is nothing for the user to do.
    #[test]
    fn desktop_preflight_does_not_stop_a_pre_workspace_install() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_vec(&serde_json::json!({
                "last_directory": dir.path().join("legacy-library")
            }))
            .unwrap(),
        )
        .unwrap();

        let status = collect_startup_status(dir.path(), &settings_path).unwrap();

        assert!(status.is_ready());
    }

    #[test]
    fn chat_events_accumulate_into_the_persisted_assistant_message() {
        let mut messages = vec![ChatMessageRecord {
            message_id: "assistant-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            role: "assistant".to_string(),
            thought: String::new(),
            content: Vec::new(),
            error: None,
            environment: None,
        }];

        apply_chat_event(
            &mut messages,
            &ChatEvent::TextDelta {
                turn_id: "turn-1".to_string(),
                delta: "First ".to_string(),
            },
        );
        apply_chat_event(
            &mut messages,
            &ChatEvent::TextDelta {
                turn_id: "turn-1".to_string(),
                delta: "answer".to_string(),
            },
        );
        apply_chat_event(
            &mut messages,
            &ChatEvent::ThoughtDelta {
                turn_id: "turn-1".to_string(),
                delta: "reasoning".to_string(),
            },
        );

        assert_eq!(messages[0].thought, "reasoning");
        assert_eq!(
            messages[0].content,
            vec![ChatReplayContentBlock::Text {
                text: "First answer".to_string()
            }]
        );
    }

    struct MockEmitter;

    impl EventEmitter for MockEmitter {
        fn emit(&self, _name: &str, _payload: serde_json::Value) {}
    }

    #[derive(Clone)]
    struct SearchEmitter {
        events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl SearchEventSink for SearchEmitter {
        fn emit_event(&self, name: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((name.to_string(), payload));
        }
    }

    #[tokio::test]
    async fn test_get_python_info() {
        let result = get_python_info().await;
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_get_python_info_fallback() {
        // Just check it doesn't panic
        let _ = get_python_info().await;
    }

    #[tokio::test]
    async fn test_active_searches() {
        let active = ActiveSearches(Mutex::new(HashMap::new()));
        let mut guard = active.0.lock().unwrap();
        guard.insert("test".to_string(), tokio::spawn(async {}));
        assert!(guard.contains_key("test"));
    }

    #[tokio::test]
    async fn test_search_for_ctx_orchestration_emits_and_cleans_up() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("example.txt"), "hello world").unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let emitter = SearchEmitter {
            events: Arc::clone(&events),
        };
        let active_searches = Arc::new(ActiveSearches(Mutex::new(HashMap::new())));
        let (_ctx_dir, ctx) = test_ctx();
        wilkes_api::commands::settings::update_settings(
            &ctx.settings_path,
            serde_json::json!({ "last_directory": root.clone() }),
        )
        .await
        .unwrap();
        let query = wilkes_core::types::SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: wilkes_core::types::SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let search_id = search_for_ctx(
            ctx,
            Arc::clone(&active_searches),
            emitter,
            query,
            Some("search-1".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(search_id, "search-1");

        for _ in 0..20 {
            if active_searches.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }

        assert!(active_searches.0.lock().unwrap().is_empty());

        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|(name, _)| name == "search-result-search-1"));
        assert!(events
            .iter()
            .any(|(name, _)| name == "search-complete-search-1"));
    }

    #[test]
    fn test_validate_open_target() {
        let dir = tempdir().unwrap();
        assert!(validate_open_target(&dir.path().display().to_string()).is_ok());
        assert_eq!(
            validate_open_target(&dir.path().join("missing").display().to_string()),
            Err("Path does not exist".into())
        );
        assert!(validate_open_target("https://doi.org/10.1000/xyz123").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_open_path_uses_spawned_opener() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = OPEN_PATH_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let opener_name = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let opener = dir.path().join(opener_name);
        std::fs::write(&opener, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&opener).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opener, perms).unwrap();

        let path = dir.path().join("folder");
        std::fs::create_dir(&path).unwrap();

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), original_path);
        std::env::set_var("PATH", &new_path);

        let res = open_path(path.display().to_string()).await;
        std::env::set_var("PATH", original_path);

        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn test_get_model_size_for_ctx_with_injected_fetcher() {
        let result = get_model_size_for_ctx_with(
            EmbeddingEngine::Candle,
            "model-x".to_string(),
            |_engine, model_id| async move {
                assert_eq!(model_id, "model-x");
                Ok(42)
            },
        )
        .await
        .unwrap();

        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_get_model_size_for_ctx_with_error() {
        let err = get_model_size_for_ctx_with(
            EmbeddingEngine::Candle,
            "model-x".to_string(),
            |_engine, _model_id| async move { Err(anyhow::anyhow!("no size")) },
        )
        .await
        .unwrap_err();

        assert!(err.contains("no size"));
    }

    #[tokio::test]
    async fn test_delete_index_for_ctx_removes_db() {
        let (_dir, ctx) = test_ctx();
        let db_path = ctx.data_dir.join("semantic_index.db");
        std::fs::write(&db_path, "fake db").unwrap();

        delete_index_for_ctx(Arc::clone(&ctx), None).await.unwrap();
        assert!(!db_path.exists());
    }

    #[tokio::test]
    async fn test_get_index_status_for_ctx_missing() {
        let (_dir, ctx) = test_ctx();
        let result = get_index_status_for_ctx(ctx, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_open_file_for_ctx_allows_outside_data_dir() {
        let (_dir, ctx) = test_ctx();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let preview = open_file_for_ctx(ctx, outside.display().to_string())
            .await
            .unwrap();
        match preview {
            wilkes_core::types::PreviewData::Text { content, .. } => {
                assert_eq!(content, "hello")
            }
            _ => panic!("Expected text preview"),
        }
    }

    #[tokio::test]
    async fn test_get_file_metadata_for_path_allows_outside_data_dir() {
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let (_dir, ctx) = test_ctx();
        let metadata = get_file_metadata_for_path(ctx, outside.display().to_string())
            .await
            .unwrap();
        assert_eq!(metadata, DocumentMetadata::default());
    }

    #[tokio::test]
    async fn test_refresh_file_metadata_for_ctx_allows_outside_data_dir() {
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let (_data_dir, ctx) = test_ctx();
        refresh_file_metadata_for_ctx(ctx, Some(outside.display().to_string()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_rename_file_for_path_allows_outside_data_dir() {
        let (_data_dir, ctx) = test_ctx();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let renamed =
            rename_file_for_path(ctx, outside.display().to_string(), "renamed.txt".into())
                .await
                .unwrap();

        let renamed_path = outside_dir.path().join("renamed.txt");
        assert_eq!(renamed, renamed_path.display().to_string());
        assert!(!outside.exists());
        assert_eq!(std::fs::read_to_string(renamed_path).unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_import_files_into_current_root_for_ctx_moves_external_file() {
        let (_dir, ctx) = test_ctx();
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("paper.pdf");
        std::fs::write(&source, "pdf").unwrap();

        ctx.update_settings(serde_json::json!({
            "last_directory": root_dir.path().display().to_string(),
            "supported_extensions": ["pdf"]
        }))
        .await
        .unwrap();

        let imported = import_files_into_current_root_for_ctx(
            ctx,
            vec![source.display().to_string()],
            root_dir.path().display().to_string(),
            wilkes_api::commands::files::FileImportMode::Move,
        )
        .await
        .unwrap();

        let target = root_dir.path().join("paper.pdf");
        assert_eq!(
            imported,
            vec![target.canonicalize().unwrap().display().to_string()]
        );
        assert!(!source.exists());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "pdf");
    }

    #[tokio::test]
    async fn test_import_files_into_current_root_for_ctx_copies_external_file() {
        let (_dir, ctx) = test_ctx();
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("paper.pdf");
        std::fs::write(&source, "pdf").unwrap();

        ctx.update_settings(serde_json::json!({
            "last_directory": root_dir.path().display().to_string(),
            "supported_extensions": ["pdf"]
        }))
        .await
        .unwrap();

        let imported = import_files_into_current_root_for_ctx(
            ctx,
            vec![source.display().to_string()],
            root_dir.path().display().to_string(),
            wilkes_api::commands::files::FileImportMode::Copy,
        )
        .await
        .unwrap();

        let target = root_dir.path().join("paper.pdf");
        assert_eq!(
            imported,
            vec![target.canonicalize().unwrap().display().to_string()]
        );
        assert_eq!(std::fs::read_to_string(source).unwrap(), "pdf");
        assert_eq!(std::fs::read_to_string(target).unwrap(), "pdf");
    }

    #[tokio::test]
    async fn test_build_index_for_ctx_missing_root() {
        let (_dir, ctx) = test_ctx();
        let err = build_index_for_ctx(
            ctx,
            "/definitely/missing/root".to_string(),
            SelectedEmbedder::default_for(EmbeddingEngine::Candle),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Index root not found"));
    }

    #[tokio::test]
    async fn test_get_worker_status_for_ctx_and_timeout_update() {
        let dir = tempdir().unwrap();
        use std::path::PathBuf;

        let emitter = Arc::new(MockEmitter);
        let (ctx, _rx, loop_fut) = AppContext::new(
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
        let _loop_handle = tokio::spawn(loop_fut);

        let status = get_worker_status_for_ctx(Arc::clone(&ctx));
        assert!(!status.active);
        assert_eq!(status.timeout_secs, 300);

        set_worker_timeout_for_ctx(Arc::clone(&ctx), 123)
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let status = get_worker_status_for_ctx(ctx);
        assert_eq!(status.timeout_secs, 123);
    }

    #[tokio::test]
    async fn capabilities_cover_every_supported_engine() {
        let (_dir, ctx) = test_ctx();
        let manifest = embedder_capabilities_for_ctx(ctx).await.unwrap();
        assert_eq!(manifest.engines, EmbeddingEngine::supported_engines());
        assert!(!manifest.models.is_empty());
        // The picker reads its default from here now, so the manifest has to
        // carry one rather than leaving the UI to guess at a name.
        assert!(manifest.models.iter().any(|model| model.is_default));
    }

    #[tokio::test]
    async fn test_cancel_and_kill_helpers() {
        let (_dir, ctx) = test_ctx();
        cancel_embed_for_ctx(Arc::clone(&ctx)).await.unwrap();
        kill_worker_for_ctx(ctx).await.unwrap();
    }

    #[tokio::test]
    async fn test_set_worker_timeout_for_ctx_errors_without_loop() {
        let (_dir, ctx) = test_ctx();
        let err = set_worker_timeout_for_ctx(ctx, 12).await.unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_search_for_ctx_removes_handle() {
        let active = Arc::new(ActiveSearches(Mutex::new(HashMap::new())));
        active.0.lock().unwrap().insert(
            "search-1".to_string(),
            tokio::spawn(async {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            }),
        );

        cancel_search_for_ctx(Arc::clone(&active), "search-1");

        assert!(active.0.lock().unwrap().is_empty());
    }

    /// A workspace manager over a temporary data directory, for the listener
    /// tests: the external MCP resolves its library through one of these
    /// rather than through a single context.
    fn test_workspaces() -> (tempfile::TempDir, Arc<WorkspaceManager>) {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter: Arc<dyn wilkes_api::context::EventEmitter> = Arc::new(MockEmitter);
        let (workspaces, _rx, _loop) =
            WorkspaceManager::new(dir.path().to_path_buf(), settings_path, emitter).unwrap();
        (dir, workspaces)
    }

    fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let emitter = Arc::new(MockEmitter);
        let paths = WorkerPaths {
            python_path: std::path::PathBuf::from("python"),
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            venv_dir: std::path::PathBuf::from("venv"),
            worker_bin: std::path::PathBuf::from("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let (ctx, _rx, _loop) =
            AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
        (dir, ctx)
    }

    #[tokio::test]
    async fn test_get_settings_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let settings = get_settings_for_ctx(ctx).await.unwrap();
        assert!(settings.favorites.is_empty());
        assert!(settings.last_directory.is_none());
        assert!(!settings.semantic.enabled);
    }

    #[tokio::test]
    async fn test_bookmark_helpers_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let bookmark = add_bookmark_for_ctx(
            Arc::clone(&ctx),
            NewBookmark {
                path: "/tmp/example.pdf".into(),
                origin: SourceOrigin::PdfPage {
                    page: 4,
                    bbox: None,
                },
                text_range: None,
                quote: "quote".to_string(),
                note: None,
                rects: Vec::new(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            list_bookmarks_for_ctx(Arc::clone(&ctx))
                .await
                .unwrap()
                .len(),
            1
        );

        let noted = update_bookmark_note_for_ctx(
            Arc::clone(&ctx),
            bookmark.id.clone(),
            Some("a note".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(noted.note.as_deref(), Some("a note"));

        remove_bookmark_for_ctx(ctx.clone(), bookmark.id)
            .await
            .unwrap();
        assert!(list_bookmarks_for_ctx(ctx).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_update_settings_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let updated = update_settings_for_ctx(
            Arc::clone(&ctx),
            serde_json::json!({
                "semantic": {
                    "enabled": true
                }
            }),
        )
        .await
        .unwrap();
        assert!(updated.semantic.enabled);
    }

    /// A manager over a context that owns a workspace, as the app has at
    /// startup.
    fn http_api_manager(ctx: Arc<AppContext>) -> HttpApiManager {
        let (events_tx, _) = broadcast::channel(16);
        HttpApiManager::new(Arc::new(wilkes_server::http::state::AppState {
            ctx: Some(Arc::clone(&ctx)),
            workspaces: None,
            uploads_dir: ctx.data_dir.join("uploads"),
            events_tx,
        }))
    }

    #[tokio::test]
    async fn http_api_serves_the_workspace_this_process_already_owns() {
        let (_dir, ctx) = test_ctx();
        let manager = http_api_manager(ctx);

        // Off is the default, and off means nothing is listening.
        let settings = HttpApiSettings::default();
        assert!(!settings.enabled);
        manager.apply(&settings).await.unwrap();
        assert!(!manager.status(&settings).await.running);

        // A real port, the way the other listener's tests pick one: port 0 is
        // rejected on purpose, since a setting the user cannot find the app on
        // is not a valid setting.
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let enabled = HttpApiSettings {
            enabled: true,
            port,
            ..HttpApiSettings::default()
        };
        manager.apply(&enabled).await.unwrap();
        let status = manager.status(&enabled).await;
        assert!(status.running);
        assert!(status.error.is_none());
        let url = status.url.expect("a running listener reports its url");

        // The point of the whole exercise: a consumer reaches this workspace
        // over HTTP instead of opening its databases as a second owner.
        let health = reqwest_get(&format!("{url}/health")).await;
        assert_eq!(health, "200");

        manager.stop().await;
        assert!(!manager.status(&enabled).await.running);
    }

    #[tokio::test]
    async fn a_port_already_taken_is_reported_and_leaves_nothing_listening() {
        let (_dir, ctx) = test_ctx();
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();

        let manager = http_api_manager(ctx);
        let settings = HttpApiSettings {
            enabled: true,
            port,
            ..HttpApiSettings::default()
        };
        let error = manager.apply(&settings).await.unwrap_err();
        assert!(
            error.contains("Could not start the Wilkes HTTP API"),
            "{error}"
        );

        // Reported, and reported where the settings screen reads it — not
        // logged and forgotten.
        let status = manager.status(&settings).await;
        assert!(!status.running);
        assert_eq!(status.error.as_deref(), Some(error.as_str()));
    }

    /// Minimal HTTP/1.1 GET returning the status code, so the desktop crate
    /// does not gain an HTTP client dependency for one assertion.
    async fn reqwest_get(url: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let rest = url.strip_prefix("http://").expect("http url");
        let (authority, path) = rest.split_once('/').expect("url has a path");
        let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
        stream
            .write_all(
                format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
            .split_whitespace()
            .nth(1)
            .expect("status line")
            .to_string()
    }

    #[test]
    fn external_mcp_token_is_persistent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(EXTERNAL_MCP_TOKEN_FILENAME);
        let first = load_or_create_external_mcp_token(&path).unwrap();
        let second = load_or_create_external_mcp_token(&path).unwrap();

        assert_eq!(first, second);
        assert!(first.len() >= 64);
        assert_eq!(read_external_mcp_token(&path).unwrap(), first);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn external_mcp_manager_starts_and_stops_with_setting() {
        let (_dir, workspaces) = test_workspaces();
        let library = tempdir().unwrap();
        workspaces
            .active()
            .update_settings(serde_json::json!({
                "last_directory": library.path()
            }))
            .await
            .unwrap();
        let token_dir = tempdir().unwrap();
        let manager = ExternalMcpManager::new(token_dir.path().to_path_buf());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let enabled = ExternalMcpSettings {
            enabled: true,
            require_token: false,
            bind_address: "127.0.0.1".parse().unwrap(),
            port,
        };

        manager
            .apply(&enabled, Arc::clone(&workspaces))
            .await
            .unwrap();
        let status = manager.status(&enabled).await;
        assert!(status.enabled);
        assert!(status.running);
        let expected_url = format!("http://127.0.0.1:{port}/mcp");
        assert_eq!(status.url.as_deref(), Some(expected_url.as_str()));
        assert!(!status.require_token);
        assert!(status.token.is_none());

        let disabled = ExternalMcpSettings {
            enabled: false,
            require_token: false,
            bind_address: "127.0.0.1".parse().unwrap(),
            port,
        };
        manager.apply(&disabled, workspaces).await.unwrap();
        let status = manager.status(&disabled).await;
        assert!(!status.running);
        assert!(status.token.is_none());
    }

    #[tokio::test]
    async fn external_mcp_manager_toggles_token_authentication_live() {
        let (_dir, workspaces) = test_workspaces();
        let token_dir = tempdir().unwrap();
        let manager = ExternalMcpManager::new(token_dir.path().to_path_buf());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let open = ExternalMcpSettings {
            enabled: true,
            require_token: false,
            bind_address: "127.0.0.1".parse().unwrap(),
            port,
        };
        manager.apply(&open, Arc::clone(&workspaces)).await.unwrap();
        let url = manager.status(&open).await.url.unwrap();
        assert!(manager.status(&open).await.token.is_none());
        assert!(manager.rotate_token(&open).await.is_err());

        let authenticated = ExternalMcpSettings {
            require_token: true,
            ..open.clone()
        };
        manager
            .apply(&authenticated, Arc::clone(&workspaces))
            .await
            .unwrap();
        let status = manager.status(&authenticated).await;
        assert!(status.token.is_some());
        assert_eq!(status.url.as_deref(), Some(url.as_str()));

        manager.apply(&open, Arc::clone(&workspaces)).await.unwrap();
        let status = manager.status(&open).await;
        assert_eq!(status.url.as_deref(), Some(url.as_str()));
        assert!(status.token.is_none());
        assert!(manager.token_path.exists());

        manager
            .apply(
                &ExternalMcpSettings {
                    enabled: false,
                    ..open
                },
                workspaces,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn external_mcp_manager_reports_port_collision_without_fallback() {
        let (_dir, workspaces) = test_workspaces();
        let token_dir = tempdir().unwrap();
        let manager = ExternalMcpManager::new(token_dir.path().to_path_buf());
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = occupied.local_addr().unwrap().port();
        let enabled = ExternalMcpSettings {
            enabled: true,
            require_token: false,
            bind_address: "127.0.0.1".parse().unwrap(),
            port,
        };

        let error = manager.apply(&enabled, workspaces).await.unwrap_err();
        assert!(error.contains(&port.to_string()));
        let status = manager.status(&enabled).await;
        assert!(!status.running);
        assert!(status.error.is_some());
    }

    #[tokio::test]
    async fn external_mcp_manager_switches_bind_address_on_the_same_port() {
        let (_dir, workspaces) = test_workspaces();
        let token_dir = tempdir().unwrap();
        let manager = ExternalMcpManager::new(token_dir.path().to_path_buf());
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let loopback = ExternalMcpSettings {
            enabled: true,
            require_token: false,
            bind_address: "127.0.0.1".parse().unwrap(),
            port,
        };
        manager
            .apply(&loopback, Arc::clone(&workspaces))
            .await
            .unwrap();

        let all_interfaces = ExternalMcpSettings {
            enabled: true,
            require_token: false,
            bind_address: "0.0.0.0".parse().unwrap(),
            port,
        };
        manager
            .apply(&all_interfaces, Arc::clone(&workspaces))
            .await
            .unwrap();
        let status = manager.status(&all_interfaces).await;
        let expected_url = format!("http://0.0.0.0:{port}/mcp");
        assert!(status.running);
        assert_eq!(status.bind_address, all_interfaces.bind_address);
        assert_eq!(status.url.as_deref(), Some(expected_url.as_str()));

        manager
            .apply(
                &ExternalMcpSettings {
                    enabled: false,
                    ..all_interfaces
                },
                workspaces,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_list_files_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let dir = tempdir().unwrap();
        let path = dir.path().join("example.txt");
        std::fs::write(&path, "hello").unwrap();
        let files = list_files_for_ctx(
            Arc::clone(&ctx),
            dir.path().display().to_string(),
            None,
            vec![],
            None,
        )
        .await
        .unwrap();
        assert!(!files.files.is_empty());

        let tag = ctx
            .create_tag(NewTag {
                name: "Reviewed".into(),
                color: None,
            })
            .unwrap();
        ctx.update_document_tags(DocumentTagUpdate {
            paths: vec![path],
            add_tag_ids: vec![tag.id.clone()],
            remove_tag_ids: vec![],
        })
        .unwrap();
        let tagged = list_files_for_ctx(
            Arc::clone(&ctx),
            dir.path().display().to_string(),
            None,
            vec![tag.id],
            None,
        )
        .await
        .unwrap();
        assert_eq!(tagged.files.len(), 1);
        let unmatched = list_files_for_ctx(
            Arc::clone(&ctx),
            dir.path().display().to_string(),
            None,
            vec!["missing-tag".into()],
            None,
        )
        .await
        .unwrap();
        assert!(unmatched.files.is_empty());

        let preview = list_files_for_ctx(
            ctx,
            dir.path().display().to_string(),
            None,
            vec![],
            Some("extension == 'txt'".into()),
        )
        .await
        .unwrap();
        assert_eq!(preview.files.len(), 1);
    }

    #[tokio::test]
    async fn test_trash_file_rejects_missing_and_non_file_paths() {
        let dir = tempdir().unwrap();
        let directory_error = trash_file_for_path(dir.path().display().to_string())
            .await
            .unwrap_err();
        assert!(directory_error.contains("non-file path"));

        let missing_error =
            trash_file_for_path(dir.path().join("missing.pdf").display().to_string())
                .await
                .unwrap_err();
        assert!(missing_error.contains("Cannot trash"));
    }

    #[tokio::test]
    async fn test_is_semantic_ready_for_ctx() {
        let (_dir, ctx) = test_ctx();
        assert!(!is_semantic_ready_for_ctx(ctx));
    }

    #[tokio::test]
    async fn test_cancel_embed_for_ctx() {
        let (_dir, ctx) = test_ctx();
        assert!(super::cancel_embed_for_ctx(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_data_paths_and_logs_for_ctx() {
        let (_dir, _ctx) = test_ctx();

        let paths = data_paths_from(
            "test-data".to_string(),
            "test-data/workspaces/w1".to_string(),
        );
        assert_eq!(paths.app_data, "test-data");
        // The workspace path is its own answer, not the installation's: the
        // Data page names both, and naming one twice is what it used to do.
        assert_eq!(paths.workspace, "test-data/workspaces/w1");

        let _ = super::get_python_info().await;

        super::clear_logs().await.unwrap();
        let logs = super::get_logs().await.unwrap();
        assert!(logs.is_empty());
    }
}
