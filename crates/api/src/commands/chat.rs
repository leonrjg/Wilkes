//! Wilkes's side of the ACP chat: what the agent is answering about, and
//! where its conversations are kept.
//!
//! The chat itself is `wilkes-chat` — the subprocess, the handshake, the
//! streamed turn, the parked permission request, the store. None of that is
//! about documents and none of it is here. What Wilkes adds is one thing, a
//! [`wilkes_agent::WilkesChatHost`]: the read-only MCP server, the pushed
//! context block, the reads Wilkes can answer better than the agent's own
//! tools, and the auto-allow for that server and nothing else.
//!
//! Everything else in this module is a re-export of the chat's own surface, so
//! the desktop layer has one name to reach for and Wilkes has one copy of the
//! store rather than a second that agrees with it until someone edits one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use wilkes_agent::search::WorkspaceCatalog;
use wilkes_agent::WilkesChatHost;
use wilkes_chat::session::{SpawnOptions, SpawnedChatSession};
use wilkes_core::types::IntegrationsSettings;

pub use wilkes_agent::{HostActiveDoc, HostContext, HostContextFile};
pub use wilkes_chat::backend::{
    backend_status, install_backend, is_resumable, label, list_backends, AgentBackend,
    BackendStatus,
};
pub use wilkes_chat::session::{
    config_for_backend, config_values_from_options, upsert_backend_config, ChatBackendConfig,
    ChatConfigOption, ChatConfigValue, ChatEvent, ChatReplayContentBlock, ChatReplayMessage,
    ChatReplayToolCall, ChatSession,
};
pub use wilkes_chat::store::{
    branch_history_text, create_conversation, create_fork_conversation, environment_at_message,
    forget_conversation, get_conversation, list_conversations, mark_branch_history_seeded,
    records_from_replay, replace_conversation_messages, touch_conversation,
    update_conversation_config, ChatConversationRecord, ChatMessageRecord, ChatTurnEnvironment,
};
pub use wilkes_chat::transcript::apply_chat_event;
pub use wilkes_chat::wire::{emission, ChatEmission, ChatUpdate};

/// Start a subprocess, its MCP server, and complete the ACP handshake.
///
/// Resolves after the handshake rather than after the spawn, so a caller
/// learns here that an installed agent is not usable — logged out, most
/// often — instead of on the first message.
///
/// The returned host is the caller's handle on what the chat is about: it is
/// the same `Arc` the session holds, so applying a [`HostContext`] to it
/// changes what the next prompt carries.
pub async fn start(
    backend: AgentBackend,
    cwd: PathBuf,
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    integrations: IntegrationsSettings,
) -> anyhow::Result<(SpawnedChatSession, Arc<WilkesChatHost>)> {
    let host = WilkesChatHost::start(backend, cwd.clone(), workspaces, integrations).await?;
    let spawned =
        wilkes_chat::session::spawn(SpawnOptions::new(backend, cwd).host(host.clone())).await?;
    Ok((spawned, host))
}

/// Reattach to the agent's own session for a saved conversation.
///
/// Reopened in the directory it was held in, which is what the record stores
/// it for: a resumed session that silently moved would give the agent a
/// different view of the world than the transcript was written against.
pub async fn open(
    record: &ChatConversationRecord,
    workspaces: Option<Arc<dyn WorkspaceCatalog>>,
    integrations: IntegrationsSettings,
) -> anyhow::Result<(SpawnedChatSession, Arc<WilkesChatHost>)> {
    let cwd = PathBuf::from(&record.cwd);
    let host = WilkesChatHost::start(record.backend, cwd.clone(), workspaces, integrations).await?;
    let spawned = wilkes_chat::session::spawn(
        SpawnOptions::new(record.backend, cwd)
            .host(host.clone())
            .load(record.backend_session_id.clone()),
    )
    .await?;
    Ok((spawned, host))
}

