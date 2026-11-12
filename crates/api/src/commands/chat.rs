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

pub use wilkes_agent::{HostActiveDoc, HostContext, HostContextFile};
pub use wilkes_chat::auth::{ChatAuth, LoginOutcome};
pub use wilkes_chat::backend::{
    backend_status, install_backend, is_resumable, label, list_backends, AgentBackend,
    BackendStatus,
};
pub use wilkes_chat::session::{
    config_for_backend, config_values_from_options, upsert_backend_config, ChatBackendConfig,
    ChatConfigOption, ChatConfigValue, ChatEvent, ChatReplayContentBlock, ChatReplayMessage,
    ChatReplayToolCall, ChatSession, TurnError,
};
pub use wilkes_chat::store::{
    branch_history_text, create_conversation, create_fork_conversation, environment_at_message,
    forget_conversation, get_conversation, list_conversations, mark_branch_history_seeded,
    records_from_replay, replace_conversation_messages, touch_conversation,
    update_conversation_config, ChatConversationRecord, ChatMessageRecord, ChatTurnEnvironment,
};
pub use wilkes_chat::transcript::apply_chat_event;
pub use wilkes_chat::wire::{emission, turn_failure, ChatEmission, ChatUpdate};

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
) -> anyhow::Result<(SpawnedChatSession, Arc<WilkesChatHost>)> {
    let host = WilkesChatHost::start(backend, cwd.clone(), workspaces).await?;
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
) -> anyhow::Result<(SpawnedChatSession, Arc<WilkesChatHost>)> {
    let cwd = PathBuf::from(&record.cwd);
    let host = WilkesChatHost::start(record.backend, cwd.clone(), workspaces).await?;
    let spawned = wilkes_chat::session::spawn(
        SpawnOptions::new(record.backend, cwd)
            .host(host.clone())
            .load(record.backend_session_id.clone()),
    )
    .await?;
    Ok((spawned, host))
}

const CONVERSATION_FILE: &str = "chat-conversations.json";

/// What a workspace's own store is renamed to once its conversations have been
/// gathered: kept rather than deleted, as the only copy until the shared store
/// has been seen to hold them.
const GATHERED_FILE: &str = "chat-conversations.json.gathered";

