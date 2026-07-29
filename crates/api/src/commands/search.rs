use std::sync::{Arc, Mutex};

use crate::research::SearchLogTracker;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::error;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::Embedder;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::search::grep::GrepSearchProvider;
use wilkes_core::search::semantic::SemanticSearchProvider;
use wilkes_core::search::SearchProvider;
use wilkes_core::types::{
    FileMatches, IndexingConfig, SearchLogStatus, SearchMode, SearchQuery, SearchStats,
};

/// Handle to a running search. Dropping the handle cancels the search.
pub struct SearchHandle {
    pub rx: mpsc::Receiver<FileMatches>,
    worker: JoinHandle<Vec<String>>,
    log: Option<SearchLogTracker>,
}

impl SearchHandle {
    pub async fn next(&mut self) -> Option<FileMatches> {
        self.rx.recv().await
    }

    /// Wait for the worker to finish and return any non-fatal errors it collected.
    /// Must only be called after `next()` has returned `None`.
    pub async fn finish(mut self) -> Vec<String> {
        let errors = self.worker.await.unwrap_or_else(|e| {
            error!("search worker panicked: {e}");
            vec![format!("search worker panicked: {e}")]
        });
        if let Some(log) = &mut self.log {
            log.finish(SearchLogStatus::Completed, 0, 0, errors.first().cloned());
        }
        errors
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
            if let Some(log) = &mut self.log {
                log.observe(fm.matches.len());
            }
            if !on_result(fm).await {
                self.rx.close();
                break;
            }
        }

        let errors = self.worker.await.unwrap_or_else(|e| {
            error!("search worker panicked: {e}");
            vec![format!("search worker panicked: {e}")]
        });
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if let Some(log) = &mut self.log {
            let status = if errors.iter().any(|error| {
                error.starts_with("search failed:") || error.contains("worker panicked")
            }) {
                SearchLogStatus::Failed
            } else {
                SearchLogStatus::Completed
            };
            log.finish(status, total_matches, elapsed_ms, errors.first().cloned());
        }
        SearchStats {
            files_scanned,
            total_matches,
            elapsed_ms,
            errors,
        }
    }
}

/// Spawn a search and return a `SearchHandle` whose `rx` streams `FileMatches`.
///
/// For `SearchMode::Grep`: `embedder` and `index` are ignored.
/// For `SearchMode::Semantic`: both must be `Some`, otherwise the search returns
/// an immediate error. The desktop validates presence before calling.
#[allow(clippy::too_many_arguments)]
pub fn start_search(
    query: SearchQuery,
    all_roots: Vec<std::path::PathBuf>,
    all_root_errors: Vec<String>,
    embedder: Option<Arc<dyn Embedder>>,
    index: Option<Arc<Mutex<Option<SemanticIndex>>>>,
    indexing: Option<IndexingConfig>,
    eligible_paths: Option<std::collections::HashSet<std::path::PathBuf>>,
    log: Option<SearchLogTracker>,
) -> SearchHandle {
    let (tx, rx) = mpsc::channel::<FileMatches>(64);

    let worker = tokio::task::spawn_blocking(move || {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        let provider: Box<dyn SearchProvider> = match query.mode {
            SearchMode::Semantic => match (embedder, index) {
                (Some(emb), Some(idx)) => Box::new(SemanticSearchProvider::new(
                    emb,
                    idx,
                    indexing.unwrap_or_else(|| IndexingConfig {
                        chunk_size: 1000,
                        chunk_overlap: 200,
                        supported_extensions: query.supported_extensions.clone(),
                    }),
                )),
                _ => {
                    return vec![
                        "Semantic search requires a loaded embedder and built index".into()
                    ];
                }
            },
            SearchMode::Grep => Box::new(GrepSearchProvider::with_all_roots(
                all_roots,
                all_root_errors,
            )),
        };

        provider
            .search(&query, &registry, tx, eligible_paths.as_ref())
            .unwrap_or_else(|e| vec![format!("search failed: {e:#}")])
    });

    SearchHandle { rx, worker, log }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let mut handle = start_search(query, Vec::new(), Vec::new(), None, None, None, None, None);
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
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Semantic,
            scope: Default::default(),
            supported_extensions: vec![],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let handle = start_search(query, Vec::new(), Vec::new(), None, None, None, None, None);
        let errors = handle.finish().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Semantic search requires"));
    }

    #[tokio::test]
    async fn test_search_handle_run() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("test1.txt"), "hello first").unwrap();
        fs::write(root.join("test2.txt"), "hello second").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let handle = start_search(query, Vec::new(), Vec::new(), None, None, None, None, None);

        let stats = handle
            .run(|fm| async move {
                assert!(!fm.matches.is_empty());
                true
            })
            .await;

        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.total_matches, 2);
        assert!(stats.errors.is_empty());
    }

    #[tokio::test]
    async fn test_search_handle_run_abort() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("test1.txt"), "hello first").unwrap();
        fs::write(root.join("test2.txt"), "hello second").unwrap();

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let handle = start_search(query, Vec::new(), Vec::new(), None, None, None, None, None);

        let stats = handle
            .run(|_fm| async move {
                false // Abort immediately
            })
            .await;

        assert_eq!(stats.files_scanned, 1);
        assert!(stats.total_matches >= 1);
    }

    #[tokio::test]
    async fn test_start_search_provider_error() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();

        // Invalid regex will cause GrepSearchProvider to fail
        let query = SearchQuery {
            pattern: "[".to_string(),
            is_regex: true,
            case_sensitive: false,
            root: root.clone(),
            max_results: 10,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        let handle = start_search(query, Vec::new(), Vec::new(), None, None, None, None, None);
        let errors = handle.finish().await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("search failed"));
    }

    #[tokio::test]
    async fn test_search_handle_worker_panic() {
        let (_tx, rx) = mpsc::channel(1);
        let worker = tokio::task::spawn_blocking(|| {
            panic!("test panic");
        });
        let handle = SearchHandle {
            rx,
            worker,
            log: None,
        };
        let errors = handle.finish().await;
        assert!(!errors.is_empty());
        assert!(errors[0].contains("search worker panicked"));
    }
}
