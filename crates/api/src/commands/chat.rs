//! Tauri-facing verbs for the "Ask the documents" chat pane. Thin: session
//! lifetime and event forwarding are owned by the desktop layer (mirroring
//! how `ActiveSearches` owns search lifetime, not this crate) -- this module
//! only knows how to list/start backends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use wilkes_core::types::{AgentBackend, ChatBackendConfig, IntegrationsSettings};

pub use wilkes_agent::session::{
    ChatConfigOption, ChatEvent, ChatReplayContentBlock, ChatReplayMessage, ChatReplayToolCall,
    ChatSession, SpawnedChatSession,
};

/// A persisted config selection. Aliases the canonical core type so the
/// per-conversation snapshot and the per-backend default in `Settings` share
/// one shape rather than a second, parallel representation.
pub use wilkes_core::types::ChatConfigValue as ChatConfigValueRecord;

#[derive(Debug, Clone, Serialize)]
pub struct BackendStatus {
    pub backend: AgentBackend,
    pub label: String,
    pub available: bool,
    pub auth_note: String,
    pub installable: bool,
    pub unavailable_reason: Option<String>,
}

/// Availability of supported backends, for the agent selector and the header
/// split-button dropdown (spec §7.1, §7.3). Never filters unavailable
/// backends out -- the caller shows them disabled with `auth_note`.
pub fn list_backends(refresh: bool) -> Vec<BackendStatus> {
    [
        AgentBackend::ClaudeCode,
        AgentBackend::Codex,
        AgentBackend::Nanocoder,
    ]
    .into_iter()
    .map(|backend| backend_status(backend, refresh))
    .collect()
}

pub fn backend_status(backend: AgentBackend, refresh: bool) -> BackendStatus {
    let availability = wilkes_agent::probe_backend_availability(backend, refresh);
    BackendStatus {
        backend,
        label: wilkes_agent::label(backend).to_string(),
        available: availability.available,
        auth_note: wilkes_agent::auth_note(backend).to_string(),
        installable: availability.installable,
        unavailable_reason: availability.unavailable_reason,
    }
}

pub async fn install_backend(backend: AgentBackend) -> anyhow::Result<BackendStatus> {
    wilkes_agent::install_backend_adapter(backend).await?;
    Ok(backend_status(backend, false))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatContextFileRecord {
    pub path: String,
    pub pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatActiveDocRecord {
    pub path: String,
    pub page: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurnEnvironmentRecord {
    pub context_files: Vec<ChatContextFileRecord>,
    pub active_doc: Option<ChatActiveDocRecord>,
    pub search_root: Option<String>,
    pub config_values: Vec<ChatConfigValueRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessageRecord {
    pub message_id: String,
    pub turn_id: Option<String>,
    pub role: String,
    pub thought: String,
    pub content: Vec<ChatReplayContentBlock>,
    pub error: Option<String>,
    pub environment: Option<ChatTurnEnvironmentRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConversationRecord {
    pub conversation_id: String,
    pub backend: AgentBackend,
    pub backend_session_id: String,
    pub cwd: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: String,
    pub context_files: Vec<ChatContextFileRecord>,
    pub active_doc: Option<ChatActiveDocRecord>,
    pub config_values: Vec<ChatConfigValueRecord>,
    pub messages: Vec<ChatMessageRecord>,
    pub parent_conversation_id: Option<String>,
    pub forked_from_message_id: Option<String>,
    pub branch_history_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatConversationFile {
    version: u32,
    conversations: Vec<ChatConversationRecord>,
}

impl Default for ChatConversationFile {
    fn default() -> Self {
        Self {
            version: 2,
            conversations: Vec::new(),
        }
    }
}

pub fn is_durable_backend(backend: AgentBackend) -> bool {
    matches!(backend, AgentBackend::ClaudeCode | AgentBackend::Codex)
}

pub fn list_conversations(data_dir: &Path) -> anyhow::Result<Vec<ChatConversationRecord>> {
    let mut store = read_store(data_dir)?;
    store
        .conversations
        .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(store.conversations)
}

pub fn get_conversation(
    data_dir: &Path,
    conversation_id: &str,
) -> anyhow::Result<ChatConversationRecord> {
    read_store(data_dir)?
        .conversations
        .into_iter()
        .find(|conversation| conversation.conversation_id == conversation_id)
        .ok_or_else(|| anyhow::anyhow!("chat conversation not found: {conversation_id}"))
}

pub fn create_conversation(
    data_dir: &Path,
    backend: AgentBackend,
    cwd: &Path,
    backend_session_id: String,
    config_options: &[ChatConfigOption],
    context_files: Vec<ChatContextFileRecord>,
    active_doc: Option<ChatActiveDocRecord>,
) -> anyhow::Result<Option<ChatConversationRecord>> {
    if !is_durable_backend(backend) {
        return Ok(None);
    }

    let now = now_string();
    let record = ChatConversationRecord {
        conversation_id: uuid::Uuid::new_v4().to_string(),
        backend,
        backend_session_id,
        cwd: cwd.display().to_string(),
        title: format!("New {} chat", wilkes_agent::label(backend)),
        created_at: now.clone(),
        updated_at: now.clone(),
        last_opened_at: now,
        context_files,
        active_doc,
        config_values: config_values_from_options(config_options),
        messages: Vec::new(),
        parent_conversation_id: None,
        forked_from_message_id: None,
        branch_history_pending: false,
    };

    mutate_store(data_dir, |store| store.conversations.push(record.clone()))?;
    Ok(Some(record))
}

pub fn touch_conversation(
    data_dir: &Path,
    conversation_id: &str,
    title_hint: Option<&str>,
) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        if let Some(record) = store
            .conversations
            .iter_mut()
            .find(|record| record.conversation_id == conversation_id)
        {
            let now = now_string();
            record.updated_at = now.clone();
            record.last_opened_at = now;
            if let Some(title_hint) = title_hint {
                if record.title.starts_with("New ") {
                    record.title = title_from_text(title_hint);
                }
            }
        }
    })
}

pub fn forget_conversation(data_dir: &Path, conversation_id: &str) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        store
            .conversations
            .retain(|record| record.conversation_id != conversation_id);
    })
}

