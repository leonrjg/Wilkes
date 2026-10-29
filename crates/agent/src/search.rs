use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use wilkes_core::types::{
    DocumentMetadata, FileListResponse, FileMatches, IntegrationsSettings, LiteratureSearchResult,
    RelatedDocument, RelatedDocumentsQuery, SearchQuery, SearchStats, SmartCollection,
};

/// How long a search started by an agent may run before it answers with what
/// it has.
///
/// An MCP client waits for a tool call inside one open request and gives up on
/// it; a search that answers after that has its answer thrown away, and the
/// client reports a timeout rather than the exact matches the search did find.
/// Unindexed exact matching over a corpus of PDFs is minutes, so the wait has
/// to be bounded somewhere, and here is the only place that knows the caller
/// cannot stream.
pub const AGENT_SEARCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
pub struct CollectedSearch {
    pub files: Vec<FileMatches>,
    pub stats: SearchStats,
    /// More documents matched than the caller asked to be given.
    pub truncated: bool,
    /// Whether the scan reached the end of the corpus. `false` means the
    /// budget ran out first: what is here matched, but what is not here was
    /// never looked at.
    pub complete: bool,
}

/// Read-only search boundary for agent integrations.
///
/// The agent crate defines only the contract. The API layer owns the actual
/// exact/semantic search implementation because it owns settings, index state,
/// embedders, and index builds.
#[async_trait]
pub trait SearchService: Send + Sync {
    async fn read_library(
        self: Arc<Self>,
        kind: crate::library::LibraryKind,
    ) -> Result<serde_json::Value, String> {
        let _ = kind;
        Err("Research-library reads are not available in this session".into())
    }

    async fn edit_library(
        self: Arc<Self>,
        edit: crate::library::LibraryEdit,
    ) -> Result<serde_json::Value, String> {
        let _ = edit;
        Err("Research-library edits are not available in this session".into())
    }

    /// Keeps a literature result for later. The result is the whole record
    /// the provider returned, which the MCP server holds and the model never
    /// sees, so this is not an `edit_library` action.
    async fn save_literature_result(
        self: Arc<Self>,
        provider: String,
        work: LiteratureSearchResult,
        note: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let _ = (provider, work, note);
        Err("Saving literature results is not available in this session".into())
    }

    /// Marks a saved literature result as in the library once `download` has
    /// fetched it to `path`. A result that was never saved is left alone.
    async fn mark_literature_acquired(
        self: Arc<Self>,
        provider: String,
        work: LiteratureSearchResult,
        path: PathBuf,
    ) -> Result<(), String> {
        let _ = (provider, work, path);
        Err("Saved literature results are not available in this session".into())
    }