/// Move conversations written before the chat moved into `wilkes-chat` into
/// the shape that crate reads.
///
/// Both files call themselves version 2, because both were version 2 of a
/// store that had not yet been shared — so the version number cannot tell them
/// apart and the shape has to. Wilkes wrote what a turn was about as three
/// fields of its own on each message's `environment`; the shared store keeps
/// exactly one, an opaque `host` blob it never reads. The fields move into it.
///
/// Idempotent, and a no-op for a file that is already in the new shape or is
/// not there at all. Anything it cannot parse is left alone rather than
/// rewritten: a store this could not understand is one it must not overwrite.
pub fn migrate_legacy_conversations(data_dir: &Path) -> anyhow::Result<()> {
    const LEGACY_KEYS: [&str; 3] = ["context_files", "active_doc", "search_root"];

    let path = data_dir.join("chat-conversations.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut store) = serde_json::from_str::<Value>(&text) else {
        return Ok(());
    };

    let mut moved = 0usize;
    let Some(conversations) = store.get_mut("conversations").and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for conversation in conversations.iter_mut() {
        // The conversation-level pair went with the pane, which now owns what
        // it shows and restores it from the turns below.
        if let Some(object) = conversation.as_object_mut() {
            for key in ["context_files", "active_doc"] {
                if object.remove(key).is_some() {
                    moved += 1;
                }
            }
        }

        let Some(messages) = conversation
            .get_mut("messages")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for message in messages.iter_mut() {
            let Some(environment) = message
                .get_mut("environment")
                .and_then(Value::as_object_mut)
            else {
                continue;
            };
            if environment.contains_key("host") {
                continue;
            }
            let mut host = serde_json::Map::new();
            for key in LEGACY_KEYS {
                if let Some(value) = environment.remove(key) {
                    host.insert(key.to_string(), value);
                }
            }
            if host.is_empty() {
                continue;
            }
            environment.insert("host".to_string(), Value::Object(host));
            moved += 1;
        }
    }

    if moved == 0 {
        return Ok(());
    }

    // Through a temp file and a rename, like every other write to this store:
    // a half-written migration would be a store neither shape can read.
    let temp = path.with_extension("json.migrating");
    std::fs::write(&temp, serde_json::to_string_pretty(&store)?)?;
    std::fs::rename(&temp, &path)?;
    tracing::info!(
        moved,
        "chat: migrated conversations to the shared store's shape"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, json: &str) {
        std::fs::write(dir.join("chat-conversations.json"), json).unwrap();
    }

    fn read(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("chat-conversations.json")).unwrap())
            .unwrap()
    }

    const LEGACY: &str = r#"{
      "version": 2,
      "conversations": [{
        "conversation_id": "c1",
        "context_files": [{"path": "/a.pdf", "pages": 3}],
        "active_doc": {"path": "/a.pdf", "page": 2},
        "messages": [{
          "message_id": "m1",
          "role": "user",
          "environment": {
            "config_values": [{"id": "model", "value": "sonnet"}],
            "context_files": [{"path": "/a.pdf", "pages": 3}],
            "active_doc": {"path": "/a.pdf", "page": 2},
            "search_root": "/library"
          }
        }]
      }]
    }"#;

    #[test]
    fn what_a_turn_was_about_survives_the_move_into_the_shared_store() {
        // The three fields are what a branch reopens on. Losing them would
        // leave every saved conversation forkable only onto today's documents.
        let dir = tempdir().unwrap();
        write(dir.path(), LEGACY);
        migrate_legacy_conversations(dir.path()).unwrap();

        let host = &read(dir.path())["conversations"][0]["messages"][0]["environment"]["host"];
        assert_eq!(host["search_root"], "/library");
        assert_eq!(host["active_doc"]["page"], 2);
        assert_eq!(host["context_files"][0]["path"], "/a.pdf");
    }

    #[test]
    fn the_config_the_turn_ran_under_stays_where_it_was() {
        // `config_values` is the shared store's own field and must not be
        // swept into the host blob with the rest.
        let dir = tempdir().unwrap();
        write(dir.path(), LEGACY);
        migrate_legacy_conversations(dir.path()).unwrap();

        let environment = &read(dir.path())["conversations"][0]["messages"][0]["environment"];
        assert_eq!(environment["config_values"][0]["value"], "sonnet");
        assert!(environment.get("search_root").is_none());
    }

    #[test]
    fn the_blob_deserializes_as_the_context_the_host_applies() {
        // The migration's output is only useful if it is the same shape the
        // window sends, since both end up at the same `apply`.
        let dir = tempdir().unwrap();
        write(dir.path(), LEGACY);
        migrate_legacy_conversations(dir.path()).unwrap();

        let host =
            read(dir.path())["conversations"][0]["messages"][0]["environment"]["host"].clone();
        let context: HostContext = serde_json::from_value(host).unwrap();
        assert_eq!(context.search_root.as_deref(), Some("/library"));
        assert_eq!(context.context_files[0].pages, Some(3));
    }

    #[test]
    fn running_it_twice_changes_nothing_the_second_time() {
        let dir = tempdir().unwrap();
        write(dir.path(), LEGACY);
        migrate_legacy_conversations(dir.path()).unwrap();
        let once = read(dir.path());
        migrate_legacy_conversations(dir.path()).unwrap();

        assert_eq!(once, read(dir.path()));
    }

    #[test]
    fn a_store_it_cannot_read_is_left_alone_rather_than_rewritten() {
        // Overwriting something unparseable would destroy the only copy of
        // whatever it actually was.
        let dir = tempdir().unwrap();
        write(dir.path(), "{ not json");
        migrate_legacy_conversations(dir.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("chat-conversations.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn no_store_at_all_is_not_an_error() {
        let dir = tempdir().unwrap();
        migrate_legacy_conversations(dir.path()).unwrap();
        assert!(!dir.path().join("chat-conversations.json").exists());
    }
}
