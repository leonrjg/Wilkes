use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wilkes_core::embed::Embedder;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::search::semantic::SemanticSearchProvider;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::search::grep::GrepSearchProvider;
use wilkes_core::search::SearchProvider;
use wilkes_core::types::{FileMatches, SearchMode, SearchQuery};

/// Handle to a running search. Dropping the handle cancels the search.
pub struct SearchHandle {
    pub rx: mpsc::Receiver<FileMatches>,
    worker: JoinHandle<Vec<String>>,
}

impl SearchHandle {
    pub async fn next(&mut self) -> Option<FileMatches> {
        self.rx.recv().await
    }

    /// Wait for the worker to finish and return any non-fatal errors it collected.
    /// Must only be called after `next()` has returned `None`.
    pub async fn finish(self) -> Vec<String> {
        match self.worker.await {
            Ok(errors) => errors,
            Err(e) => {
                eprintln!("search worker panicked: {e}");
                vec![format!("search worker panicked: {e}")]
            }
        }
    }
}

/// Spawn a search and return a `SearchHandle` whose `rx` streams `FileMatches`.
///
/// For `SearchMode::Grep`: `embedder` and `index` are ignored.
/// For `SearchMode::Semantic`: both must be `Some`, otherwise the search returns
/// an immediate error. The desktop validates presence before calling.
pub fn start_search(
    query: SearchQuery,
    embedder: Option<Arc<dyn Embedder>>,
    index: Option<Arc<Mutex<Option<SemanticIndex>>>>,
) -> SearchHandle {
    let (tx, rx) = mpsc::channel::<FileMatches>(64);

    let worker = tokio::task::spawn_blocking(move || {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        let provider: Box<dyn SearchProvider> = match query.mode {
            SearchMode::Semantic => {
                match (embedder, index) {
                    (Some(emb), Some(idx)) => {
                        Box::new(SemanticSearchProvider::new(emb, idx))
                    }
                    _ => {
                        return vec!["Semantic search requires a loaded embedder and built index".into()];
                    }
                }
            }
            SearchMode::Grep => Box::new(GrepSearchProvider::new()),
        };

        match provider.search(&query, &registry, tx) {
            Ok(errors) => errors,
            Err(e) => vec![format!("search failed: {e:#}")],
        }
    });

    SearchHandle { rx, worker }
}
