use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use wilkes_core::types::{
    FileMatches, RelatedDocument, RelatedDocumentsQuery, SearchQuery, SearchStats, SmartCollection,
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
