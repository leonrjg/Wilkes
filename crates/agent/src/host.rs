//! Wilkes, as a chat session sees it.
//!
//! The chat itself is `wilkes-chat`: the handshake, the streaming, the parked
//! permission request, the conversations on disk. None of that is about
//! documents, and none of it is here. This module is the other half — the four
//! things `ChatHost` asks an application, answered by Wilkes:
//!
//! - **What must the agent know every turn?** The current root, the open
//!   document and its extracted text, the documents in context. Pushed rather
//!   than offered as a tool, because "answer about *these* documents" is an
//!   invariant and must not sit behind the model's discretion.
//! - **Which MCP servers are attached?** Wilkes's own read-only one, whose
//!   lifetime is this host's: dropping the host stops the server.
//! - **Which files may the client read?** Those in the context set, the open
//!   document, and anything under the current root. Wilkes answers reads its
//!   own way — page-mapped text out of a PDF, not raw bytes — which is the
//!   reason to offer client-side reads at all.
//! - **Which tool calls are the application's own?** Calls to that MCP server,
//!   and only those. Everything else, including every tool the agent brought
//!   with it, is the user's decision.
//!
//! What it answers those questions *about* comes from the window, one
//! [`HostContext`] per call, applied before the session or turn it belongs to.
//! The client is the single owner of what is in context; this is its mirror.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{McpServer, ToolCallUpdate};
use serde::{Deserialize, Serialize};
use tracing::warn;
use wilkes_chat::host::ChatHost;
use wilkes_chat::AgentBackend;
use wilkes_core::types::IntegrationsSettings;

use crate::context::{
    build_context_block, root_context, ActiveDoc, ActiveDocText, ContextFile, RootContext,
};
use crate::search::WorkspaceCatalog;

const ACTIVE_DOC_CONTEXT_CHAR_LIMIT: usize = 12_000;
const READ_ACCESS_GUIDANCE: &str = "The user can either move the file to this root, switch to that root, or add it to the context using the right-click menu on the file list";

pub(crate) fn read_access_error(path: &Path) -> String {
    format!(
        "{} is not in the current root or this chat's context. {READ_ACCESS_GUIDANCE}",
        path.display()
    )
}

/// One document the chat is answering about, as the window names it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostContextFile {
    pub path: String,
    /// Total page count, when the window knows it (PDFs).
    #[serde(default)]
    pub pages: Option<u32>,
}

/// The document open in the preview pane, and where in it the reader is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostActiveDoc {
    pub path: String,
    #[serde(default)]
    pub page: Option<u32>,
}

/// What the window says this chat is about, right now.
///
/// Sent with every call that opens a session or a turn, and applied whole —
/// this replaces the state rather than amending it. That is the point: the
/// window is the one place that knows which documents are in context, so a
/// session cannot hold an opinion of its own that has to be kept in step.
///
/// Every field defaults, so a caller that knows about only some of them (an
/// older window, a test) still deserializes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostContext {
    #[serde(default)]
    pub search_root: Option<String>,
    #[serde(default)]
    pub active_doc: Option<HostActiveDoc>,
    #[serde(default)]
    pub context_files: Vec<HostContextFile>,
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
    pub root: RootContext,
}

/// The context set, shared between the host and the MCP server that answers
/// out of it. Cloning shares; there is one set per session.
#[derive(Clone, Debug, Default)]
pub(crate) struct ContextStateHandle {
    state: Arc<Mutex<SharedContextState>>,
}

impl ContextStateHandle {
    /// Replace what the chat is about with what the window says it is.
    ///
    /// `added_this_turn` is derived rather than declared: a document the
    /// previous state did not hold is new, and the next context block marks it
    /// so. The window does not have to remember what it last sent.
    pub(crate) fn apply(&self, context: HostContext) {
        let mut state = self.state.lock().unwrap();
        let known: Vec<String> = state
            .context_files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        state.search_root = context.search_root.map(PathBuf::from);
        state.active_doc = context.active_doc.map(|doc| ActiveDoc {
            path: doc.path,
            page: doc.page,
        });
        state.context_files = context
            .context_files
            .into_iter()
            .map(|file| ContextFile {
                added_this_turn: !known.contains(&file.path),
                path: file.path,
                pages: file.pages,
            })
            .collect();
    }