pub fn update_conversation_context(
    data_dir: &Path,
    conversation_id: &str,
    context_files: Option<Vec<ChatContextFileRecord>>,
    active_doc: Option<Option<ChatActiveDocRecord>>,
) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        if let Some(record) = store
            .conversations
            .iter_mut()
            .find(|record| record.conversation_id == conversation_id)
        {
            record.updated_at = now_string();
            if let Some(context_files) = context_files {
                record.context_files = context_files;
            }
            if let Some(active_doc) = active_doc {
                record.active_doc = active_doc;
            }
        }
    })
}

pub fn update_conversation_config(
    data_dir: &Path,
    conversation_id: &str,
    config_options: &[ChatConfigOption],
) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        if let Some(record) = store
            .conversations
            .iter_mut()
            .find(|record| record.conversation_id == conversation_id)
        {
            record.updated_at = now_string();
            record.config_values = config_values_from_options(config_options);
        }
    })
}

pub fn replace_conversation_messages(
    data_dir: &Path,
    conversation_id: &str,
    messages: Vec<ChatMessageRecord>,
) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        if let Some(record) = store
            .conversations
            .iter_mut()
            .find(|record| record.conversation_id == conversation_id)
        {
            record.updated_at = now_string();
            record.messages = messages;
        }
    })
}

pub fn mark_branch_history_seeded(data_dir: &Path, conversation_id: &str) -> anyhow::Result<()> {
    mutate_store(data_dir, |store| {
        if let Some(record) = store
            .conversations
            .iter_mut()
            .find(|record| record.conversation_id == conversation_id)
        {
            record.branch_history_pending = false;
            record.updated_at = now_string();
        }
    })
}

pub fn create_fork_conversation(
    data_dir: &Path,
    source_conversation_id: &str,
    forked_from_message_id: &str,
    include_message: bool,
    backend_session_id: String,
) -> anyhow::Result<ChatConversationRecord> {
    let source = get_conversation(data_dir, source_conversation_id)?;
    let index = source
        .messages
        .iter()
        .position(|message| message.message_id == forked_from_message_id)
        .ok_or_else(|| anyhow::anyhow!("chat message not found: {forked_from_message_id}"))?;
    let prefix_len = if include_message { index + 1 } else { index };
    let environment = environment_at_message(&source, forked_from_message_id)?;
    let now = now_string();
    let record = ChatConversationRecord {
        conversation_id: uuid::Uuid::new_v4().to_string(),
        backend: source.backend,
        backend_session_id,
        cwd: source.cwd,
        title: format!("Fork of {}", source.title),
        created_at: now.clone(),
        updated_at: now.clone(),
        last_opened_at: now,
        context_files: environment.context_files,
        active_doc: environment.active_doc,
        config_values: environment.config_values,
        messages: source.messages[..prefix_len].to_vec(),
        parent_conversation_id: Some(source_conversation_id.to_string()),
        forked_from_message_id: Some(forked_from_message_id.to_string()),
        branch_history_pending: prefix_len > 0,
    };
    mutate_store(data_dir, |store| store.conversations.push(record.clone()))?;
    Ok(record)
}

