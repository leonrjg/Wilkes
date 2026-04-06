use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::error;
use wilkes_core::embed::Embedder;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::search::semantic::SemanticSearchProvider;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::search::grep::GrepSearchProvider;
use wilkes_core::search::SearchProvider;
use wilkes_core::types::{FileMatches, SearchMode, SearchQuery, SearchStats};

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
                error!("search worker panicked: {e}");
                vec![format!("search worker panicked: {e}")]
            }
        }
    }

    /// Consumes the search stream, executing `on_result` for each match.
    /// Returns the final SearchStats once the search is complete.
    pub async fn run<F, Fut>(mut self, mut on_result: F) -> SearchStats
    where
        F: FnMut(FileMatches) -> Fut,
        Fut: std::future::Future<Output = bool> + Send, // Return false to abort early
    {
        let started = std::time::Instant::now();
        let mut total_matches = 0;
        let mut files_scanned = 0;

        while let Some(fm) = self.next().await {
            total_matches += fm.matches.len();
            files_scanned += 1;
            if !on_result(fm).await {
                break;
            }
        }

        SearchStats {
            files_scanned,
            total_matches,
            elapsed_ms: started.elapsed().as_millis() as u64,
            errors: self.finish().await,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[tokio::test]
    async fn test_start_search_grep() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("test.txt"), "hello world").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.clone(),
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            supported_extensions: vec!["txt".to_string()],
        };

        let mut handle = start_search(query, None, None);
        let mut matches = Vec::new();
        while let Some(m) = handle.rx.recv().await {
            matches.push(m);
        }
        
        assert!(!matches.is_empty());
        assert_eq!(matches[0].path.file_name().unwrap(), "test.txt");
        assert_eq!(matches[0].matches.len(), 1);
        assert!(matches[0].matches[0].matched_text.contains("hello"));

        let errs = handle.finish().await;
        assert!(errs.is_empty());
    }

    #[tokio::test]
    async fn test_start_search_semantic_missing() {
        let dir = tempdir().unwrap();
        let query = SearchQuery {
            pattern: "test".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: dir.path().to_path_buf(),
            file_type_filters: vec![],
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Semantic,
            supported_extensions: vec![],
        };

        let handle = start_search(query, None, None);
        let errors = handle.finish().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Semantic search requires"));
    }
}
