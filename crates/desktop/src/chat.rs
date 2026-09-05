//! The chat's session lifetime, and the events it puts on the window's bus.
//!
//! Here rather than in `wilkes_api` for the reason the rest of this crate is:
//! `wilkes_api` stays UI-framework-agnostic, and a session that outlives the
//! command that opened it is a fact about *this* shell -- the same split
//! `ActiveSearches` makes.
//!
//! Every command that opens a session or a turn takes a `host` argument: what
//! the window says the chat is currently about. It is applied before the call
//! it came with, and it *replaces* rather than amends. That is deliberate --
//! the window is the one place that knows which documents are in context, and
//! the alternative Wilkes used to run (`chat_add_context`, `chat_remove_context`,
//! `chat_set_active_doc` pushing into a live session) made the pane and the
//! session two copies of one fact, with a replay loop to keep them together
//! and nothing to notice when it did not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tracing::{error, info};
use wilkes_agent::WilkesChatHost;
use wilkes_api::commands::chat::{
    apply_chat_event, config_values_from_options, emission, is_resumable, upsert_backend_config,
    AgentBackend, BackendStatus, ChatConfigOption, ChatConversationRecord, ChatEmission, ChatEvent,
    ChatMessageRecord, ChatSession, ChatTurnEnvironment, HostContext,
};
use wilkes_api::context::AppContext;
use wilkes_api::context::EventEmitter;

use crate::{app_context, workspace_manager, TauriEmitter};

/// One open session: the subprocess, the Wilkes behind it, and the transcript
/// being written into it.
///
/// The transcript is kept here rather than read back off the store between
/// turns because a turn streams: the file is written once, when the turn ends,
/// and this is what it is written from.
pub struct ManagedChatSession {
    session: Arc<ChatSession>,
    /// The same `Arc` the session holds, so applying a context here is what
    /// the next prompt carries.
    host: Arc<WilkesChatHost>,
    conversation_id: Mutex<Option<String>>,
    cwd: PathBuf,
    messages: Mutex<Vec<ChatMessageRecord>>,
    /// A forked session has not been handed the dialogue above its branch
    /// point yet. Cleared by the first turn that carries it.
    branch_history_pending: Mutex<bool>,
}

#[derive(Default)]
pub struct ChatManager(Mutex<HashMap<String, Arc<ManagedChatSession>>>);

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

    /// Ends every subprocess. Called on window close: an ACP adapter is a
    /// child process, and one left running is a process the user cannot see
    /// and did not ask for.
    pub fn close_all(&self) {
        for (_, managed) in self.0.lock().unwrap().drain() {
            managed.session.close();
        }
    }
}

/// End every open session. Called on a workspace switch and on window close:
/// an ACP adapter is a child process, and one left running belongs to a
/// workspace that is no longer open.
pub(crate) fn close_all(app: &AppHandle) {
    manager(app).close_all();
}

fn manager(app: &AppHandle) -> Arc<ChatManager> {
    app.state::<Arc<ChatManager>>().inner().clone()
}

fn session_or_err(app: &AppHandle, session_id: &str) -> Result<Arc<ManagedChatSession>, String> {
    manager(app)
        .get(session_id)
        .ok_or_else(|| format!("this chat session is no longer open: {session_id}"))
}

#[derive(Debug, Serialize)]
pub struct ChatStartResult {
    session_id: String,
    conversation_id: Option<String>,
    backend_session_id: Option<String>,
    /// The agent's own session config (model, mode, thought level), if it has
    /// any. Later changes -- ours or its own -- arrive on
    /// `chat/config-<sessionId>`.
    config_options: Vec<ChatConfigOption>,
    messages: Vec<ChatMessageRecord>,
}

#[derive(Debug, Serialize)]
pub struct ChatSendResult {
    conversation_id: Option<String>,
}