    /// What the host currently holds, in the shape the window sent it.
    ///
    /// Read back rather than remembered by the caller, so the record a turn
    /// keeps of what it was about is the state that actually answered it.
    pub(crate) fn current(&self) -> HostContext {
        let state = self.state.lock().unwrap();
        HostContext {
            search_root: state
                .search_root
                .as_ref()
                .map(|root| root.to_string_lossy().into_owned()),
            active_doc: state.active_doc.as_ref().map(|doc| HostActiveDoc {
                path: doc.path.clone(),
                page: doc.page,
            }),
            context_files: state
                .context_files
                .iter()
                .map(|file| HostContextFile {
                    path: file.path.clone(),
                    pages: file.pages,
                })
                .collect(),
        }
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
            root: root_context(state.search_root.as_deref()),
        }
    }

    /// The snapshot for the turn about to be sent, and whether it is the
    /// session's first. Clears the per-turn "added" marks, so a document is
    /// announced as new exactly once.
    fn prepare_turn(&self) -> (bool, ContextSnapshot) {
        let mut state = self.state.lock().unwrap();
        let first_turn = !state.first_turn_sent;
        let snapshot = ContextSnapshot {
            active_doc: state.active_doc.clone(),
            context_files: state.context_files.clone(),
            root: root_context(state.search_root.as_deref()),
        };
        state.first_turn_sent = true;
        for file in state.context_files.iter_mut() {
            file.added_this_turn = false;
        }
        (first_turn, snapshot)
    }

    /// Whether the agent may be handed this file's text.
    ///
    /// Canonicalised on both sides, so a symlink or a `..` cannot name a file
    /// outside the root by spelling it differently. A path that does not
    /// resolve is refused rather than guessed at.
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
            || state
                .search_root
                .as_deref()
                .and_then(|root| root.canonicalize().ok())
                .is_some_and(|root| canonical.starts_with(root))
    }
}

/// Building a context set a piece at a time.
///
/// Production has exactly one way in -- [`ContextStateHandle::apply`] -- because
/// the window is the only thing that decides what a chat is about, and a second
/// entrance is a second owner. These exist so a test can say "a root and one
/// document" without writing out the whole state each time.
#[cfg(test)]
impl ContextStateHandle {
    pub(crate) fn set_search_root(&self, root: Option<String>) {
        self.apply(HostContext { search_root: root, ..self.current() });
    }

    pub(crate) fn set_active_doc(&self, path: Option<String>, page: Option<u32>) {
        self.apply(HostContext {
            active_doc: path.map(|path| HostActiveDoc { path, page }),
            ..self.current()
        });
    }

    pub(crate) fn add_context(&self, path: String, pages: Option<u32>) {
        let mut current = self.current();
        if !current.context_files.iter().any(|file| file.path == path) {
            current.context_files.push(HostContextFile { path, pages });
        }
        self.apply(current);
    }
}

/// Wilkes behind a chat session: the context set, and the MCP server that
/// answers out of it.
///
/// The server's lifetime is this host's. A session holds its host for as long
/// as it lives, so the server is torn down with the session it was started
/// for and there is no separate shutdown to remember.
pub struct WilkesChatHost {
    context: ContextStateHandle,
    mcp: Option<crate::mcp::McpRuntime>,
}

impl WilkesChatHost {
    /// Start a host for one session, MCP server and all.
    ///
    /// The server is only started for backends that can reach it: Nanocoder
    /// speaks ACP but does not attach MCP servers, and starting a listener it
    /// will never call is a port and a task for nothing.
    pub async fn start(
        backend: AgentBackend,
        cwd: PathBuf,
        workspaces: Option<Arc<dyn WorkspaceCatalog>>,
        integrations: IntegrationsSettings,
    ) -> anyhow::Result<Arc<Self>> {
        let context = ContextStateHandle::default();
        let mcp = if matches!(backend, AgentBackend::ClaudeCode | AgentBackend::Codex) {
            Some(crate::mcp::start(context.clone(), cwd, workspaces, integrations).await?)
        } else {
            None
        };
        Ok(Arc::new(Self { context, mcp }))
    }

    /// Point the host at what the window says the chat is now about.
    pub fn apply(&self, context: HostContext) {
        self.context.apply(context);
    }

    /// The documents in context, for the record a conversation keeps of what
    /// it was answering about.
    pub fn context_paths(&self) -> Vec<String> {
        self.context.context_paths()
    }

    /// What this host currently holds, for the environment a turn records.
    pub fn context(&self) -> HostContext {
        self.context.current()
    }

    /// The root this chat is searching, if any.
    pub fn search_root(&self) -> Option<PathBuf> {
        self.context.search_root()
    }
}