    async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
        None
    }

    async fn library_roots(self: Arc<Self>) -> Vec<PathBuf>;

    /// Whether this workspace refuses writes. Every other method on this trait
    /// reads, so only the `download` tool has to ask — it is the one place an
    /// MCP client can put a file into a library, and a workspace another
    /// application owns is not one it may put files into.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Current maximum file size for search. Agent-facing callers use this
    /// rather than maintaining a second copy of the application setting.
    /// A value of zero means unlimited.
    async fn max_search_file_size(self: Arc<Self>) -> u64;

    /// Current per-file library download limit, read for each download.
    async fn max_download_bytes(self: Arc<Self>) -> u64;

    async fn list_smart_collections(self: Arc<Self>) -> Result<Vec<SmartCollection>, String> {
        Ok(Vec::new())
    }

    /// Current integration settings, read for each call so a server observes
    /// settings edits — including a manifest an agent just saved — without a
    /// restart.
    async fn integrations(self: Arc<Self>) -> IntegrationsSettings {
        IntegrationsSettings::default()
    }

    /// The custom integrations the user stored, or the one `id` names with its
    /// manifest.
    async fn custom_integrations(
        self: Arc<Self>,
        id: Option<String>,
    ) -> Result<serde_json::Value, String> {
        let _ = id;
        Err("Custom integrations are not available in this session".into())
    }

    /// Runs a draft manifest's search once. The service decides which stored
    /// secrets, if any, the draft may use; the caller never supplies one.
    async fn probe_custom_integration(
        self: Arc<Self>,
        manifest: String,
        query: String,
    ) -> Result<IntegrationProbe, String> {
        let _ = (manifest, query);
        Err("Custom integrations are not available in this session".into())
    }

    /// Stores `manifest` under its own id, adding an integration or replacing
    /// the one of that id. `enabled` of `None` keeps what is stored.
    async fn save_custom_integration(
        self: Arc<Self>,
        manifest: String,
        enabled: Option<bool>,
    ) -> Result<serde_json::Value, String> {
        let _ = (manifest, enabled);
        Err("Custom integrations are not available in this session".into())
    }

    /// Where the installation's catalogue mirror lives, so the catalogues are
    /// providers here as they are everywhere else. `None` is a service with no
    /// mirror, which then offers none.
    fn catalogue_mirror(&self) -> Option<PathBuf> {
        None
    }

    async fn search(
        self: Arc<Self>,
        query: SearchQuery,
        max_files: usize,
    ) -> Result<CollectedSearch, String>;

    async fn related_documents(
        self: Arc<Self>,
        query: RelatedDocumentsQuery,
    ) -> Result<Vec<RelatedDocument>, String>;

    /// List the documents under `root`, each carrying the same cache-enriched
    /// bibliographic fields (title, author, DOI, publication date, citation
    /// count, tags) the desktop file list shows. Backs the `list_documents`
    /// MCP tool.
    async fn list_documents(self: Arc<Self>, root: PathBuf) -> Result<FileListResponse, String> {
        let _ = root;
        Err("Document listing is not available in this session.".to_string())
    }

    /// Richest available metadata for a single document: cache-first (so
    /// provider enrichment already resolved for the library is included),
    /// falling back to on-the-fly extraction for a not-yet-cached file. Backs
    /// the `get_file_metadata` MCP tool.
    async fn document_metadata(self: Arc<Self>, path: PathBuf) -> Result<DocumentMetadata, String> {
        let _ = path;
        Err("Document metadata is not available in this session.".to_string())
    }
}

/// What a draft manifest's probe found.
#[derive(Debug, Clone)]
pub struct IntegrationProbe {
    pub report: wilkes_core::integrations::custom::ProbeReport,
    /// Whether it ran with the secrets stored for the integration it would
    /// replace. It runs without them when it is new, when none are stored, or
    /// when it reaches an origin or program the stored manifest did not.
    pub stored_secrets: bool,
}

/// One Wilkes workspace as an MCP client sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceDescriptor {
    pub id: String,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub active_root: Option<PathBuf>,
    /// Whether this is the workspace a tool call reaches when it names none.
    pub active: bool,
    /// Whether the workspace can only be read. An application-managed corpus
    /// is listed like any other so its documents can be searched, but every
    /// write to it is refused.
    pub read_only: bool,
}

/// Resolves which workspace's library a single tool call reads.
///
/// Each workspace owns its own roots, metadata cache and index, so a
/// [`SearchService`] answers for exactly one of them. Holding one service for
/// the lifetime of a server therefore pins it to whichever workspace was
/// active when it started; this boundary resolves the service per call
/// instead, from an id the caller may name.
///
/// Naming a workspace must never activate it: the registry, the desktop
/// window and the active context stay where they are.
#[async_trait]
pub trait WorkspaceCatalog: Send + Sync {
    /// Every workspace, with the active one flagged.
    async fn workspaces(&self) -> Result<Vec<WorkspaceDescriptor>, String>;

    /// The service reading `workspace_id`, or the active workspace when the
    /// caller names none. Errors when the id is unknown.
    async fn search_for(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Arc<dyn SearchService>, String>;
}
