use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use wilkes_core::types::{
    FileMatches, IntegrationsSettings, RelatedDocument, RelatedDocumentsQuery, SearchQuery,
    SearchStats, SmartCollection,
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
}