impl ChatHost for WilkesChatHost {
    fn context_block(&self, first_turn: bool) -> Option<String> {
        let (session_first_turn, snapshot) = self.context.prepare_turn();
        let active_doc_text = snapshot
            .active_doc
            .as_ref()
            .map(active_doc_text_for_context);
        Some(build_context_block(
            // The session's own count, not the caller's: a resumed
            // conversation is on its first prompt of *this* subprocess, which
            // is what has never been told the preamble.
            first_turn || session_first_turn,
            &snapshot.root,
            snapshot.active_doc.as_ref(),
            &snapshot.context_files,
            active_doc_text.as_ref(),
        ))
    }

    fn mcp_servers(&self) -> Vec<McpServer> {
        self.mcp
            .as_ref()
            .map(|runtime| vec![runtime.server_config()])
            .unwrap_or_default()
    }

    fn offers_file_read(&self) -> bool {
        // Wilkes reads a PDF as page-mapped text rather than as bytes, which
        // the agent's own file tools cannot do. That, and not wider reach, is
        // why these are answered here: the reachable set is narrower than the
        // agent's own, not broader.
        true
    }

    fn read_text_file(
        &self,
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<String, String> {
        if !self.context.is_allowed(path) {
            return Err(read_access_error(path));
        }
        crate::reader::read_text(path, None, line, limit).map_err(|e| e.to_string())
    }

    fn auto_allows(&self, tool_call: &ToolCallUpdate) -> bool {
        // Only if this host actually started the server. Without that check a
        // Nanocoder session would auto-allow a tool named `wilkes.search`
        // that Wilkes never served.
        self.mcp.is_some() && is_wilkes_mcp_call(tool_call)
    }

    fn strip_context_block(&self, text: &str) -> Option<String> {
        strip_wilkes_context_prefix(text)
    }
}

/// Take Wilkes's own pushed block back off a replayed user message.
///
/// `session/load` replays what was *sent*, and what was sent had the context
/// block on the front of it. Without this, reopening a conversation shows the
/// user their own question with the machinery stapled to it.
fn strip_wilkes_context_prefix(text: &str) -> Option<String> {
    const CLOSE: &str = "</wilkes-context>";
    if !text.starts_with("<wilkes-context>") {
        return None;
    }
    let end = text.find(CLOSE)? + CLOSE.len();
    Some(text[end..].trim_start().to_string())
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

/// Whether a tool call is Wilkes's own MCP server answering.
///
/// Matched on the name the agent reports, which each CLI spells its own way:
/// `wilkes.search`, `mcp.wilkes.search`, `mcp__wilkes__search`. The raw input
/// is checked too, because some agents put the tool name only there.
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

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ToolCallId, ToolCallUpdateFields};
    use serde_json::json;

    fn tool_call(title: Option<&str>, raw_input: Option<serde_json::Value>) -> ToolCallUpdate {
        let mut fields = ToolCallUpdateFields::default();
        fields.title = title.map(|t| t.to_string());
        fields.raw_input = raw_input;
        ToolCallUpdate::new(ToolCallId::new("call-1"), fields)
    }

    #[test]
    fn the_window_is_the_only_owner_of_what_is_in_context() {
        // Applying replaces rather than amends: a document the window stopped
        // showing must stop being in context, and there is no "remove" call
        // for it to arrive through.
        let handle = ContextStateHandle::default();
        handle.apply(HostContext {
            search_root: Some("/library".into()),
            active_doc: None,
            context_files: vec![
                HostContextFile { path: "/a.pdf".into(), pages: Some(3) },
                HostContextFile { path: "/b.pdf".into(), pages: None },
            ],
        });
        handle.apply(HostContext {
            search_root: Some("/library".into()),
            active_doc: None,
            context_files: vec![HostContextFile { path: "/b.pdf".into(), pages: None }],
        });

        assert_eq!(handle.context_paths(), vec!["/b.pdf".to_string()]);
    }

    #[test]
    fn a_document_is_announced_as_new_once_and_then_is_not() {
        // "<- added this turn" is derived from what the host already held, so
        // the window never has to remember what it last sent.
        let handle = ContextStateHandle::default();
        handle.apply(HostContext {
            context_files: vec![HostContextFile { path: "/a.pdf".into(), pages: None }],
            ..HostContext::default()
        });

        let (_, snapshot) = handle.prepare_turn();
        assert!(snapshot.context_files[0].added_this_turn);

        handle.apply(HostContext {
            context_files: vec![HostContextFile { path: "/a.pdf".into(), pages: None }],
            ..HostContext::default()
        });
        let (_, snapshot) = handle.prepare_turn();
        assert!(!snapshot.context_files[0].added_this_turn);
    }

