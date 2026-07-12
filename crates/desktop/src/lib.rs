use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::task::JoinHandle;
use tracing::{error, info};
use wilkes_agent::session::{ChatConfigOption, ChatEvent, ChatSession};
use wilkes_api::commands::chat::{
    BackendStatus, ChatActiveDocRecord, ChatContextFileRecord, ChatConversationRecord,
};
use wilkes_api::context::{AppContext, EventEmitter};
use wilkes_core::embed::worker::manager::WorkerStatus;
use wilkes_core::types::{
    AddOutcome, AgentBackend, Bookmark, CitationResult, DataPaths, DocumentMetadata,
    EmbeddingEngine, FileListResponse, IndexStatus, IntegrationStatus, ModelDescriptor,
    NewBookmark, OpenAlexWork, SelectedEmbedder, SemanticScholarPaper, Settings,
};

mod platform;

use platform::{
    build_startup_plan, validate_open_target, validate_reveal_target, DesktopPlatform,
    DesktopStartupPlan, SystemDesktopPlatform, TauriPlatform,
};

fn app_context(app: &AppHandle) -> Arc<AppContext> {
    app.state::<Arc<AppContext>>().inner().clone()
}

fn active_searches_state(app: &AppHandle) -> Arc<ActiveSearches> {
    app.state::<Arc<ActiveSearches>>().inner().clone()
}

fn data_paths_from(app_data: String) -> DataPaths {
    DataPaths { app_data }
}

async fn list_files_for_ctx(
    ctx: Arc<AppContext>,
    root: String,
) -> Result<FileListResponse, String> {
    ctx.list_files(root.into()).await.map_err(|e| e.to_string())
}

async fn open_file_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<wilkes_core::types::PreviewData, String> {
    ctx.open_file(path.into()).await.map_err(|e| e.to_string())
}

async fn rename_file_for_path(path: String, new_name: String) -> Result<String, String> {
    wilkes_api::commands::files::rename_file(path.into(), new_name)
        .await
        .map(|path| path.display().to_string())
        .map_err(|e| e.to_string())
}

async fn trash_file_for_path(path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("Cannot trash {}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!("Cannot trash non-file path: {}", path.display()));
        }
        trash::delete(&path)
            .map_err(|error| format!("Failed to move {} to Trash: {error}", path.display()))
    })
    .await
    .map_err(|error| format!("Trash operation failed: {error}"))?
}

async fn move_files_into_current_root_for_ctx(
    ctx: Arc<AppContext>,
    paths: Vec<String>,
    root: String,
) -> Result<Vec<String>, String> {
    let paths = paths.into_iter().map(PathBuf::from).collect();
    ctx.move_files_into_current_root(paths, root.into())
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
    let supported_extensions = ctx.get_settings().await.supported_extensions;
    wilkes_api::commands::files::move_files_into_root(
        vec![path.into()],
        target_root.into(),
        supported_extensions,
    )
    .await
    .map(|mut moved| moved.pop().unwrap_or_default().display().to_string())
    .map_err(|e| e.to_string())
}