/// Forward every event for the life of the session -- not one turn -- onto the
/// window's bus.
///
/// Which channel each event goes on is `wilkes_chat::wire`'s decision, not this
/// function's: a turn's updates are keyed by turn so a client subscribes for
/// the life of one message, and a crash is keyed by session because it can
/// arrive when no turn is running and would otherwise reach nobody.
fn forward_events(
    app: AppHandle,
    session_id: String,
    managed: Arc<ManagedChatSession>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ChatEvent>,
) {
    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        while let Some(event) = events.recv().await {
            // Recorded before it is emitted, so a window that closes mid-turn
            // has already lost nothing the file will not have.
            apply_chat_event(&mut managed.messages.lock().unwrap(), &event);

            // `EventEmitter` carries JSON, so each payload is serialized on
            // the way out. What it serializes to is the wire's decision, not
            // this function's -- these are the crate's own types.
            match emission(event) {
                ChatEmission::Turn { turn_id, update } => emitter.emit(
                    &format!("chat/update-{turn_id}"),
                    serde_json::to_value(update).unwrap_or_default(),
                ),
                ChatEmission::SessionError { message } => {
                    error!("chat session {session_id}: {message}");
                    emitter.emit(
                        &format!("chat/session-error-{session_id}"),
                        serde_json::json!({ "message": message }),
                    )
                }
                ChatEmission::ConfigOptions { options } => emitter.emit(
                    &format!("chat/config-{session_id}"),
                    serde_json::to_value(options).unwrap_or_default(),
                ),
            }
        }
    });
}

/// Record the conversation, if this backend's chats can be resumed at all.
///
/// Deferred to the first turn rather than done at session start: an agent
/// opened and never spoken to would otherwise leave an empty conversation in
/// the history menu every time the pane was shown.
fn ensure_conversation(
    ctx: &AppContext,
    managed: &ManagedChatSession,
) -> Result<Option<String>, String> {
    if !is_resumable(managed.session.backend) {
        return Ok(None);
    }
    let config_options = managed.session.config_options();
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
    )
    .map_err(|e| e.to_string())?;
    *conversation_id = record.map(|record| record.conversation_id);
    Ok(conversation_id.clone())
}

/// Register an opened session and start forwarding its events.
fn adopt(
    app: &AppHandle,
    session: Arc<ChatSession>,
    host: Arc<WilkesChatHost>,
    events: tokio::sync::mpsc::UnboundedReceiver<ChatEvent>,
    cwd: PathBuf,
    conversation_id: Option<String>,
    messages: Vec<ChatMessageRecord>,
    branch_history_pending: bool,
) -> String {
    let session_id = uuid::Uuid::new_v4().to_string();
    let managed = Arc::new(ManagedChatSession {
        session,
        host,
        conversation_id: Mutex::new(conversation_id),
        cwd,
        messages: Mutex::new(messages),
        branch_history_pending: Mutex::new(branch_history_pending),
    });
    forward_events(
        app.clone(),
        session_id.clone(),
        Arc::clone(&managed),
        events,
    );
    manager(app).insert(session_id.clone(), managed);
    session_id
}

/// Hand a freshly opened session the user's standing instructions and the
/// configuration this agent was last used with.
///
/// Both come from settings and neither is the chat's own: the instructions are
/// the user's text, and the config is what they chose the last time they talked
/// to this agent. Returns the configuration the agent actually ended up with,
/// which is not always what was asked for -- a model can be retired between one
/// chat and the next.
async fn apply_settings(
    ctx: &AppContext,
    session: &ChatSession,
    stored: &[wilkes_api::commands::chat::ChatConfigValue],
) -> Vec<ChatConfigOption> {
    session.set_instructions(Some(ctx.get_settings().await.chat_custom_instructions));
    session.apply_config(stored).await
}

#[tauri::command]
pub async fn chat_list_backends(refresh: bool, _app: AppHandle) -> Vec<BackendStatus> {
    wilkes_api::commands::chat::list_backends(refresh)
}

