pub mod grep;

use crate::extract::ExtractorRegistry;
use crate::types::{FileMatches, SearchCapabilities, SearchQuery};
use tokio::sync::mpsc;

pub type SearchResultTx = mpsc::Sender<FileMatches>;

pub trait SearchProvider: Send + Sync {
    /// Begin searching. Results are sent to `tx` as they are discovered.
    /// Returns when the search is complete or cancelled (`tx.is_closed()`).
    /// The returned `Vec<String>` contains non-fatal per-file errors (e.g. failed
    /// PDF extraction) that did not abort the search.
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
    ) -> anyhow::Result<Vec<String>>;

    fn capabilities(&self) -> SearchCapabilities;
}