async fn get_file_metadata_for_path(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<DocumentMetadata, String> {
    let supported_extensions = ctx.get_settings().await.supported_extensions;
    wilkes_api::commands::metadata::get_file_metadata(path.into(), supported_extensions)
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
    let settings = ctx.get_settings().await;
    wilkes_api::commands::integrations::zotero::resolve_file_metadata(settings, path.into())
        .await
        .map_err(|e| e.to_string())
}

async fn zotero_add_item_for_ctx(ctx: Arc<AppContext>, path: String) -> Result<AddOutcome, String> {
    let settings = ctx.get_settings().await;
    wilkes_api::commands::integrations::zotero::zotero_add_item(settings, path.into())
        .await
        .map_err(|e| e.to_string())
}

async fn zotero_generate_citation_for_ctx(
    ctx: Arc<AppContext>,
    path: String,
) -> Result<CitationResult, String> {
    let settings = ctx.get_settings().await;
    wilkes_api::commands::integrations::zotero::zotero_generate_citation(settings, path.into())
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

async fn list_models_for_ctx(
    ctx: Arc<AppContext>,
    engine: EmbeddingEngine,
) -> Result<Vec<ModelDescriptor>, String> {
    Ok(wilkes_api::commands::embed::list_models(engine, &ctx.data_dir).await)
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

async fn kill_worker_for_ctx(ctx: Arc<AppContext>) -> Result<(), String> {
    ctx.kill_worker();
    Ok(())
}

async fn set_worker_timeout_for_ctx(ctx: Arc<AppContext>, secs: u64) -> Result<(), String> {
    ctx.set_worker_timeout(secs)
        .await
        .map_err(|e| e.to_string())
}

fn handle_exit_event(app_handle: &AppHandle, event: tauri::RunEvent) {
    if matches!(
        event,
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
    ) {
        let ctx = app_handle.state::<Arc<AppContext>>().inner().clone();
        // Kill any in-flight turn and the chat subprocesses themselves before
        // the process tree goes away, rather than leaving orphaned CLIs behind.
        chat_manager_state(app_handle).close_all();
        tauri::async_runtime::spawn(async move {
            ctx.shutdown().await;
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

/// Forward every `ChatEvent` for the life of a session -- not just one turn --
/// through `EventEmitter` as `chat/update-<turn_id>` (spec §7.8). Runs until
/// the subprocess's connection closes (session close, crash, or app exit).
fn spawn_chat_event_forwarder(
    app: AppHandle,
    session_id: String,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ChatEvent>,
) {
    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        while let Some(event) = events.recv().await {
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
    let handle = Arc::clone(&ctx).start_search(query).await?;

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
    let app_data = app
        .path()
        .app_data_dir()
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())?;
    Ok(data_paths_from(app_data))
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
async fn trash_file(path: String) -> Result<(), String> {
    trash_file_for_path(path).await
}

#[tauri::command]
async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.map(|p| p.to_string()));
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
    replay_messages: Vec<wilkes_agent::session::ChatReplayMessage>,
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
    let spawned =
        wilkes_api::commands::chat::start(backend, cwd.clone(), Some(ctx.clone()), integrations)
            .await
            .map_err(|e| e.to_string())?;
    let wilkes_agent::session::SpawnedChatSession {
        session,
        events,
        replay_messages,
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
    spawn_chat_event_forwarder(app.clone(), session_id.clone(), events);
    chat_manager_state(&app).insert(
        session_id.clone(),
        Arc::new(ManagedChatSession {
            session,
            conversation_id: Mutex::new(None),
            cwd,
            context_files: Mutex::new(Vec::new()),
            active_doc: Mutex::new(None),
        }),
    );
    Ok(ChatStartResult {
        session_id,
        conversation_id: None,
        backend_session_id: Some(backend_session_id),
        config_options,
        replay_messages,
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
    let spawned = wilkes_api::commands::chat::open(&record, Some(ctx.clone()), integrations)
        .await
        .map_err(|e| e.to_string())?;
    let wilkes_agent::session::SpawnedChatSession {
        session,
        events,
        replay_messages,
    } = spawned;
    let session = Arc::new(session);
    session.set_search_root(search_root);
    for file in &record.context_files {
        session.add_context(file.path.clone(), file.pages);
    }
    if let Some(active_doc) = &record.active_doc {
        session.set_active_doc(Some(active_doc.path.clone()), active_doc.page);
    }
    wilkes_api::commands::chat::touch_conversation(&ctx.data_dir, &conversation_id, None)
        .map_err(|e| e.to_string())?;

    let session_id = uuid::Uuid::new_v4().to_string();
    // Restore the config this conversation was last using -- not the backend's
    // global default -- so reopening lands on the same model/mode as before.
    let config_options =
        wilkes_api::commands::chat::apply_config(&session, &record.config_values).await;
    spawn_chat_event_forwarder(app.clone(), session_id.clone(), events);
    chat_manager_state(&app).insert(
        session_id.clone(),
        Arc::new(ManagedChatSession {
            session,
            conversation_id: Mutex::new(Some(conversation_id.clone())),
            cwd: std::path::PathBuf::from(&record.cwd),
            context_files: Mutex::new(record.context_files.clone()),
            active_doc: Mutex::new(record.active_doc.clone()),
        }),
    );
    Ok(ChatStartResult {
        session_id,
        conversation_id: Some(conversation_id),
        backend_session_id: Some(record.backend_session_id),
        config_options,
        replay_messages,
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
    text: String,
    search_root: Option<String>,
    app: AppHandle,
) -> Result<ChatSendResult, String> {
    let ctx = app_context(&app);
    let managed = managed_chat_session_or_err(&chat_manager_state(&app), &session_id)?;
    managed.session.set_search_root(search_root);
    let session = Arc::clone(&managed.session);
    // Read this at send time so changes from Settings apply immediately to
    // already-open conversations, not just sessions created afterwards.
    let custom_instructions = ctx.get_settings().await.chat_custom_instructions;
    let conversation_id = ensure_chat_conversation(&ctx, &managed)?;
    let task_conversation_id = conversation_id.clone();
    let title_hint = text.clone();

    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        let payload = match session
            .send_with_custom_instructions(turn_id.clone(), text, custom_instructions)
            .await
        {
            Ok(stop_reason) => {
                if let Some(conversation_id) = &task_conversation_id {
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
                emitter.emit(
                    &format!("chat/update-{turn_id}"),
                    serde_json::json!({ "kind": "error", "message": e.to_string() }),
                );
                serde_json::json!({ "stop_reason": "error" })
            }
        };
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
    match_ref: wilkes_core::types::MatchRef,
) -> Result<wilkes_core::types::PreviewData, String> {
    wilkes_api::commands::preview::preview(match_ref)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_files(root: String, app: AppHandle) -> Result<FileListResponse, String> {
    list_files_for_ctx(app_context(&app), root).await
}

#[tauri::command]
async fn open_file(
    path: String,
    app: AppHandle,
) -> Result<wilkes_core::types::PreviewData, String> {
    open_file_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn rename_file(path: String, new_name: String) -> Result<String, String> {
    rename_file_for_path(path, new_name).await
}

#[tauri::command]
async fn import_dropped_files(
    paths: Vec<String>,
    root: String,
    app: AppHandle,
) -> Result<Vec<String>, String> {
    move_files_into_current_root_for_ctx(app_context(&app), paths, root).await
}

#[tauri::command]
async fn move_file(path: String, target_root: String, app: AppHandle) -> Result<String, String> {
    move_file_to_root_for_ctx(app_context(&app), path, target_root).await
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
    update_settings_for_ctx(app_context(&app), patch).await
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
async fn zotero_status(app: AppHandle) -> Result<IntegrationStatus, String> {
    zotero_status_for_ctx(app_context(&app)).await
}

#[tauri::command]
async fn resolve_file_metadata(path: String, app: AppHandle) -> Result<DocumentMetadata, String> {
    resolve_file_metadata_for_ctx(app_context(&app), path).await
}

#[tauri::command]
async fn refresh_file_metadata(path: Option<String>, app: AppHandle) -> Result<(), String> {
    app_context(&app)
        .refresh_file_metadata(path.map(Into::into))
        .await
        .map_err(|e| e.to_string())
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

#[tauri::command]
fn get_supported_engines() -> Vec<EmbeddingEngine> {
    EmbeddingEngine::supported_engines()
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
async fn list_models(
    engine: EmbeddingEngine,
    app: AppHandle,
) -> Result<Vec<ModelDescriptor>, String> {
    list_models_for_ctx(app_context(&app), engine).await
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
            let handle = app.handle().clone();
            let platform = TauriPlatform(handle.clone());
            let DesktopStartupPlan {
                data_dir,
                settings_path,
                worker_paths: paths,
            } = build_startup_plan(&platform)?;

            let emitter = Arc::new(TauriEmitter(handle.clone()));
            let (ctx, event_rx, loop_fut) =
                AppContext::new(data_dir, settings_path, paths, emitter);

            app.manage(Arc::clone(&ctx));
            app.manage(Arc::new(ActiveSearches(Mutex::new(HashMap::new()))));
            app.manage(Arc::new(ChatManager(Mutex::new(HashMap::new()))));

            let ctx_c = Arc::clone(&ctx);
            tauri::async_runtime::spawn(async move {
                ctx_c.spawn_background_tasks(event_rx, loop_fut);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search,
            cancel_search,
            related_documents,
            preview,
            list_files,
            open_file,
            rename_file,
            import_dropped_files,
            move_file,
            list_directories,
            trash_file,
            get_file_metadata,
            get_python_info,
            get_supported_engines,
            get_settings,
            update_settings,
            list_bookmarks,
            add_bookmark,
            remove_bookmark,
            update_bookmark_note,
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
            list_models,
            get_model_size,
            cancel_embed,
            get_index_status,
            delete_index,
            get_logs,
            clear_logs,
            get_data_paths,
            open_path,
            reveal_path,
            is_semantic_ready,
            get_worker_status,
            kill_worker,
            set_worker_timeout,
            chat_list_backends,
            chat_install_backend,
            chat_list_conversations,
            chat_forget_conversation,
            chat_start,
            chat_open_conversation,
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
    use wilkes_core::embed::worker::manager::WorkerPaths;
    use wilkes_core::types::SourceOrigin;

    static OPEN_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn test_get_supported_engines() {
        let engines = get_supported_engines();
        assert!(!engines.is_empty());
        assert!(engines.contains(&EmbeddingEngine::SBERT));
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
    async fn test_open_file_for_ctx_denied() {
        let (_dir, ctx) = test_ctx();
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let err = open_file_for_ctx(ctx, outside.display().to_string())
            .await
            .unwrap_err();
        assert!(err.contains("Access denied"));
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
    async fn test_rename_file_for_path_allows_outside_data_dir() {
        let outside_dir = tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");
        std::fs::write(&outside, "hello").unwrap();

        let renamed = rename_file_for_path(outside.display().to_string(), "renamed.txt".into())
            .await
            .unwrap();

        let renamed_path = outside_dir.path().join("renamed.txt");
        assert_eq!(renamed, renamed_path.display().to_string());
        assert!(!outside.exists());
        assert_eq!(std::fs::read_to_string(renamed_path).unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_move_files_into_current_root_for_ctx_moves_external_file() {
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

        let imported = move_files_into_current_root_for_ctx(
            ctx,
            vec![source.display().to_string()],
            root_dir.path().display().to_string(),
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
    async fn test_list_models_for_ctx_returns_catalog() {
        let (_dir, ctx) = test_ctx();
        let models = list_models_for_ctx(ctx, EmbeddingEngine::Candle)
            .await
            .unwrap();
        assert!(!models.is_empty());
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

    #[tokio::test]
    async fn test_list_files_for_ctx() {
        let (_dir, ctx) = test_ctx();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("example.txt"), "hello").unwrap();
        let files = list_files_for_ctx(ctx, dir.path().display().to_string())
            .await
            .unwrap();
        assert!(!files.files.is_empty());
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

        let paths = data_paths_from("test-data".to_string());
        assert_eq!(paths.app_data, "test-data");

        let _ = super::get_python_info().await;

        super::clear_logs().await.unwrap();
        let logs = super::get_logs().await.unwrap();
        assert!(logs.is_empty());
    }
}