#[tauri::command]
pub async fn chat_install_backend(
    backend: AgentBackend,
    _app: AppHandle,
) -> Result<BackendStatus, String> {
    wilkes_api::commands::chat::install_backend(backend)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn chat_list_conversations(
    app: AppHandle,
) -> Result<Vec<ChatConversationRecord>, String> {
    wilkes_api::commands::chat::list_conversations(&app_context(&app).data_dir)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn chat_forget_conversation(
    conversation_id: String,
    app: AppHandle,
) -> Result<(), String> {
    wilkes_api::commands::chat::forget_conversation(&app_context(&app).data_dir, &conversation_id)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn chat_start(
    backend: AgentBackend,
    host: Option<HostContext>,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let integrations = ctx.get_settings().await.integrations;
    let (spawned, wilkes) = wilkes_api::commands::chat::start(
        backend,
        cwd.clone(),
        Some(workspace_manager(&app)),
        integrations,
    )
    .await
    .map_err(|e| format!("{e:#}"))?;
    wilkes.apply(host.unwrap_or_default());

    let session = Arc::new(spawned.session);
    let stored = wilkes_api::commands::chat::config_for_backend(
        &ctx.get_settings().await.chat_config,
        backend,
    )
    .to_vec();
    let config_options = apply_settings(&ctx, &session, &stored).await;
    let backend_session_id = session.backend_session_id().to_string();

    let session_id = adopt(
        &app,
        session,
        wilkes,
        spawned.events,
        cwd,
        None,
        Vec::new(),
        false,
    );

    Ok(ChatStartResult {
        session_id,
        conversation_id: None,
        backend_session_id: Some(backend_session_id),
        config_options,
        messages: Vec::new(),
    })
}

#[tauri::command]
pub async fn chat_open_conversation(
    conversation_id: String,
    host: Option<HostContext>,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let record = wilkes_api::commands::chat::get_conversation(&ctx.data_dir, &conversation_id)
        .map_err(|e| format!("{e:#}"))?;
    let integrations = ctx.get_settings().await.integrations;
    let (spawned, wilkes) =
        wilkes_api::commands::chat::open(&record, Some(workspace_manager(&app)), integrations)
            .await
            .map_err(|e| format!("{e:#}"))?;
    wilkes.apply(host.unwrap_or_default());

    let session = Arc::new(spawned.session);
    // The agent's replay is the authority on what was said, not the file: the
    // file's copy can be short by whatever a crash cost it, and two accounts
    // of one conversation is the state worth not having.
    let messages = wilkes_api::commands::chat::records_from_replay(spawned.replay_messages);
    let config_options = apply_settings(&ctx, &session, &record.config_values).await;
    let backend_session_id = session.backend_session_id().to_string();

    if record.branch_history_pending {
        session.set_prelude(Some(wilkes_api::commands::chat::branch_history_text(
            &record.messages,
        )));
    }
    if let Err(e) = wilkes_api::commands::chat::replace_conversation_messages(
        &ctx.data_dir,
        &conversation_id,
        messages.clone(),
    ) {
        error!("chat: could not write back the replayed transcript: {e:#}");
    }
    if let Err(e) =
        wilkes_api::commands::chat::touch_conversation(&ctx.data_dir, &conversation_id, None)
    {
        error!("chat: could not mark the conversation as opened: {e:#}");
    }

    let session_id = adopt(
        &app,
        session,
        wilkes,
        spawned.events,
        PathBuf::from(&record.cwd),
        Some(conversation_id.clone()),
        messages.clone(),
        record.branch_history_pending,
    );

    Ok(ChatStartResult {
        session_id,
        conversation_id: Some(conversation_id),
        backend_session_id: Some(backend_session_id),
        config_options,
        messages,
    })
}

#[tauri::command]
pub async fn chat_fork_conversation(
    conversation_id: String,
    message_id: String,
    include_message: bool,
    host: Option<HostContext>,
    app: AppHandle,
) -> Result<ChatStartResult, String> {
    let ctx = app_context(&app);
    let source = wilkes_api::commands::chat::get_conversation(&ctx.data_dir, &conversation_id)
        .map_err(|e| format!("{e:#}"))?;
    if !is_resumable(source.backend) {
        return Err("This chat backend does not support durable forks".to_string());
    }
    let environment = wilkes_api::commands::chat::environment_at_message(&source, &message_id);
    let integrations = ctx.get_settings().await.integrations;

    // A *new* session, not a resume: the point of a branch is to go where the
    // original did not, and an agent reattached to its own last state still
    // has the abandoned continuation in its context.
    let cwd = PathBuf::from(&source.cwd);
    let (spawned, wilkes) = wilkes_api::commands::chat::start(
        source.backend,
        cwd.clone(),
        Some(workspace_manager(&app)),
        integrations,
    )
    .await
    .map_err(|e| format!("{e:#}"))?;
    // The window has already put itself back into the state this message was
    // asked in, so what arrives here is that state and not today's.
    wilkes.apply(host.unwrap_or_default());

    let session = Arc::new(spawned.session);
    let config_options = apply_settings(&ctx, &session, &environment.config_values).await;
    let backend_session_id = session.backend_session_id().to_string();

    let record = match wilkes_api::commands::chat::create_fork_conversation(
        &ctx.data_dir,
        &conversation_id,
        &message_id,
        include_message,
        backend_session_id.clone(),
    ) {
        Ok(record) => record,
        Err(e) => {
            // The agent is already running and now belongs to nothing.
            session.close();
            return Err(format!("{e:#}"));
        }
    };

    if record.branch_history_pending {
        session.set_prelude(Some(wilkes_api::commands::chat::branch_history_text(
            &record.messages,
        )));
    }

    let session_id = adopt(
        &app,
        session,
        wilkes,
        spawned.events,
        cwd,
        Some(record.conversation_id.clone()),
        record.messages.clone(),
        record.branch_history_pending,
    );

    Ok(ChatStartResult {
        session_id,
        conversation_id: Some(record.conversation_id),
        backend_session_id: Some(backend_session_id),
        config_options,
        messages: record.messages,
    })
}

#[tauri::command]
pub async fn chat_close(session_id: String, app: AppHandle) -> Result<(), String> {
    if let Some(managed) = manager(&app).remove(&session_id) {
        // Cancel before close: a turn in flight is a subprocess waiting on a
        // response, and closing the command loop under it would leave the
        // child to be reaped rather than to exit.
        let _ = managed.session.cancel();
        managed.session.close();
    }
    Ok(())
}

#[tauri::command]
pub async fn chat_set_config_option(
    session_id: String,
    config_id: String,
    value: String,
    app: AppHandle,
) -> Result<Vec<ChatConfigOption>, String> {
    let ctx = app_context(&app);
    let managed = session_or_err(&app, &session_id)?;
    let options = managed
        .session
        .set_config_option(config_id, value)
        .await
        .map_err(|e| format!("{e:#}"))?;

    let conversation_id = managed.conversation_id.lock().unwrap().clone();
    if let Some(conversation_id) = conversation_id {
        if let Err(e) = wilkes_api::commands::chat::update_conversation_config(
            &ctx.data_dir,
            &conversation_id,
            &options,
        ) {
            error!("chat: could not record the config change: {e:#}");
        }
    }
    // ...and remembered as this agent's default, so the *next* new chat with
    // it starts here rather than at the agent's own defaults. Two writes for
    // one choice, deliberately: the record above says what answered this
    // conversation, and this says what to open the next one with.
    let chat_config = upsert_backend_config(
        ctx.get_settings().await.chat_config,
        managed.session.backend,
        config_values_from_options(&options),
    );
    ctx.update_settings(serde_json::json!({ "chat_config": chat_config }))
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(options)
}

#[tauri::command]
pub async fn chat_send(
    session_id: String,
    turn_id: String,
    user_message_id: String,
    text: String,
    host: Option<HostContext>,
    app: AppHandle,
) -> Result<ChatSendResult, String> {
    let ctx = app_context(&app);
    let managed = session_or_err(&app, &session_id)?;
    managed.host.apply(host.unwrap_or_default());
    // Read at send time so an edit in Settings reaches conversations that are
    // already open, not only sessions started afterwards.
    managed
        .session
        .set_instructions(Some(ctx.get_settings().await.chat_custom_instructions));
    let conversation_id = ensure_conversation(&ctx, &managed)?;

    {
        // The conditions this turn is being sent under, so a branch taken from
        // it later reopens on the documents it was asked about and the model
        // that answered, rather than on today's. Read back off the host rather
        // than from the argument, so what is recorded is the state that
        // actually answered.
        let environment = ChatTurnEnvironment {
            config_values: config_values_from_options(&managed.session.config_options()),
            host: serde_json::to_value(managed.host.context()).ok(),
        };
        let mut messages = managed.messages.lock().unwrap();
        messages.push(ChatMessageRecord::user_in(
            user_message_id,
            turn_id.clone(),
            text.clone(),
            environment,
        ));
        messages.push(ChatMessageRecord::assistant_placeholder(turn_id.clone()));
    }

    let session = Arc::clone(&managed.session);
    let managed_for_task = Arc::clone(&managed);
    let task_conversation_id = conversation_id.clone();
    let title_hint = text.clone();
    let data_dir = ctx.data_dir.clone();

    // The turn runs detached and the command answers now. It has to: a turn
    // can take minutes, and the client needs the conversation id to key its
    // history long before the agent has finished talking. The end of the turn
    // reaches the client as `chat/done-<turn>`.
    tokio::spawn(async move {
        let emitter = TauriEmitter(app);
        let result = session.send(turn_id.clone(), text).await;

        let payload = match result {
            Ok(stop_reason) => serde_json::json!({ "stop_reason": stop_reason }),
            Err(e) => {
                let message = format!("{e:#}");
                error!("chat turn {turn_id} failed: {message}");
                if let Some(last) = managed_for_task
                    .messages
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|m| m.message_id == turn_id)
                {
                    last.error = Some(message.clone());
                }
                // Reported on the turn's own channel, in the same union as
                // every other update, so the client learns of the failure
                // where it is already listening rather than in a second shape.
                emitter.emit(
                    &format!("chat/update-{turn_id}"),
                    serde_json::json!({ "kind": "error", "message": message }),
                );
                serde_json::json!({ "stop_reason": null })
            }
        };

        if let Some(conversation_id) = &task_conversation_id {
            // The prelude rode on this turn and is spent. Recorded on disk as
            // well as in memory, so a window reopened on this branch does not
            // hand the agent its own history a second time.
            let carried = {
                let mut pending = managed_for_task.branch_history_pending.lock().unwrap();
                std::mem::replace(&mut *pending, false)
            };
            if carried {
                if let Err(e) = wilkes_api::commands::chat::mark_branch_history_seeded(
                    &data_dir,
                    conversation_id,
                ) {
                    error!("chat: could not mark the branch history as handed over: {e:#}");
                }
            }

            let messages = managed_for_task.messages.lock().unwrap().clone();
            if let Err(e) = wilkes_api::commands::chat::replace_conversation_messages(
                &data_dir,
                conversation_id,
                messages,
            ) {
                error!("chat: could not save the transcript: {e:#}");
            }
            if let Err(e) = wilkes_api::commands::chat::touch_conversation(
                &data_dir,
                conversation_id,
                Some(&title_hint),
            ) {
                error!("chat: could not update the conversation's metadata: {e:#}");
            }
        }

        emitter.emit(&format!("chat/done-{turn_id}"), payload);
    });

    Ok(ChatSendResult { conversation_id })
}

#[tauri::command]
pub async fn chat_cancel(
    session_id: String,
    turn_id: String,
    app: AppHandle,
) -> Result<(), String> {
    info!("chat_cancel: session={session_id} turn={turn_id}");
    // One turn runs at a time, so cancelling the session cancels this turn.
    // `turn_id` is taken to keep the client's call self-describing and to
    // leave room for a session that runs more than one.
    session_or_err(&app, &session_id)?
        .session
        .cancel()
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn chat_answer_permission(
    session_id: String,
    request_id: String,
    option_id: Option<String>,
    app: AppHandle,
) -> Result<(), String> {
    session_or_err(&app, &session_id)?
        .session
        .answer_permission(&request_id, option_id);
    Ok(())
}