/// Gather the conversations each workspace kept into the installation's one
/// store, in `app_data_dir`.
///
/// Saved chats used to live in each workspace's directory, so the history menu
/// showed only the workspace that was open, and a chat begun in one was gone
/// from it after a switch. They are one list now, and this moves the old ones
/// into it.
///
/// Merged as JSON rather than through the store's types, so a file still in
/// the pre-`wilkes-chat` shape arrives whole for
/// [`migrate_legacy_conversations`] to convert. A conversation already in the
/// shared store is kept as it is there: it can only be there because an
/// earlier run gathered it, and the shared copy is the one written since.
///
/// The shared store is written before any workspace file is renamed, so an
/// interruption between the two only gathers the same conversations again,
/// which the rule above makes harmless. A workspace store that cannot be read
/// is left where it is, and named in the error once the others are gathered;
/// a shared store that cannot be read stops the whole move, since writing it
/// would destroy whatever it actually holds.
pub fn gather_workspace_conversations(app_data_dir: &Path) -> anyhow::Result<()> {
    let workspaces = crate::workspace::workspaces_dir(app_data_dir);
    let entries = match std::fs::read_dir(&workspaces) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut sources = Vec::new();
    for entry in entries {
        let path = entry?.path().join(CONVERSATION_FILE);
        if path.is_file() {
            sources.push(path);
        }
    }
    if sources.is_empty() {
        return Ok(());
    }
    sources.sort();

    let shared_path = app_data_dir.join(CONVERSATION_FILE);
    let mut shared: Value = if shared_path.is_file() {
        serde_json::from_str(&std::fs::read_to_string(&shared_path)?).map_err(|error| {
            anyhow::anyhow!(
                "the shared chat store {} cannot be read: {error}",
                shared_path.display()
            )
        })?
    } else {
        serde_json::json!({ "version": 2, "conversations": [] })
    };

    let mut unreadable = Vec::new();
    let mut gathered = Vec::new();
    let mut moved = 0usize;
    for source in sources {
        let store = match std::fs::read_to_string(&source)
            .map_err(anyhow::Error::from)
            .and_then(|text| Ok(serde_json::from_str::<Value>(&text)?))
        {
            Ok(store) => store,
            Err(error) => {
                tracing::error!("chat: cannot read {}: {error:#}", source.display());
                unreadable.push(source);
                continue;
            }
        };
        let (Some(version), Some(conversations)) = (
            store.get("version").and_then(Value::as_u64),
            store.get("conversations").and_then(Value::as_array),
        ) else {
            tracing::error!(
                "chat: {} has no version or conversations list",
                source.display()
            );
            unreadable.push(source);
            continue;
        };

        let shared_version = shared["version"].as_u64().ok_or_else(|| {
            anyhow::anyhow!(
                "the shared chat store {} has no version",
                shared_path.display()
            )
        })?;
        shared["version"] = Value::from(shared_version.max(version));
        let into = shared["conversations"].as_array_mut().ok_or_else(|| {
            anyhow::anyhow!(
                "the shared chat store {} has no conversations list",
                shared_path.display()
            )
        })?;
        for conversation in conversations {
            let id = conversation
                .get("conversation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "a conversation in {} has no conversation_id",
                        source.display()
                    )
                })?;
            let known = into.iter().any(|existing| {
                existing.get("conversation_id").and_then(Value::as_str) == Some(id)
            });
            if !known {
                into.push(conversation.clone());
                moved += 1;
            }
        }
        gathered.push(source);
    }

    if !gathered.is_empty() {
        std::fs::create_dir_all(app_data_dir)?;
        let temp = shared_path.with_extension("json.gathering");
        std::fs::write(&temp, serde_json::to_string_pretty(&shared)?)?;
        std::fs::rename(&temp, &shared_path)?;
        for source in &gathered {
            std::fs::rename(source, source.with_file_name(GATHERED_FILE))?;
        }
        tracing::info!(
            moved,
            workspaces = gathered.len(),
            "chat: gathered the workspaces' conversations into the shared store"
        );
    }

    anyhow::ensure!(
        unreadable.is_empty(),
        "these workspace chat stores could not be read and were left in place: {}",
        unreadable
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
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

    let path = data_dir.join(CONVERSATION_FILE);
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

    fn workspace_store(root: &Path, workspace: &str, json: &str) -> PathBuf {
        let dir = root.join("workspaces").join(workspace);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat-conversations.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    fn ids(store: &Value) -> Vec<String> {
        let mut ids: Vec<String> = store["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["conversation_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn every_workspace_s_chats_end_up_in_the_one_list() {
        // Kept per workspace, a chat begun in one was missing from the history
        // menu of every other.
        let dir = tempdir().unwrap();
        let a = workspace_store(
            dir.path(),
            "a",
            r#"{"version": 2, "conversations": [{"conversation_id": "c1"}]}"#,
        );
        let b = workspace_store(
            dir.path(),
            "b",
            r#"{"version": 2, "conversations": [{"conversation_id": "c2"}]}"#,
        );
        gather_workspace_conversations(dir.path()).unwrap();

        assert_eq!(ids(&read(dir.path())), ["c1", "c2"]);
        assert!(!a.exists() && !b.exists());
        assert!(a
            .with_file_name("chat-conversations.json.gathered")
            .exists());
    }

    #[test]
    fn a_conversation_already_gathered_keeps_its_shared_copy() {
        // Only an interrupted earlier run puts one in both places, and the
        // shared copy is the one that has been written to since.
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            r#"{"version": 2, "conversations": [{"conversation_id": "c1", "title": "newer"}]}"#,
        );
        workspace_store(
            dir.path(),
            "a",
            r#"{"version": 2, "conversations": [{"conversation_id": "c1", "title": "older"}]}"#,
        );
        gather_workspace_conversations(dir.path()).unwrap();

        let store = read(dir.path());
        assert_eq!(ids(&store), ["c1"]);
        assert_eq!(store["conversations"][0]["title"], "newer");
    }

    #[test]
    fn a_legacy_workspace_store_arrives_whole_for_the_shape_migration() {
        let dir = tempdir().unwrap();
        workspace_store(dir.path(), "a", LEGACY);
        gather_workspace_conversations(dir.path()).unwrap();
        migrate_legacy_conversations(dir.path()).unwrap();

        let host = &read(dir.path())["conversations"][0]["messages"][0]["environment"]["host"];
        assert_eq!(host["search_root"], "/library");
    }

    #[test]
    fn an_unreadable_workspace_store_is_left_in_place_and_named() {
        let dir = tempdir().unwrap();
        let broken = workspace_store(dir.path(), "a", "{ not json");
        workspace_store(
            dir.path(),
            "b",
            r#"{"version": 2, "conversations": [{"conversation_id": "c2"}]}"#,
        );
        let error = gather_workspace_conversations(dir.path()).unwrap_err();

        assert!(error.to_string().contains(&broken.display().to_string()));
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json");
        assert_eq!(ids(&read(dir.path())), ["c2"]);
    }

    #[test]
    fn an_unreadable_shared_store_stops_the_move() {
        // Writing it would destroy whatever it actually holds.
        let dir = tempdir().unwrap();
        write(dir.path(), "{ not json");
        let source = workspace_store(
            dir.path(),
            "a",
            r#"{"version": 2, "conversations": [{"conversation_id": "c1"}]}"#,
        );
        assert!(gather_workspace_conversations(dir.path()).is_err());

        assert!(source.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("chat-conversations.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn no_workspaces_at_all_is_not_an_error() {
        let dir = tempdir().unwrap();
        gather_workspace_conversations(dir.path()).unwrap();
        assert!(!dir.path().join("chat-conversations.json").exists());
    }

    #[test]
    fn no_store_at_all_is_not_an_error() {
        let dir = tempdir().unwrap();
        migrate_legacy_conversations(dir.path()).unwrap();
        assert!(!dir.path().join("chat-conversations.json").exists());
    }
}