pub fn environment_at_message(
    conversation: &ChatConversationRecord,
    message_id: &str,
) -> anyhow::Result<ChatTurnEnvironmentRecord> {
    let index = conversation
        .messages
        .iter()
        .position(|message| message.message_id == message_id)
        .ok_or_else(|| anyhow::anyhow!("chat message not found: {message_id}"))?;
    conversation.messages[..=index]
        .iter()
        .rev()
        .find_map(|message| message.environment.clone())
        .ok_or_else(|| anyhow::anyhow!("chat message has no turn environment: {message_id}"))
}

pub fn branch_history_text(messages: &[ChatMessageRecord]) -> String {
    messages
        .iter()
        .filter_map(|message| {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ChatReplayContentBlock::Text { text } => Some(text.as_str()),
                    ChatReplayContentBlock::Tool { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.trim().is_empty() {
                None
            } else {
                let speaker = if message.role == "user" {
                    "User"
                } else {
                    "Assistant"
                };
                Some(format!("{speaker}: {text}"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Spawn a subprocess and complete the ACP handshake for `backend`.
pub async fn start(
    backend: AgentBackend,
    cwd: PathBuf,
    search: Option<Arc<dyn wilkes_agent::search::SearchService>>,
    integrations: IntegrationsSettings,
) -> anyhow::Result<SpawnedChatSession> {
    wilkes_agent::session::spawn_with_services(backend, cwd, search, integrations).await
}

pub async fn open(
    record: &ChatConversationRecord,
    search: Option<Arc<dyn wilkes_agent::search::SearchService>>,
    integrations: IntegrationsSettings,
) -> anyhow::Result<SpawnedChatSession> {
    wilkes_agent::session::load_with_services(
        record.backend,
        PathBuf::from(&record.cwd),
        record.backend_session_id.clone(),
        search,
        integrations,
    )
    .await
}

fn conversation_path(data_dir: &Path) -> PathBuf {
    data_dir.join("chat-conversations.json")
}

fn read_store(data_dir: &Path) -> anyhow::Result<ChatConversationFile> {
    let path = conversation_path(data_dir);
    if !path.exists() {
        return Ok(ChatConversationFile::default());
    }
    let text = std::fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(ChatConversationFile::default());
    }
    let store: ChatConversationFile = serde_json::from_str(&text)?;
    anyhow::ensure!(
        store.version == 2,
        "unsupported chat conversation version: {}",
        store.version
    );
    Ok(store)
}

fn write_store(data_dir: &Path, store: &ChatConversationFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let path = conversation_path(data_dir);
    let tmp_path = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(store)?;
    std::fs::write(&tmp_path, text)?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

fn mutate_store<F>(data_dir: &Path, f: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut ChatConversationFile),
{
    let mut store = read_store(data_dir)?;
    f(&mut store);
    write_store(data_dir, &store)
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}

fn title_from_text(text: &str) -> String {
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = trimmed.chars().take(80).collect();
    if title.is_empty() {
        title = "Untitled chat".to_string();
    }
    title
}

pub fn config_values_from_options(options: &[ChatConfigOption]) -> Vec<ChatConfigValueRecord> {
    options
        .iter()
        .map(|option| ChatConfigValueRecord {
            id: option.id.clone(),
            value: option.current_value.clone(),
        })
        .collect()
}

/// Apply persisted config values to a freshly started session, best-effort.
///
/// A saved value may reference a model/mode the agent no longer offers (it was
/// upgraded, or its choices changed); we log and skip such a value rather than
/// aborting session start over a stale preference. Only values that differ from
/// what the agent already reports are sent, so restoring the agent's own
/// default is a no-op. Returns the agent's final option list.
pub async fn apply_config(
    session: &ChatSession,
    values: &[ChatConfigValueRecord],
) -> Vec<ChatConfigOption> {
    for value in values {
        let differs = session
            .config_options()
            .iter()
            .find(|option| option.id == value.id)
            .map(|option| option.current_value != value.value)
            .unwrap_or(false);
        if !differs {
            continue;
        }
        if let Err(error) = session
            .set_config_option(value.id.clone(), value.value.clone())
            .await
        {
            tracing::warn!(
                "chat: could not restore saved config {}={} for {:?}: {error}",
                value.id,
                value.value,
                session.backend,
            );
        }
    }
    session.config_options()
}

/// Replace (or insert) the persisted per-backend chat config defaults for
/// `backend`, returning the updated list to write back into `Settings`.
pub fn upsert_backend_config(
    mut existing: Vec<ChatBackendConfig>,
    backend: AgentBackend,
    values: Vec<ChatConfigValueRecord>,
) -> Vec<ChatBackendConfig> {
    if let Some(entry) = existing.iter_mut().find(|entry| entry.backend == backend) {
        entry.values = values;
    } else {
        existing.push(ChatBackendConfig { backend, values });
    }
    existing
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(id: &str, v: &str) -> ChatConfigValueRecord {
        ChatConfigValueRecord {
            id: id.to_string(),
            value: v.to_string(),
        }
    }

    #[test]
    fn upsert_backend_config_inserts_then_replaces() {
        let config = upsert_backend_config(
            Vec::new(),
            AgentBackend::ClaudeCode,
            vec![value("model", "sonnet")],
        );
        assert_eq!(config.len(), 1);
        assert_eq!(config[0].backend, AgentBackend::ClaudeCode);
        assert_eq!(config[0].values, vec![value("model", "sonnet")]);

        // A second backend is appended, not merged into the first.
        let config = upsert_backend_config(config, AgentBackend::Codex, vec![value("model", "o3")]);
        assert_eq!(config.len(), 2);

        // Re-writing an existing backend replaces its values in place.
        let config = upsert_backend_config(
            config,
            AgentBackend::ClaudeCode,
            vec![value("model", "opus"), value("thought", "high")],
        );
        assert_eq!(config.len(), 2);
        let claude = config
            .iter()
            .find(|entry| entry.backend == AgentBackend::ClaudeCode)
            .unwrap();
        assert_eq!(
            claude.values,
            vec![value("model", "opus"), value("thought", "high")]
        );
    }

    #[test]
    fn lists_supported_backends() {
        let backends = list_backends(false);
        assert_eq!(backends.len(), 3);
        assert!(backends
            .iter()
            .any(|b| b.backend == AgentBackend::ClaudeCode));
        assert!(backends.iter().any(|b| b.backend == AgentBackend::Codex));
        assert!(backends
            .iter()
            .any(|b| b.backend == AgentBackend::Nanocoder));
        assert!(backends
            .iter()
            .all(|b| !b.label.is_empty() && !b.auth_note.is_empty()));
        // Availability/installability depend on the local toolchain and npx
        // cache, so assert only the structural invariant: a backend is never
        // both available and still offering a pre-warm.
        assert!(backends.iter().all(|b| !(b.available && b.installable)));
    }

    #[test]
    fn persists_only_durable_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("workspace");

        let claude = create_conversation(
            dir.path(),
            AgentBackend::ClaudeCode,
            &cwd,
            "claude-session".to_string(),
            &[],
            Vec::new(),
            None,
        )
        .unwrap();
        let nanocoder = create_conversation(
            dir.path(),
            AgentBackend::Nanocoder,
            &cwd,
            "nanocoder-session".to_string(),
            &[],
            Vec::new(),
            None,
        )
        .unwrap();

        assert!(claude.is_some());
        assert!(nanocoder.is_none());
        let conversations = list_conversations(dir.path()).unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].backend, AgentBackend::ClaudeCode);
        assert_eq!(conversations[0].backend_session_id, "claude-session");
    }

    #[test]
    fn updates_and_forgets_conversation_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let record = create_conversation(
            dir.path(),
            AgentBackend::Codex,
            dir.path(),
            "codex-session".to_string(),
            &[],
            Vec::new(),
            None,
        )
        .unwrap()
        .unwrap();

        update_conversation_context(
            dir.path(),
            &record.conversation_id,
            Some(vec![ChatContextFileRecord {
                path: "/tmp/a.pdf".to_string(),
                pages: Some(3),
            }]),
            Some(Some(ChatActiveDocRecord {
                path: "/tmp/a.pdf".to_string(),
                page: Some(2),
            })),
        )
        .unwrap();
        touch_conversation(
            dir.path(),
            &record.conversation_id,
            Some("What does the introduction say about the method?"),
        )
        .unwrap();

        let updated = get_conversation(dir.path(), &record.conversation_id).unwrap();
        assert_eq!(updated.context_files.len(), 1);
        assert_eq!(updated.active_doc.unwrap().page, Some(2));
        assert_eq!(
            updated.title,
            "What does the introduction say about the method?"
        );

        forget_conversation(dir.path(), &record.conversation_id).unwrap();
        assert!(list_conversations(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn creates_conversation_with_initial_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let record = create_conversation(
            dir.path(),
            AgentBackend::ClaudeCode,
            dir.path(),
            "claude-session".to_string(),
            &[],
            vec![ChatContextFileRecord {
                path: "/tmp/paper.pdf".to_string(),
                pages: Some(12),
            }],
            Some(ChatActiveDocRecord {
                path: "/tmp/paper.pdf".to_string(),
                page: Some(5),
            }),
        )
        .unwrap()
        .unwrap();

        let persisted = get_conversation(dir.path(), &record.conversation_id).unwrap();
        assert_eq!(persisted.context_files.len(), 1);
        assert_eq!(persisted.context_files[0].path, "/tmp/paper.pdf");
        assert_eq!(persisted.active_doc.unwrap().page, Some(5));
    }

    fn text_message(id: &str, turn: &str, role: &str, text: &str) -> ChatMessageRecord {
        ChatMessageRecord {
            message_id: id.to_string(),
            turn_id: Some(turn.to_string()),
            role: role.to_string(),
            thought: "private reasoning is not branch context".to_string(),
            content: vec![ChatReplayContentBlock::Text {
                text: text.to_string(),
            }],
            error: None,
            environment: if role == "user" {
                Some(ChatTurnEnvironmentRecord {
                    context_files: Vec::new(),
                    active_doc: None,
                    search_root: Some("/tmp/original-root".to_string()),
                    config_values: Vec::new(),
                })
            } else {
                None
            },
        }
    }

    #[test]
    fn forks_an_immutable_message_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let source = create_conversation(
            dir.path(),
            AgentBackend::Codex,
            dir.path(),
            "source-session".to_string(),
            &[],
            Vec::new(),
            None,
        )
        .unwrap()
        .unwrap();
        replace_conversation_messages(
            dir.path(),
            &source.conversation_id,
            vec![
                text_message("user-1", "turn-1", "user", "Original prompt"),
                text_message("assistant-1", "turn-1", "assistant", "Original answer"),
            ],
        )
        .unwrap();

        let fork = create_fork_conversation(
            dir.path(),
            &source.conversation_id,
            "assistant-1",
            true,
            "fork-session".to_string(),
        )
        .unwrap();

        assert_eq!(fork.messages.len(), 2);
        assert_eq!(
            fork.parent_conversation_id.as_deref(),
            Some(source.conversation_id.as_str())
        );
        assert_eq!(fork.forked_from_message_id.as_deref(), Some("assistant-1"));
        assert!(fork.branch_history_pending);
        assert_eq!(
            get_conversation(dir.path(), &source.conversation_id)
                .unwrap()
                .messages
                .len(),
            2
        );
    }

    #[test]
    fn edit_prefix_excludes_the_selected_user_message() {
        let dir = tempfile::tempdir().unwrap();
        let source = create_conversation(
            dir.path(),
            AgentBackend::ClaudeCode,
            dir.path(),
            "source-session".to_string(),
            &[],
            Vec::new(),
            None,
        )
        .unwrap()
        .unwrap();
        let messages = vec![
            text_message("user-1", "turn-1", "user", "First"),
            text_message("assistant-1", "turn-1", "assistant", "Answer"),
            text_message("user-2", "turn-2", "user", "Edit me"),
        ];
        replace_conversation_messages(dir.path(), &source.conversation_id, messages).unwrap();
        update_conversation_context(
            dir.path(),
            &source.conversation_id,
            Some(vec![ChatContextFileRecord {
                path: "/tmp/added-later.pdf".to_string(),
                pages: Some(2),
            }]),
            None,
        )
        .unwrap();

        let fork = create_fork_conversation(
            dir.path(),
            &source.conversation_id,
            "user-2",
            false,
            "fork-session".to_string(),
        )
        .unwrap();
        assert_eq!(fork.messages.len(), 2);
        assert!(fork.context_files.is_empty());
        let history = branch_history_text(&fork.messages);
        assert_eq!(history, "User: First\n\nAssistant: Answer");
        assert!(!history.contains("private reasoning"));
    }

    #[test]
    fn rejects_pre_branch_conversation_files_without_migrating_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            conversation_path(dir.path()),
            r#"{"version":1,"conversations":[]}"#,
        )
        .unwrap();

        let error = list_conversations(dir.path()).unwrap_err().to_string();
        assert!(error.contains("unsupported chat conversation version: 1"));
    }
}
