pub mod grep;
pub mod semantic;

use crate::extract::ExtractorRegistry;
use crate::types::{FileMatches, SearchCapabilities, SearchQuery};
use tokio::sync::mpsc;

pub type SearchResultTx = mpsc::Sender<FileMatches>;

/// Non-streaming information produced by a search provider. File matches still
/// travel over `SearchResultTx`; this outcome carries terminal diagnostics and
/// the exact query-expansion text that affected ranking.
#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub errors: Vec<String>,
    pub hyde_documents: Vec<String>,
}

impl From<Vec<String>> for SearchOutcome {
    fn from(errors: Vec<String>) -> Self {
        Self {
            errors,
            hyde_documents: Vec::new(),
        }
    }
}

pub trait SearchProvider: Send + Sync {
    /// Begin searching. Results are sent to `tx` as they are discovered.
    /// Returns when the search is complete or cancelled (`tx.is_closed()`).
    /// The returned outcome contains non-fatal per-file errors (e.g. failed PDF
    /// extraction) and any query-expansion text that actually affected ranking.
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
        eligible_paths: Option<&std::collections::HashSet<std::path::PathBuf>>,
    ) -> anyhow::Result<SearchOutcome>;

    fn capabilities(&self) -> SearchCapabilities;
}
