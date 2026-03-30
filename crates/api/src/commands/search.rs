use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::search::grep::GrepSearchProvider;
use wilkes_core::search::SearchProvider;
use wilkes_core::types::{FileMatches, SearchQuery};

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
        self.worker.await.unwrap_or_default()
    }
}

/// Spawn a search and return a `SearchHandle` whose `rx` streams `FileMatches`.
pub fn start_search(query: SearchQuery) -> SearchHandle {
    let (tx, rx) = mpsc::channel::<FileMatches>(64);

    let worker = tokio::task::spawn_blocking(move || {
        let provider = GrepSearchProvider::new();
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        match provider.search(&query, &registry, tx) {
            Ok(errors) => errors,
            Err(e) => vec![format!("search failed: {e:#}")],
        }
    });

    SearchHandle { rx, worker }
}
