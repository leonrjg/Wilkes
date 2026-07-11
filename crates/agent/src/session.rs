//! `ChatSession`: one long-lived CLI subprocess speaking ACP, and the single
//! read-only permission boundary shared by all supported backends (spec §4, §8).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities,
    InitializeRequest, LoadSessionRequest, NewSessionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, PromptRequest, ReadTextFileRequest, ReadTextFileResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionId, SetSessionConfigOptionRequest, StopReason, TextContent,
    ToolCallStatus, ToolCallUpdate, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{AcpAgent, Agent, Client, ConnectionTo, Responder};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};
use wilkes_core::types::AgentBackend;

use crate::context::{build_context_block, ActiveDoc, ActiveDocText, ContextFile};
use crate::search::SearchService;

const ACTIVE_DOC_CONTEXT_CHAR_LIMIT: usize = 12_000;

/// One update streamed out of a `ChatSession` while a turn runs, for as long
/// as the session (not just one turn) is open. `wilkes_api`/`wilkes_desktop`
/// forward these through `EventEmitter` as `chat/update-<turn_id>` (spec
/// §7.8); the terminal `chat/done-<turn_id>` comes from `ChatSession::send`'s
/// return value instead, since that's when the caller learns the stop reason.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    TextDelta {
        turn_id: String,
        delta: String,
    },
    ThoughtDelta {
        turn_id: String,
        delta: String,
    },
    ToolCall {
        turn_id: String,
        tool_call_id: String,
        title: Option<String>,
        status: Option<String>,
        locations: Option<Vec<ChatLocation>>,
        /// Detail behind the compact chip: the tool's own reported content
        /// (text/diff/terminal), for a click-to-expand view. `None` on an
        /// update that didn't touch this field, same patch semantics as the
        /// other fields here.
        content: Option<Vec<ChatToolContent>>,
        raw_input: Option<serde_json::Value>,
        raw_output: Option<serde_json::Value>,
    },
    /// The agent asked to run a tool that isn't Wilkes's own read-only MCP
    /// server, so the decision is surfaced to the user (spec §8, revised: the
    /// user owns security via the agent's Mode). The turn blocks on the
    /// subprocess side until [`ChatSession::answer_permission`] resolves this
    /// `request_id`.
    PermissionRequest {
        turn_id: String,
        request_id: String,
        tool_call_id: String,
        title: Option<String>,
        options: Vec<ChatPermissionOption>,
    },
    /// The subprocess/connection died outside of any turn's request/response
    /// cycle (spawn failure, crash, protocol error).
    SessionError {
        message: String,
    },
    /// The agent's session configuration (model, mode, thought level, ...)
    /// changed -- either because we set it, or the agent pushed an update on
    /// its own. Not turn-scoped: can arrive at any time.
    ConfigOptionsUpdated {
        options: Vec<ChatConfigOption>,
    },
}

/// One value a `ChatConfigOption` can be set to. `group` is set only for
/// grouped selectors (e.g. models organized by provider); flattened here so
/// clients that don't care about grouping can ignore it.
#[derive(Debug, Clone, Serialize)]
pub struct ChatConfigChoice {
    pub value: String,
    pub name: String,
    pub group: Option<String>,
}

/// A single ACP session configuration option (spec extension beyond the base
/// integration spec: ACP's `session/set_config_option` mechanism, which ships
/// a well-known `model` category alongside mode/thought-level/etc.). Only the
/// stable `select` kind is surfaced -- `boolean` is behind ACP's unstable
/// feature flag, which this crate does not enable.
#[derive(Debug, Clone, Serialize)]
pub struct ChatConfigOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current_value: String,
    pub choices: Vec<ChatConfigChoice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatLocation {
    pub path: String,
    pub line: Option<u32>,
}

/// One choice offered for a surfaced permission request, mirrored from the
/// agent's own `PermissionOption` so the UI renders exactly the options the
/// agent proposed (allow/reject, once/always) and echoes the chosen
/// `option_id` straight back.
#[derive(Debug, Clone, Serialize)]
pub struct ChatPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

/// Registry of permission requests awaiting a user decision, keyed by the
/// `request_id` we mint per surfaced prompt. The ACP request handler parks on
/// the receiver; [`ChatSession::answer_permission`] resolves the sender from a
/// *separate* task (the Tauri command thread) -- deliberately not routed through
/// the command loop, which is blocked awaiting the in-flight `PromptResponse`.
type PendingPermissions = Arc<Mutex<HashMap<String, oneshot::Sender<Option<String>>>>>;

