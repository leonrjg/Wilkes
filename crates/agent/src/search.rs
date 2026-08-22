use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use wilkes_core::types::{
    DocumentMetadata, FileListResponse, FileMatches, IntegrationsSettings, RelatedDocument,
    RelatedDocumentsQuery, SearchQuery, SearchStats, SmartCollection,
};

#[derive(Debug, Clone, Serialize)]
pub struct CollectedSearch {
    pub files: Vec<FileMatches>,
    pub stats: SearchStats,
    pub truncated: bool,
}

/// Read-only search boundary for agent integrations.
///
/// The agent crate defines only the contract. The API layer owns the actual
/// exact/semantic search implementation because it owns settings, index state,
/// embedders, and background reindexing.
#[async_trait]
pub trait SearchService: Send + Sync {
    async fn default_root(self: Arc<Self>) -> Option<PathBuf> {
        None
    }

    async fn library_roots(self: Arc<Self>) -> Vec<PathBuf>;

    /// Current maximum file size for search. Agent-facing callers use this
    /// rather than maintaining a second copy of the application setting.
    /// A value of zero means unlimited.
    async fn max_search_file_size(self: Arc<Self>) -> u64;

    async fn list_smart_collections(self: Arc<Self>) -> Result<Vec<SmartCollection>, String> {
        Ok(Vec::new())
    }

    /// Current integration settings for long-lived MCP servers. Chat-scoped
    /// servers receive a point-in-time snapshot directly, while an external
    /// application-scoped server must observe settings edits without a restart.
    async fn integrations(self: Arc<Self>) -> IntegrationsSettings {
        IntegrationsSettings::default()
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

/// One Wilkes workspace as an MCP client sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceDescriptor {
    pub id: String,
    pub name: String,
    pub roots: Vec<PathBuf>,
    pub active_root: Option<PathBuf>,
    /// Whether this is the workspace a tool call reaches when it names none.
    pub active: bool,
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