    #[test]
    fn what_the_host_holds_reads_back_as_what_was_sent_to_it() {
        // The turn's record is written from this, so a round trip that lost a
        // field would record a conversation as being about less than it was.
        let handle = ContextStateHandle::default();
        let sent = HostContext {
            search_root: Some("/library".into()),
            active_doc: Some(HostActiveDoc { path: "/a.pdf".into(), page: Some(4) }),
            context_files: vec![HostContextFile { path: "/a.pdf".into(), pages: Some(9) }],
        };
        handle.apply(sent.clone());

        assert_eq!(handle.current(), sent);
    }

    #[test]
    fn only_the_first_prompt_of_a_session_carries_the_preamble() {
        let handle = ContextStateHandle::default();
        assert!(handle.prepare_turn().0);
        assert!(!handle.prepare_turn().0);
    }

    #[test]
    fn a_file_outside_the_root_and_the_context_is_not_readable() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("inside.txt"), "x").unwrap();
        std::fs::write(elsewhere.path().join("outside.txt"), "x").unwrap();

        let handle = ContextStateHandle::default();
        handle.apply(HostContext {
            search_root: Some(root.path().to_string_lossy().into_owned()),
            ..HostContext::default()
        });

        assert!(handle.is_allowed(&root.path().join("inside.txt")));
        assert!(!handle.is_allowed(&elsewhere.path().join("outside.txt")));
    }

    #[test]
    fn a_file_named_into_the_root_from_outside_it_is_still_outside() {
        // The check canonicalises, so `<root>/../elsewhere/x` does not pass by
        // being spelled as though it started inside.
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("outside.txt"), "x").unwrap();

        let handle = ContextStateHandle::default();
        handle.apply(HostContext {
            search_root: Some(root.path().to_string_lossy().into_owned()),
            ..HostContext::default()
        });

        let sneaky = root.path().join("..").join(
            elsewhere
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        );
        assert!(!handle.is_allowed(&sneaky.join("outside.txt")));
    }

    #[test]
    fn the_refusal_says_what_would_make_the_read_work() {
        // Read by a model deciding what to do next, so it names the path and
        // the ways out rather than only reporting a denial.
        let message = read_access_error(Path::new("/tmp/paper.pdf"));
        assert!(message.contains("/tmp/paper.pdf"));
        assert!(message.contains("add it to the context"));
    }

    #[test]
    fn a_replayed_message_loses_the_block_and_keeps_the_question() {
        let replayed = "<wilkes-context>\nOpen document: none\n</wilkes-context>\nWhat is this?";
        assert_eq!(
            strip_wilkes_context_prefix(replayed).as_deref(),
            Some("What is this?")
        );
        assert!(strip_wilkes_context_prefix("Just a question").is_none());
    }

    #[test]
    fn wilkes_own_tools_are_recognised_however_the_agent_spells_them() {
        assert!(is_wilkes_mcp_call(&tool_call(Some("wilkes.search"), None)));
        assert!(is_wilkes_mcp_call(&tool_call(
            Some("mcp__wilkes__get_document_text"),
            None
        )));
        assert!(is_wilkes_mcp_call(&tool_call(
            None,
            Some(json!({ "tool": "mcp.wilkes.list_context" }))
        )));
        assert!(is_wilkes_mcp_call(&tool_call(Some("list_context"), None)));
    }

    #[test]
    fn a_tool_that_merely_mentions_searching_is_not_wilkes_own() {
        // The auto-allow is the one place a permission prompt is skipped, so
        // matching has to be on the server's own names and nothing looser.
        assert!(!is_wilkes_mcp_call(&tool_call(Some("Search the web"), None)));
        assert!(!is_wilkes_mcp_call(&tool_call(Some("other.search"), None)));
    }

    #[test]
    fn downloading_is_not_one_of_the_tools_that_skips_the_prompt() {
        // `download` is a mutating tool on the same server. It must go to the
        // user like anything else that changes the disk.
        assert!(!crate::mcp::WILKES_MCP_TOOL_NAMES.contains(&"download"));
        assert!(!is_wilkes_mcp_call(&tool_call(Some("wilkes.download"), None)));
    }
}