/// A tool call's own reported content, for the click-to-expand detail view.
/// Image/audio/embedded-resource content blocks are dropped -- out of scope
/// for a read-only document Q&A pane, and none of the supported backends emit
/// them for the file-read/search tools this pane actually exercises.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatToolContent {
    Text {
        text: String,
    },
    Diff {
        path: String,
        old_text: Option<String>,
        new_text: String,
    },
    Terminal {
        terminal_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatReplayToolCall {
    pub tool_call_id: String,
    pub title: String,
    pub status: String,
    pub locations: Vec<ChatLocation>,
    pub content: Vec<ChatToolContent>,
    pub raw_input: Option<serde_json::Value>,
    pub raw_output: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatReplayMessage {
    pub role: String,
    pub text: String,
    pub thought: String,
    pub tools: Vec<ChatReplayToolCall>,
}

pub struct SpawnedChatSession {
    pub session: ChatSession,
    pub events: mpsc::UnboundedReceiver<ChatEvent>,
    pub replay_messages: Vec<ChatReplayMessage>,
}

#[derive(Debug, Clone)]
pub enum SessionOpenMode {
    New,
    Load { backend_session_id: String },
}

struct SessionReady {
    backend_session_id: String,
    replay_messages: Vec<ChatReplayMessage>,
}

enum SessionCommand {
    Prompt {
        turn_id: String,
        blocks: Vec<ContentBlock>,
        reply: oneshot::Sender<Result<String, String>>,
    },
    SetConfigOption {
        config_id: String,
        value: String,
        reply: oneshot::Sender<Result<Vec<ChatConfigOption>, String>>,
    },
    Close,
}

#[derive(Debug, Default)]
struct SharedContextState {
    active_doc: Option<ActiveDoc>,
    context_files: Vec<ContextFile>,
    search_root: Option<PathBuf>,
    first_turn_sent: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ContextSnapshot {
    pub active_doc: Option<ActiveDoc>,
    pub context_files: Vec<ContextFile>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ContextStateHandle {
    state: Arc<Mutex<SharedContextState>>,
}

impl ContextStateHandle {
    pub(crate) fn add_context(&self, path: String, pages: Option<u32>) {
        let mut state = self.state.lock().unwrap();
        if state.context_files.iter().any(|f| f.path == path) {
            return;
        }
        state.context_files.push(ContextFile {
            path,
            pages,
            added_this_turn: true,
        });
    }

    pub(crate) fn remove_context(&self, path: &str) {
        self.state
            .lock()
            .unwrap()
            .context_files
            .retain(|f| f.path != path);
    }

    pub(crate) fn set_active_doc(&self, path: Option<String>, page: Option<u32>) {
        self.state.lock().unwrap().active_doc = path.map(|path| ActiveDoc { path, page });
    }

    pub(crate) fn set_search_root(&self, root: Option<String>) {
        self.state.lock().unwrap().search_root = root.map(PathBuf::from);
    }

    pub(crate) fn search_root(&self) -> Option<PathBuf> {
        self.state.lock().unwrap().search_root.clone()
    }

    pub(crate) fn context_paths(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .context_files
            .iter()
            .map(|f| f.path.clone())
            .collect()
    }

    pub(crate) fn snapshot(&self) -> ContextSnapshot {
        let state = self.state.lock().unwrap();
        ContextSnapshot {
            active_doc: state.active_doc.clone(),
            context_files: state.context_files.clone(),
        }
    }

    pub(crate) fn prepare_turn(&self) -> (bool, Option<ActiveDoc>, Vec<ContextFile>) {
        let mut state = self.state.lock().unwrap();
        let first_turn = !state.first_turn_sent;
        let active_doc = state.active_doc.clone();
        let context_files = state.context_files.clone();
        state.first_turn_sent = true;
        for file in state.context_files.iter_mut() {
            file.added_this_turn = false;
        }
        (first_turn, active_doc, context_files)
    }

    pub(crate) fn is_allowed(&self, path: &Path) -> bool {
        let Ok(canonical) = path.canonicalize() else {
            return false;
        };
        let matches = |candidate: &str| {
            Path::new(candidate)
                .canonicalize()
                .map(|c| c == canonical)
                .unwrap_or(false)
        };
        let state = self.state.lock().unwrap();
        state.context_files.iter().any(|f| matches(&f.path))
            || state.active_doc.as_ref().is_some_and(|d| matches(&d.path))
    }
}

/// One chat session = one subprocess. Switching backends means a new
/// `ChatSession`, never re-pointing this one (spec §4).
pub struct ChatSession {
    pub backend: AgentBackend,
    backend_session_id: String,
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    cancel_tx: mpsc::UnboundedSender<()>,
    state: ContextStateHandle,
    config_options: Arc<Mutex<Vec<ChatConfigOption>>>,
    pending_permissions: PendingPermissions,
    _mcp: Option<crate::mcp::McpRuntime>,
}

impl ChatSession {
    pub fn add_context(&self, path: String, pages: Option<u32>) {
        self.state.add_context(path, pages);
    }

    pub fn remove_context(&self, path: &str) {
        self.state.remove_context(path);
    }

    pub fn set_active_doc(&self, path: Option<String>, page: Option<u32>) {
        self.state.set_active_doc(path, page);
    }

    pub fn set_search_root(&self, root: Option<String>) {
        self.state.set_search_root(root);
    }

    pub fn context_paths(&self) -> Vec<String> {
        self.state.context_paths()
    }

    pub fn backend_session_id(&self) -> &str {
        &self.backend_session_id
    }

    /// Send one turn: build the pushed context block (§6.1), prepend it, and
    /// block until the agent's `PromptResponse` resolves. Streamed content
    /// arrives separately through the `ChatEvent` receiver returned by
    /// [`spawn`].
    pub async fn send(&self, turn_id: String, text: String) -> anyhow::Result<String> {
        self.send_with_custom_instructions(turn_id, text, String::new())
            .await
    }

    /// Same as [`Self::send`], with the current global chat instructions.
    /// Instructions are supplied per turn so edits take effect immediately in
    /// open sessions as well as newly started conversations.
    pub async fn send_with_custom_instructions(
        &self,
        turn_id: String,
        text: String,
        custom_instructions: String,
    ) -> anyhow::Result<String> {
        let (first_turn, active_doc, context_files) = self.state.prepare_turn();
        let active_doc_text = active_doc.as_ref().map(active_doc_text_for_context);
        let context_block = build_context_block(
            first_turn,
            active_doc.as_ref(),
            &context_files,
            active_doc_text.as_ref(),
            &custom_instructions,
        );
        let blocks = vec![
            ContentBlock::Text(TextContent::new(context_block)),
            ContentBlock::Text(TextContent::new(text)),
        ];

        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Prompt {
                turn_id,
                blocks,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("chat session is closed"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("chat session ended before responding"))?
            .map_err(|message| anyhow::anyhow!(message))
    }

    pub fn cancel(&self) -> anyhow::Result<()> {
        // Cancel must bypass the command loop: during an in-flight prompt that
        // loop is blocked awaiting PromptResponse, so queued commands cannot
        // interrupt the turn.
        drain_pending_permissions(&self.pending_permissions);
        self.cancel_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("chat session is closed"))
    }

    /// Resolve a surfaced permission request with the user's choice: `Some`
    /// option_id selects that option (allow/reject as the agent defined it),
    /// `None` cancels. Resolving an already-answered/expired request is a
    /// no-op (logged), not an error -- the turn may have ended before the click
    /// landed, and a stale click must not surface as a command failure.
    pub fn answer_permission(&self, request_id: &str, option_id: Option<String>) {
        match self.pending_permissions.lock().unwrap().remove(request_id) {
            Some(sender) => {
                let _ = sender.send(option_id);
            }
            None => warn!(
                request_id,
                "chat: answer for unknown/expired permission request"
            ),
        }
    }

    /// The agent's current session configuration (model, mode, ...), as of
    /// `session/new` or the last update. Synchronous -- no round trip to the
    /// subprocess -- so callers can seed UI state right after `spawn` returns.
    pub fn config_options(&self) -> Vec<ChatConfigOption> {
        self.config_options.lock().unwrap().clone()
    }

    /// Set one session configuration option (e.g. the model). Returns the
    /// full, fresh option list the agent reports back, since setting one
    /// option can change what others show as current.
    pub async fn set_config_option(
        &self,
        config_id: String,
        value: String,
    ) -> anyhow::Result<Vec<ChatConfigOption>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SetConfigOption {
                config_id,
                value,
                reply: reply_tx,
            })
            .map_err(|_| anyhow::anyhow!("chat session is closed"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("chat session ended before responding"))?
            .map_err(|message| anyhow::anyhow!(message))
    }

    /// Ends the subprocess. Callers close in-flight turns via `cancel()` before
    /// `close()` when they need graceful turn cancellation.
    pub fn close(&self) {
        let _ = self.cmd_tx.send(SessionCommand::Close);
    }
}

/// Spawn the backend's subprocess, complete the ACP handshake, and open a
/// session. Returns once `initialize` + `session/new` have both succeeded, so
/// callers know immediately whether the backend is actually usable (not just
/// installed).
pub async fn spawn(backend: AgentBackend, cwd: PathBuf) -> anyhow::Result<SpawnedChatSession> {
    spawn_with_mode(backend, cwd, SessionOpenMode::New, None).await
}

pub async fn spawn_with_search(
    backend: AgentBackend,
    cwd: PathBuf,
    search: Option<Arc<dyn SearchService>>,
) -> anyhow::Result<SpawnedChatSession> {
    spawn_with_mode(backend, cwd, SessionOpenMode::New, search).await
}

pub async fn load(
    backend: AgentBackend,
    cwd: PathBuf,
    backend_session_id: String,
) -> anyhow::Result<SpawnedChatSession> {
    spawn_with_mode(
        backend,
        cwd,
        SessionOpenMode::Load { backend_session_id },
        None,
    )
    .await
}

pub async fn load_with_search(
    backend: AgentBackend,
    cwd: PathBuf,
    backend_session_id: String,
    search: Option<Arc<dyn SearchService>>,
) -> anyhow::Result<SpawnedChatSession> {
    spawn_with_mode(
        backend,
        cwd,
        SessionOpenMode::Load { backend_session_id },
        search,
    )
    .await
}

async fn spawn_with_mode(
    backend: AgentBackend,
    cwd: PathBuf,
    open_mode: SessionOpenMode,
    search: Option<Arc<dyn SearchService>>,
) -> anyhow::Result<SpawnedChatSession> {
    let spec = crate::resolve_launch_spec(backend)?;
    let mut command_line = vec![spec.command.display().to_string()];
    command_line.extend(spec.args);
    let agent = AcpAgent::from_args(command_line).map_err(|e| {
        anyhow::anyhow!("failed to configure {} launch: {e}", crate::label(backend))
    })?;

    let (events_tx, events_rx) = mpsc::unbounded_channel::<ChatEvent>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel::<anyhow::Result<SessionReady>>();

    let state = ContextStateHandle::default();
    let mcp_runtime = if matches!(backend, AgentBackend::ClaudeCode | AgentBackend::Codex) {
        Some(crate::mcp::start(state.clone(), cwd.clone(), search).await?)
    } else {
        None
    };
    let mcp_servers = mcp_runtime
        .as_ref()
        .map(|runtime| vec![runtime.server_config()])
        .unwrap_or_default();
    let state_for_read = state.clone();

    let config_options: Arc<Mutex<Vec<ChatConfigOption>>> = Arc::new(Mutex::new(Vec::new()));
    let config_options_for_notif = Arc::clone(&config_options);
    let config_options_for_loop = Arc::clone(&config_options);
    let replay_messages: Arc<Mutex<Vec<ChatReplayMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let replay_messages_for_notif = Arc::clone(&replay_messages);
    let replaying_history: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let replaying_history_for_notif = Arc::clone(&replaying_history);

    let current_turn: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let current_turn_for_notif = Arc::clone(&current_turn);
    let current_turn_for_perm = Arc::clone(&current_turn);
    let events_tx_for_notif = events_tx.clone();
    let events_tx_for_perm = events_tx.clone();
    let events_tx_for_crash = events_tx.clone();

    let pending_permissions: PendingPermissions = Arc::new(Mutex::new(HashMap::new()));
    let pending_for_perm = Arc::clone(&pending_permissions);
    let pending_for_loop = Arc::clone(&pending_permissions);

    tokio::spawn(async move {
        let run: Result<(), agent_client_protocol::Error> = Client
            .builder()
            .on_receive_notification(
                move |notification: agent_client_protocol::schema::v1::SessionNotification, _cx| {
                    let current_turn = Arc::clone(&current_turn_for_notif);
                    let config_options = Arc::clone(&config_options_for_notif);
                    let replay_messages = Arc::clone(&replay_messages_for_notif);
                    let replaying_history = Arc::clone(&replaying_history_for_notif);
                    let events_tx = events_tx_for_notif.clone();
                    async move {
                        forward_notification(
                            notification,
                            &current_turn,
                            &config_options,
                            &replay_messages,
                            &replaying_history,
                            &events_tx,
                        );
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                move |request: ReadTextFileRequest, responder: Responder<ReadTextFileResponse>, _cx| {
                    let state = state_for_read.clone();
                    async move {
                        match handle_read(&state, &request) {
                            Ok(content) => responder.respond(ReadTextFileResponse::new(content)),
                            Err(message) => {
                                warn!(path = %request.path.display(), %message, "chat: fs/read_text_file denied");
                                responder.respond_with_error(agent_client_protocol::Error::new(
                                    -32602, message,
                                ))
                            }
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                |request: WriteTextFileRequest, responder: Responder<WriteTextFileResponse>, _cx| async move {
                    // Wilkes does not offer *client-delegated* writes: it has no
                    // editor buffers to reconcile, so `fs.writeTextFile` is
                    // advertised as false in `initialize`. Agents that honor
                    // that write with their own tools instead, gated by the
                    // user's Mode and the permission prompt below -- so this is
                    // a path well-behaved agents never take.
                    warn!(
                        path = %request.path.display(),
                        "chat: declined client-delegated fs/write_text_file (not offered)"
                    );
                    responder.respond_with_error(agent_client_protocol::Error::new(
                        -32600,
                        "Wilkes does not perform client-side file writes; use your own file tool",
                    ))
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                move |request: RequestPermissionRequest, responder: Responder<RequestPermissionResponse>, _cx| {
                    let pending = Arc::clone(&pending_for_perm);
                    let events_tx = events_tx_for_perm.clone();
                    let current_turn = Arc::clone(&current_turn_for_perm);
                    async move {
                        let tool_call_id = request.tool_call.tool_call_id.0.to_string();
                        let title = request.tool_call.fields.title.clone();

                        // Wilkes's own read-only MCP tools are the Q&A pane's
                        // internal plumbing -- auto-allow so the user is never
                        // prompted for the reads the pane itself drives.
                        if is_wilkes_mcp_call(&request.tool_call) {
                            let outcome = allow_outcome(&request.options)
                                .unwrap_or(RequestPermissionOutcome::Cancelled);
                            info!(%tool_call_id, "chat: auto-allowed Wilkes MCP tool call");
                            return responder.respond(RequestPermissionResponse::new(outcome));
                        }

                        // Everything else is the user's call (they own security
                        // via the agent's Mode): surface an interactive prompt
                        // and park until they answer. Without an active turn we
                        // have nowhere to surface it, so deny and log rather
                        // than hang the subprocess.
                        let Some(turn_id) = current_turn.lock().unwrap().clone() else {
                            warn!(%tool_call_id, ?title, "chat: permission requested outside a turn -- denying");
                            return responder.respond(RequestPermissionResponse::new(
                                RequestPermissionOutcome::Cancelled,
                            ));
                        };

                        let request_id = uuid::Uuid::new_v4().to_string();
                        let (decision_tx, decision_rx) = oneshot::channel::<Option<String>>();
                        pending.lock().unwrap().insert(request_id.clone(), decision_tx);

                        let options: Vec<ChatPermissionOption> =
                            request.options.iter().map(to_chat_permission_option).collect();
                        info!(%tool_call_id, %request_id, ?title, "chat: surfacing permission request to user");
                        let _ = events_tx.send(ChatEvent::PermissionRequest {
                            turn_id,
                            request_id: request_id.clone(),
                            tool_call_id,
                            title,
                            options,
                        });

                        // The answer arrives via `ChatSession::answer_permission`
                        // on another task, resolving this receiver directly --
                        // it is deliberately not routed through the command loop,
                        // which is blocked awaiting the in-flight PromptResponse.
                        let chosen = decision_rx.await.ok().flatten();
                        pending.lock().unwrap().remove(&request_id);
                        let outcome = match chosen {
                            Some(option_id) => RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(PermissionOptionId::from(option_id)),
                            ),
                            None => RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(RequestPermissionResponse::new(outcome))
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, move |cx: ConnectionTo<Agent>| async move {
                if let Err(e) = cx
                    .send_request(InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                        ClientCapabilities::new().fs(
                            FileSystemCapabilities::new()
                                .read_text_file(true)
                                .write_text_file(false),
                        ),
                    ))
                    .block_task()
                    .await
                {
                    let _ = ready_tx.send(Err(anyhow::anyhow!("initialize failed: {e}")));
                    return Err(e);
                }

                let session_id: SessionId = match open_mode {
                    SessionOpenMode::New => {
                        let new_session = NewSessionRequest::new(cwd).mcp_servers(mcp_servers);
                        match cx.send_request(new_session).block_task().await {
                            Ok(response) => {
                                *config_options_for_loop.lock().unwrap() = response
                                    .config_options
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(to_chat_config_option)
                                    .collect();
                                response.session_id
                            }
                            Err(e) => {
                                let _ = ready_tx.send(Err(anyhow::anyhow!("session/new failed: {e}")));
                                return Err(e);
                            }
                        }
                    }
                    SessionOpenMode::Load { backend_session_id } => {
                        *replaying_history.lock().unwrap() = true;
                        let load_session =
                            LoadSessionRequest::new(backend_session_id.clone(), cwd)
                                .mcp_servers(mcp_servers);
                        match cx.send_request(load_session).block_task().await {
                            Ok(response) => {
                                *replaying_history.lock().unwrap() = false;
                                *config_options_for_loop.lock().unwrap() = response
                                    .config_options
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(to_chat_config_option)
                                    .collect();
                                SessionId::from(backend_session_id)
                            }
                            Err(e) => {
                                *replaying_history.lock().unwrap() = false;
                                let _ = ready_tx.send(Err(anyhow::anyhow!("session/load failed: {e}")));
                                return Err(e);
                            }
                        }
                    }
                };
                let _ = ready_tx.send(Ok(SessionReady {
                    backend_session_id: session_id.0.to_string(),
                    replay_messages: replay_messages.lock().unwrap().clone(),
                }));

                let cancel_session_id = session_id.clone();
                let cancel_cx = cx.clone();
                let cancel_task = tokio::spawn(async move {
                    while cancel_rx.recv().await.is_some() {
                        if let Err(e) = cancel_cx
                            .send_notification(CancelNotification::new(cancel_session_id.clone()))
                        {
                            error!("chat: session/cancel failed: {e}");
                        }
                    }
                });

                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        SessionCommand::Prompt {
                            turn_id,
                            blocks,
                            reply,
                        } => {
                            *current_turn.lock().unwrap() = Some(turn_id);
                            let result = cx
                                .send_request(PromptRequest::new(session_id.clone(), blocks))
                                .block_task()
                                .await;
                            *current_turn.lock().unwrap() = None;
                            // The turn is over: any permission prompt still
                            // parked (agent abandoned it) would hang its handler
                            // holding the ACP responder -- resolve them as cancel.
                            drain_pending_permissions(&pending_for_loop);
                            let outcome = match result {
                                Ok(response) => Ok(stop_reason_str(response.stop_reason).to_string()),
                                Err(e) => Err(e.message),
                            };
                            let _ = reply.send(outcome);
                        }
                        SessionCommand::SetConfigOption {
                            config_id,
                            value,
                            reply,
                        } => {
                            let result = cx
                                .send_request(SetSessionConfigOptionRequest::new(
                                    session_id.clone(),
                                    config_id,
                                    value,
                                ))
                                .block_task()
                                .await;
                            let outcome = match result {
                                Ok(response) => {
                                    let options: Vec<ChatConfigOption> = response
                                        .config_options
                                        .into_iter()
                                        .map(to_chat_config_option)
                                        .collect();
                                    *config_options_for_loop.lock().unwrap() = options.clone();
                                    Ok(options)
                                }
                                Err(e) => Err(e.message),
                            };
                            let _ = reply.send(outcome);
                        }
                        SessionCommand::Close => {
                            drain_pending_permissions(&pending_for_loop);
                            break;
                        }
                    }
                }
                cancel_task.abort();
                Ok(())
            })
            .await;

        if let Err(e) = run {
            // The closure above already reports handshake failures through
            // `ready_tx` before returning `Err`, so a caller blocked on
            // `spawn()` sees the real error either way; this covers the
            // connection dying after the handshake succeeded (mid-session
            // subprocess crash), which no in-flight caller is waiting on.
            error!("chat session ended with error: {e}");
            let _ = events_tx_for_crash.send(ChatEvent::SessionError {
                message: e.to_string(),
            });
        }
    });

    let ready = ready_rx.await.map_err(|_| {
        anyhow::anyhow!(
            "{} exited before the ACP handshake completed. {}.",
            crate::label(backend),
            crate::auth_note(backend)
        )
    })??;

    Ok(SpawnedChatSession {
        session: ChatSession {
            backend,
            backend_session_id: ready.backend_session_id,
            cmd_tx,
            cancel_tx,
            state,
            config_options,
            pending_permissions,
            _mcp: mcp_runtime,
        },
        events: events_rx,
        replay_messages: ready.replay_messages,
    })
}

fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        _ => "end_turn",
    }
}

fn tool_call_status_str(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "pending",
    }
}

fn forward_notification(
    notification: agent_client_protocol::schema::v1::SessionNotification,
    current_turn: &Arc<Mutex<Option<String>>>,
    config_options: &Arc<Mutex<Vec<ChatConfigOption>>>,
    replay_messages: &Arc<Mutex<Vec<ChatReplayMessage>>>,
    replaying_history: &Arc<Mutex<bool>>,
    events_tx: &mpsc::UnboundedSender<ChatEvent>,
) {
    use agent_client_protocol::schema::v1::SessionUpdate;

    // Not turn-scoped: config (e.g. the agent switching its own model, or
    // confirming a change we made) can be reported at any time, not just
    // mid-turn, so this is handled before the turn-id gate below.
    if let SessionUpdate::ConfigOptionUpdate(update) = &notification.update {
        let options: Vec<ChatConfigOption> = update
            .config_options
            .iter()
            .cloned()
            .map(to_chat_config_option)
            .collect();
        *config_options.lock().unwrap() = options.clone();
        let _ = events_tx.send(ChatEvent::ConfigOptionsUpdated { options });
        return;
    }

    let Some(turn_id) = current_turn.lock().unwrap().clone() else {
        if *replaying_history.lock().unwrap() {
            append_replay_update(&mut replay_messages.lock().unwrap(), notification.update);
        }
        return;
    };

    let event = match notification.update {
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => Some(ChatEvent::TextDelta {
                turn_id,
                delta: text.text,
            }),
            _ => None,
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match chunk.content {
            ContentBlock::Text(text) => Some(ChatEvent::ThoughtDelta {
                turn_id,
                delta: text.text,
            }),
            _ => None,
        },
        SessionUpdate::ToolCall(tool_call) => Some(ChatEvent::ToolCall {
            turn_id,
            tool_call_id: tool_call.tool_call_id.0.to_string(),
            title: Some(tool_call.title),
            status: Some(tool_call_status_str(tool_call.status).to_string()),
            locations: Some(
                tool_call
                    .locations
                    .into_iter()
                    .map(to_chat_location)
                    .collect(),
            ),
            content: Some(
                tool_call
                    .content
                    .into_iter()
                    .filter_map(to_chat_tool_content)
                    .collect(),
            ),
            raw_input: tool_call.raw_input,
            raw_output: tool_call.raw_output,
        }),
        SessionUpdate::ToolCallUpdate(update) => Some(ChatEvent::ToolCall {
            turn_id,
            tool_call_id: update.tool_call_id.0.to_string(),
            title: update.fields.title,
            status: update
                .fields
                .status
                .map(|s| tool_call_status_str(s).to_string()),
            locations: update
                .fields
                .locations
                .map(|locs| locs.into_iter().map(to_chat_location).collect()),
            content: update.fields.content.map(|blocks| {
                blocks
                    .into_iter()
                    .filter_map(to_chat_tool_content)
                    .collect()
            }),
            raw_input: update.fields.raw_input,
            raw_output: update.fields.raw_output,
        }),
        _ => None,
    };

    if let Some(event) = event {
        let _ = events_tx.send(event);
    }
}

fn append_replay_update(
    messages: &mut Vec<ChatReplayMessage>,
    update: agent_client_protocol::schema::v1::SessionUpdate,
) {
    use agent_client_protocol::schema::v1::SessionUpdate;

    match update {
        SessionUpdate::UserMessageChunk(chunk) => {
            if let ContentBlock::Text(text) = chunk.content {
                append_replay_user_text(messages, text.text);
            }
        }
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(text) = chunk.content {
                append_replay_text(messages, "assistant", text.text);
            }
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            if let ContentBlock::Text(text) = chunk.content {
                let message = ensure_replay_assistant(messages);
                message.thought.push_str(&text.text);
            }
        }
        SessionUpdate::ToolCall(tool_call) => {
            let message = ensure_replay_assistant(messages);
            message.tools.push(ChatReplayToolCall {
                tool_call_id: tool_call.tool_call_id.0.to_string(),
                title: tool_call.title,
                status: tool_call_status_str(tool_call.status).to_string(),
                locations: tool_call
                    .locations
                    .into_iter()
                    .map(to_chat_location)
                    .collect(),
                content: tool_call
                    .content
                    .into_iter()
                    .filter_map(to_chat_tool_content)
                    .collect(),
                raw_input: tool_call.raw_input,
                raw_output: tool_call.raw_output,
            });
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let message = ensure_replay_assistant(messages);
            upsert_replay_tool(message, update);
        }
        _ => {}
    }
}

fn append_replay_user_text(messages: &mut Vec<ChatReplayMessage>, text: String) {
    let text = strip_wilkes_context_prefix(&text).unwrap_or(text);
    if text.is_empty() {
        return;
    }
    append_replay_text(messages, "user", text);
}

fn strip_wilkes_context_prefix(text: &str) -> Option<String> {
    let rest = text.strip_prefix("<wilkes-context>")?;
    let (_, after_context) = rest.split_once("</wilkes-context>")?;
    Some(
        after_context
            .trim_start_matches(char::is_whitespace)
            .to_string(),
    )
}

fn append_replay_text(messages: &mut Vec<ChatReplayMessage>, role: &str, text: String) {
    if let Some(last) = messages.last_mut() {
        if last.role == role {
            last.text.push_str(&text);
            return;
        }
    }
    messages.push(ChatReplayMessage {
        role: role.to_string(),
        text,
        thought: String::new(),
        tools: Vec::new(),
    });
}

fn ensure_replay_assistant(messages: &mut Vec<ChatReplayMessage>) -> &mut ChatReplayMessage {
    let needs_new = messages
        .last()
        .map(|message| message.role != "assistant")
        .unwrap_or(true);
    if needs_new {
        messages.push(ChatReplayMessage {
            role: "assistant".to_string(),
            text: String::new(),
            thought: String::new(),
            tools: Vec::new(),
        });
    }
    messages
        .last_mut()
        .expect("assistant replay message exists")
}

fn upsert_replay_tool(message: &mut ChatReplayMessage, update: ToolCallUpdate) {
    let tool_call_id = update.tool_call_id.0.to_string();
    let Some(tool) = message
        .tools
        .iter_mut()
        .find(|tool| tool.tool_call_id == tool_call_id)
    else {
        message.tools.push(ChatReplayToolCall {
            tool_call_id,
            title: update
                .fields
                .title
                .unwrap_or_else(|| "Tool call".to_string()),
            status: update
                .fields
                .status
                .map(tool_call_status_str)
                .unwrap_or("pending")
                .to_string(),
            locations: update
                .fields
                .locations
                .unwrap_or_default()
                .into_iter()
                .map(to_chat_location)
                .collect(),
            content: update
                .fields
                .content
                .unwrap_or_default()
                .into_iter()
                .filter_map(to_chat_tool_content)
                .collect(),
            raw_input: update.fields.raw_input,
            raw_output: update.fields.raw_output,
        });
        return;
    };

    if let Some(title) = update.fields.title {
        tool.title = title;
    }
    if let Some(status) = update.fields.status {
        tool.status = tool_call_status_str(status).to_string();
    }
    if let Some(locations) = update.fields.locations {
        tool.locations = locations.into_iter().map(to_chat_location).collect();
    }
    if let Some(content) = update.fields.content {
        tool.content = content
            .into_iter()
            .filter_map(to_chat_tool_content)
            .collect();
    }
    if update.fields.raw_input.is_some() {
        tool.raw_input = update.fields.raw_input;
    }
    if update.fields.raw_output.is_some() {
        tool.raw_output = update.fields.raw_output;
    }
}

fn to_chat_location(loc: agent_client_protocol::schema::v1::ToolCallLocation) -> ChatLocation {
    ChatLocation {
        path: loc.path.display().to_string(),
        line: loc.line,
    }
}

fn to_chat_tool_content(
    content: agent_client_protocol::schema::v1::ToolCallContent,
) -> Option<ChatToolContent> {
    use agent_client_protocol::schema::v1::ToolCallContent;
    match content {
        ToolCallContent::Content(c) => match c.content {
            ContentBlock::Text(text) => Some(ChatToolContent::Text { text: text.text }),
            _ => None,
        },
        ToolCallContent::Diff(diff) => Some(ChatToolContent::Diff {
            path: diff.path.display().to_string(),
            old_text: diff.old_text,
            new_text: diff.new_text,
        }),
        ToolCallContent::Terminal(terminal) => Some(ChatToolContent::Terminal {
            terminal_id: terminal.terminal_id.0.to_string(),
        }),
        _ => None,
    }
}

fn config_category_str(category: SessionConfigOptionCategory) -> String {
    match category {
        SessionConfigOptionCategory::Mode => "mode".to_string(),
        SessionConfigOptionCategory::Model => "model".to_string(),
        SessionConfigOptionCategory::ModelConfig => "model_config".to_string(),
        SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
        SessionConfigOptionCategory::Other(s) => s,
        _ => "other".to_string(),
    }
}

/// Only the stable `select` kind is mapped; `boolean` is behind ACP's
/// `unstable_boolean_config` feature (not enabled) and any future kind is
/// covered by the wildcard -- both surface as an empty choice list rather
/// than being dropped, so the option's name/category still show up.
fn to_chat_config_option(option: SessionConfigOption) -> ChatConfigOption {
    let (current_value, choices) = match option.kind {
        SessionConfigKind::Select(select) => {
            let current_value = select.current_value.0.to_string();
            let choices = match select.options {
                SessionConfigSelectOptions::Ungrouped(opts) => opts
                    .into_iter()
                    .map(|o| ChatConfigChoice {
                        value: o.value.0.to_string(),
                        name: o.name,
                        group: None,
                    })
                    .collect(),
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .into_iter()
                    .flat_map(|group| {
                        let group_name = group.name;
                        group.options.into_iter().map(move |o| ChatConfigChoice {
                            value: o.value.0.to_string(),
                            name: o.name,
                            group: Some(group_name.clone()),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (current_value, choices)
        }
        _ => (String::new(), Vec::new()),
    };
    ChatConfigOption {
        id: option.id.0.to_string(),
        name: option.name,
        category: option.category.map(config_category_str),
        current_value,
        choices,
    }
}

/// True when a permission request targets Wilkes's own read-only MCP server.
/// The ACP request carries no dedicated tool-name field, so match the tool
/// identity from the human-readable title and the raw input the adapter
/// forwards. Claude Code and Codex currently spell MCP tool names differently
/// (`mcp__wilkes__search` vs. `mcp.wilkes.search`), so normalize both forms
/// through one matcher. A miss only costs one extra user prompt, never a wrong
/// auto-allow -- the failure mode is safe.
fn is_wilkes_mcp_call(tool_call: &ToolCallUpdate) -> bool {
    if let Some(title) = &tool_call.fields.title {
        if is_wilkes_mcp_tool_text(title, true) {
            return true;
        }
    }
    if let Some(raw_input) = &tool_call.fields.raw_input {
        if is_wilkes_mcp_tool_text(&raw_input.to_string(), false) {
            return true;
        }
    }
    false
}

fn is_wilkes_mcp_tool_text(text: &str, allow_bare_tool_name: bool) -> bool {
    let normalized = text.to_ascii_lowercase().replace("__", ".");
    let tokens: Vec<&str> = normalized
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter(|token| !token.is_empty())
        .collect();

    tokens.iter().any(|token| is_wilkes_mcp_tool_token(token))
        || (allow_bare_tool_name
            && tokens.len() == 1
            && crate::mcp::WILKES_MCP_TOOL_NAMES.contains(&tokens[0]))
}

fn is_wilkes_mcp_tool_token(token: &str) -> bool {
    crate::mcp::WILKES_MCP_TOOL_NAMES
        .iter()
        .any(|name| token == format!("wilkes.{name}") || token == format!("mcp.wilkes.{name}"))
}

/// Select an agent-offered allow option (once or always), if any. Used only to
/// auto-allow Wilkes's own MCP tools -- everything else defers to the user.
fn allow_outcome(options: &[PermissionOption]) -> Option<RequestPermissionOutcome> {
    options
        .iter()
        .find(|o| {
            matches!(
                o.kind,
                PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
            )
        })
        .map(|o| {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(o.option_id.clone()))
        })
}

fn to_chat_permission_option(option: &PermissionOption) -> ChatPermissionOption {
    ChatPermissionOption {
        option_id: option.option_id.0.to_string(),
        name: option.name.clone(),
        kind: permission_option_kind_str(option.kind),
    }
}

fn permission_option_kind_str(kind: PermissionOptionKind) -> String {
    match kind {
        PermissionOptionKind::AllowOnce => "allow_once",
        PermissionOptionKind::AllowAlways => "allow_always",
        PermissionOptionKind::RejectOnce => "reject_once",
        PermissionOptionKind::RejectAlways => "reject_always",
        _ => "other",
    }
    .to_string()
}

/// Resolve every parked permission request as "cancelled", used when the turn
/// ends or the session closes so no ACP request handler hangs holding a
/// responder.
fn drain_pending_permissions(pending: &PendingPermissions) {
    for (_, sender) in pending.lock().unwrap().drain() {
        let _ = sender.send(None);
    }
}

fn handle_read(
    state: &ContextStateHandle,
    request: &ReadTextFileRequest,
) -> Result<String, String> {
    if !state.is_allowed(&request.path) {
        return Err(format!(
            "{} is not in this chat's context",
            request.path.display()
        ));
    }
    crate::reader::read_text(&request.path, None, request.line, request.limit)
        .map_err(|e| e.to_string())
}

fn active_doc_text_for_context(doc: &ActiveDoc) -> ActiveDocText {
    match crate::reader::read_active_excerpt(
        Path::new(&doc.path),
        doc.page,
        ACTIVE_DOC_CONTEXT_CHAR_LIMIT,
    ) {
        Ok(excerpt) if excerpt.text.trim().is_empty() => ActiveDocText::Unavailable,
        Ok(excerpt) => ActiveDocText::Available {
            text: excerpt.text,
            truncated: excerpt.truncated,
        },
        Err(e) => {
            warn!(
                path = %doc.path,
                page = ?doc.page,
                "chat: failed to extract active document text for pushed context: {e:#}"
            );
            ActiveDocText::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::ToolCallUpdateFields;

    #[test]
    fn replay_user_text_drops_wilkes_context_only_chunk() {
        let mut messages = Vec::new();

        append_replay_user_text(
            &mut messages,
            "<wilkes-context>\nOpen document: none\n</wilkes-context>".to_string(),
        );

        assert!(messages.is_empty());
    }

    #[test]
    fn replay_user_text_strips_context_prefix_from_combined_chunk() {
        let mut messages = Vec::new();

        append_replay_user_text(
            &mut messages,
            "<wilkes-context>\nOpen document: none\n</wilkes-context>\n\nSummarize this"
                .to_string(),
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text, "Summarize this");
    }

    #[test]
    fn replay_user_text_keeps_normal_user_text() {
        let mut messages = Vec::new();

        append_replay_user_text(&mut messages, "What does the paper say?".to_string());

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "What does the paper say?");
    }

    #[test]
    fn wilkes_mcp_call_matches_codex_dotted_tool_name() {
        let tool_call = ToolCallUpdate::new(
            "tc_1",
            ToolCallUpdateFields::new().title("mcp.wilkes.search"),
        );

        assert!(is_wilkes_mcp_call(&tool_call));
    }

    #[test]
    fn wilkes_mcp_call_matches_claude_double_underscore_tool_name() {
        let tool_call = ToolCallUpdate::new(
            "tc_1",
            ToolCallUpdateFields::new().title("mcp__wilkes__get_document_text"),
        );

        assert!(is_wilkes_mcp_call(&tool_call));
    }

    #[test]
    fn wilkes_mcp_call_matches_raw_input_tool_name() {
        let tool_call = ToolCallUpdate::new(
            "tc_1",
            ToolCallUpdateFields::new().raw_input(serde_json::json!({
                "tool": "mcp.wilkes.list_context"
            })),
        );

        assert!(is_wilkes_mcp_call(&tool_call));
    }

    #[test]
    fn wilkes_mcp_call_rejects_non_wilkes_search_text() {
        let tool_call = ToolCallUpdate::new(
            "tc_1",
            ToolCallUpdateFields::new().raw_input(serde_json::json!({
                "query": "search the web"
            })),
        );

        assert!(!is_wilkes_mcp_call(&tool_call));
    }
}
