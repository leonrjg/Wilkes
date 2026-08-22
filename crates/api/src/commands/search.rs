use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::research::SearchLogTracker;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::error;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::Embedder;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::generate::Generator;
use wilkes_core::metadata::cache::{FileIdentity, MetadataCache, MetadataSource};
use wilkes_core::search::grep::GrepSearchProvider;
use wilkes_core::search::semantic::SemanticSearchProvider;
use wilkes_core::search::{SearchOutcome, SearchProvider};
use wilkes_core::types::{
    FileMatches, IndexingConfig, RetrievalSettings, SearchDocument, SearchLogStatus, SearchMode,
    SearchQuery, SearchStats,
};

/// Admission control for corpus-wide scans.
///
/// `GrepSearchProvider` fans every scan out across the global rayon pool, so a
/// single scan already saturates all cores. Running several at once buys no
/// throughput: measured on an 8-core machine, three concurrent corpus searches
/// gave a 1.05x speedup while spending 2.6x the CPU time (76ms -> 201ms of
/// summed worker time), because three fan-outs oversubscribe the same pool.
///
/// Admitting one at a time keeps total work flat and lets each search finish as
/// soon as its own work is done, instead of every search completing together at
/// the end of the batch. Contention was already serialising these scans inside
/// the thread scheduler; this makes the queue explicit and drops the thrash.
const MAX_CONCURRENT_CORPUS_SCANS: usize = 1;

/// Scans below this size do not meaningfully occupy the rayon pool, so they
/// bypass admission entirely and a targeted lookup never queues behind a sweep.
const CORPUS_SCAN_MIN_DOCUMENTS: usize = 8;

struct ScanAdmission {
    available: Mutex<usize>,
    released: std::sync::Condvar,
}

impl ScanAdmission {
    const fn new(permits: usize) -> Self {
        Self {
            available: Mutex::new(permits),
            released: std::sync::Condvar::new(),
        }
    }

    fn acquire(&self) -> ScanPermit<'_> {
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *available == 0 {
            available = self
                .released
                .wait(available)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *available -= 1;
        ScanPermit { admission: self }
    }
}

struct ScanPermit<'a> {
    admission: &'a ScanAdmission,
}

impl Drop for ScanPermit<'_> {
    fn drop(&mut self) {
        let mut available = self
            .admission
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *available += 1;
        drop(available);
        self.admission.released.notify_one();
    }
}

static CORPUS_SCAN_ADMISSION: ScanAdmission = ScanAdmission::new(MAX_CONCURRENT_CORPUS_SCANS);

/// Whether a scan over `document_count` documents must take an admission permit.
fn needs_admission(document_count: usize) -> bool {
    document_count >= CORPUS_SCAN_MIN_DOCUMENTS
}

/// Handle to a running search. Dropping the handle cancels the search.
pub struct SearchHandle {
    pub rx: mpsc::Receiver<FileMatches>,
    worker: JoinHandle<SearchOutcome>,
    log: Option<SearchLogTracker>,
    metadata: Option<SearchMetadata>,
    catalog_elapsed_ms: u64,
    /// Filled in by the worker once it knows how long admission took.
    admission_wait_ms: Arc<AtomicU64>,
}

struct SearchMetadata {
    cache: Arc<Mutex<MetadataCache>>,
    primary_source: MetadataSource,
}

impl SearchHandle {
    pub fn with_metadata(
        mut self,
        cache: Option<Arc<Mutex<MetadataCache>>>,
        primary_source: MetadataSource,
    ) -> Self {
        self.metadata = cache.map(|cache| SearchMetadata {
            cache,
            primary_source,
        });
        self
    }

    pub fn with_catalog_elapsed_ms(mut self, catalog_elapsed_ms: u64) -> Self {
        self.catalog_elapsed_ms = catalog_elapsed_ms;
        self
    }

    pub async fn next(&mut self) -> Option<FileMatches> {
        let mut result = self.rx.recv().await?;
        if let (Some(metadata), Some(identity)) =
            (&self.metadata, FileIdentity::for_path(&result.path))
        {
            result.title = metadata
                .cache
                .lock()
                .ok()
                .and_then(|cache| {
                    cache
                        .get_valid_with_primary(&result.path, identity, metadata.primary_source)
                        .ok()
                })
                .flatten()
                .and_then(|cached| cached.metadata.title)
                .or(result.title);
        }
        Some(result)
    }

    /// Wait for the worker to finish and return any non-fatal errors it collected.
    /// Must only be called after `next()` has returned `None`.
    pub async fn finish(mut self) -> Vec<String> {
        let outcome = self.worker.await.unwrap_or_else(|e| {
            error!("search worker panicked: {e}");
            vec![format!("search worker panicked: {e}")].into()
        });
        if let Some(log) = &mut self.log {
            log.finish(
                SearchLogStatus::Completed,
                0,
                0,
                outcome.errors.first().cloned(),
            );
        }
        outcome.errors
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
        let mut files_with_matches = 0;

        while let Some(fm) = self.next().await {
            total_matches += fm.total_match_count();
            files_with_matches += 1;
            if let Some(log) = &mut self.log {
                log.observe(fm.total_match_count());
            }
            if !on_result(fm).await {
                self.rx.close();
                break;
            }
        }

        let outcome = self.worker.await.unwrap_or_else(|e| {
            error!("search worker panicked: {e}");
            vec![format!("search worker panicked: {e}")].into()
        });
        let admission_wait_ms = self.admission_wait_ms.load(Ordering::Relaxed);
        let elapsed_ms = (started.elapsed().as_millis() as u64).saturating_sub(admission_wait_ms);
        if let Some(log) = &mut self.log {
            let status = if outcome.errors.iter().any(|error| {
                error.starts_with("search failed:") || error.contains("worker panicked")
            }) {
                SearchLogStatus::Failed
            } else {
                SearchLogStatus::Completed
            };
            log.finish(
                status,
                total_matches,
                elapsed_ms,
                outcome.errors.first().cloned(),
            );
        }
        SearchStats {
            files_scanned: outcome.files_scanned.unwrap_or(files_with_matches),
            total_matches,
            catalog_elapsed_ms: self.catalog_elapsed_ms,
            elapsed_ms,
            admission_wait_ms,
            indexed_pdf_reads: outcome.indexed_pdf_reads,
            live_pdf_fallbacks: outcome.live_pdf_fallbacks,
            index_unavailable_fallbacks: outcome.index_unavailable_fallbacks,
            errors: outcome.errors,
            hyde_documents: outcome.hyde_documents,
        }
    }
}

/// Spawn a search and return a `SearchHandle` whose `rx` streams `FileMatches`.
///
/// For `SearchMode::Grep`: `embedder` is ignored. When `grep_use_index` is
/// enabled, `index` supplies stored PDF text and individual unavailable files
/// fall back to live extraction.
/// For `SearchMode::Semantic`: both must be `Some`, otherwise the search returns
/// an immediate error. The desktop validates presence before calling.
#[allow(clippy::too_many_arguments)]
pub fn start_search(
    query: SearchQuery,
    documents: Vec<SearchDocument>,
    catalog_errors: Vec<String>,
    embedder: Option<Arc<dyn Embedder>>,
    index: Option<Arc<Mutex<Option<SemanticIndex>>>>,
    indexing: Option<IndexingConfig>,
    log: Option<SearchLogTracker>,
    retrieval: RetrievalSettings,
    generator: Option<Arc<dyn Generator>>,
    grep_use_index: bool,
) -> SearchHandle {
    let (tx, rx) = mpsc::channel::<FileMatches>(64);
    let admission_wait = Arc::new(AtomicU64::new(0));
    let worker_admission_wait = Arc::clone(&admission_wait);

    let worker = tokio::task::spawn_blocking(move || {
        let admission_wait = worker_admission_wait;
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        let provider: Box<dyn SearchProvider> = match query.mode {
            SearchMode::Semantic => match (embedder, index) {
                (Some(emb), Some(idx)) => Box::new(
                    SemanticSearchProvider::new(
                        emb,
                        idx,
                        indexing.unwrap_or_else(|| IndexingConfig {
                            chunk_size: 1000,
                            chunk_overlap: 200,
                            supported_extensions: query.supported_extensions.clone(),
                        }),
                    )
                    .with_retrieval(retrieval, generator),
                ),
                _ => {
                    return vec![
                        "Semantic search requires a loaded embedder and built index".into()
                    ]
                    .into();
                }
            },
            SearchMode::Grep => {
                // When enabled, let grep read PDF text the index already holds
                // instead of re-extracting each file. `None` keeps every PDF on
                // the live-extraction path.
                let grep_index = if grep_use_index { index } else { None };
                Box::new(GrepSearchProvider::new().with_index(grep_index))
            }
        };

        // Held for the duration of the fan-out; released when this worker ends.
        let admission_started = std::time::Instant::now();
        let _admission = needs_admission(documents.len()).then(|| CORPUS_SCAN_ADMISSION.acquire());
        admission_wait.store(
            admission_started.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );

        let mut outcome = provider
            .search(&query, &registry, tx, &documents)
            .unwrap_or_else(|e| vec![format!("search failed: {e:#}")].into());
        if !catalog_errors.is_empty() {
            let mut errors = catalog_errors;
            errors.append(&mut outcome.errors);
            outcome.errors = errors;
        }
        outcome
    });

    SearchHandle {
        rx,
        worker,
        log,
        metadata: None,
        catalog_elapsed_ms: 0,
        admission_wait_ms: admission_wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use wilkes_core::types::{DocumentMetadata, FileType};

    /// The whole point of admission control is that a second corpus scan waits
    /// rather than competing for the same rayon pool.
    #[test]
    fn admission_lets_only_one_corpus_scan_run_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static ADMISSION: ScanAdmission = ScanAdmission::new(1);
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (live, peak) = (Arc::clone(&live), Arc::clone(&peak));
                std::thread::spawn(move || {
                    let _permit = ADMISSION.acquire();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    live.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "more than one scan held admission at once"
        );
        // Every permit was handed back, so later scans are not locked out.
        assert_eq!(live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn small_scans_bypass_admission_so_targeted_lookups_do_not_queue() {
        assert!(!needs_admission(0));
        assert!(!needs_admission(1));
        assert!(!needs_admission(CORPUS_SCAN_MIN_DOCUMENTS - 1));
        assert!(needs_admission(CORPUS_SCAN_MIN_DOCUMENTS));
        assert!(needs_admission(223));
    }

    /// A queued scan must not bill another search's work to its own
    /// `elapsed_ms`; the wait is reported as `admission_wait_ms` instead.
    #[tokio::test]
    async fn queue_time_is_reported_apart_from_worker_time() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let documents: Vec<_> = (0..CORPUS_SCAN_MIN_DOCUMENTS)
            .map(|i| {
                let path = root.join(format!("doc{i}.txt"));
                fs::write(&path, "hello world").unwrap();
                text_document(path)
            })
            .collect();
        assert!(needs_admission(documents.len()));

        let query = SearchQuery {
            pattern: "hello".to_string(),
            is_regex: false,
            case_sensitive: false,
            root,
            max_results: 100,
            respect_gitignore: true,
            max_file_size: 1024 * 1024,
            context_lines: 0,
            mode: SearchMode::Grep,
            scope: Default::default(),
            supported_extensions: vec!["txt".to_string()],
            collection_id: None,
            tag_ids: Vec::new(),
        };

        // Occupy the only permit so the search below has to queue for it.
        let blocker = CORPUS_SCAN_ADMISSION.acquire();
        let search = tokio::spawn(async move {
            start_search(
                query,
                documents,
                Vec::new(),
                None,
                None,
                None,
                None,
                RetrievalSettings::default(),
                None,
                false,
            )
            .run(|_| async { true })
            .await
        });

        let held = std::time::Duration::from_millis(150);
        tokio::time::sleep(held).await;
        drop(blocker);

        let stats = search.await.unwrap();
        assert!(
            stats.admission_wait_ms >= 100,
            "expected the queued scan to report its wait, got {}ms",
            stats.admission_wait_ms
        );
        // The scan itself greps a handful of tiny files, so once the wait is
        // removed the remaining worker time must be far below the wait.
        assert!(
            stats.elapsed_ms < stats.admission_wait_ms,
            "elapsed_ms {}ms still carries the {}ms queue wait",
            stats.elapsed_ms,
            stats.admission_wait_ms
        );
    }

    fn text_document(path: std::path::PathBuf) -> SearchDocument {
        SearchDocument {
            path,
            file_type: FileType::PlainText,
            title: None,
        }
    }

    #[tokio::test]
    async fn search_handle_enriches_cached_titles_and_preserves_missing_titles() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("paper.txt");
        let uncached_path = dir.path().join("untitled.txt");
        fs::write(&path, "searchable text").unwrap();
        fs::write(&uncached_path, "other searchable text").unwrap();
        let identity = FileIdentity::for_path(&path).unwrap();
        let cache = Arc::new(Mutex::new(MetadataCache::open(dir.path()).unwrap()));
        cache
            .lock()
            .unwrap()
            .upsert(
                &path,
                identity,
                &DocumentMetadata {
                    title: Some("Composed title".into()),
                    ..DocumentMetadata::default()
                },
                MetadataSource::File,
            )
            .unwrap();

        let (tx, rx) = mpsc::channel(2);
        tx.send(FileMatches {
            path: path.clone(),
            file_type: FileType::PlainText,
            title: None,
            field_matches: Vec::new(),
            matches: Vec::new(),
        })
        .await
        .unwrap();
        tx.send(FileMatches {
            path: uncached_path,
            file_type: FileType::PlainText,
            title: None,
            field_matches: Vec::new(),
            matches: Vec::new(),
        })
        .await
        .unwrap();
        drop(tx);
        let worker = tokio::spawn(async { SearchOutcome::default() });
        let mut handle = SearchHandle {
            rx,
            worker,
            log: None,
            metadata: None,
            catalog_elapsed_ms: 0,
            admission_wait_ms: Arc::new(AtomicU64::new(0)),
        }
        .with_metadata(Some(cache), MetadataSource::File);

        let result = handle.next().await.unwrap();
        let uncached_result = handle.next().await.unwrap();

        assert_eq!(result.title.as_deref(), Some("Composed title"));
        assert_eq!(uncached_result.title, None);
    }

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

        let mut handle = start_search(
            query,
            vec![text_document(root.join("test.txt"))],
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );
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

        let handle = start_search(
            query,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );
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

        let handle = start_search(
            query,
            vec![
                text_document(root.join("test1.txt")),
                text_document(root.join("test2.txt")),
            ],
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );

        let stats = handle
            .run(|fm| async move {
                assert!(!fm.matches.is_empty());
                true
            })
            .await;

        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.total_matches, 2);
        assert!(stats.errors.is_empty());
        assert!(stats.hyde_documents.is_empty());
    }

    #[tokio::test]
    async fn search_handle_counts_files_that_have_no_matches() {
        let dir = tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::write(root.join("test.txt"), "haystack").unwrap();

        let query = SearchQuery {
            pattern: "needle".to_string(),
            is_regex: false,
            case_sensitive: false,
            root,
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

        let handle = start_search(
            query,
            vec![text_document(dir.path().join("test.txt"))],
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );

        let stats = handle.run(|_| async { true }).await;

        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.total_matches, 0);
        assert!(stats.errors.is_empty());
    }

    #[tokio::test]
    async fn search_handle_reports_provider_outcome_diagnostics() {
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let worker = tokio::spawn(async {
            SearchOutcome {
                errors: Vec::new(),
                hyde_documents: vec!["The exact generated passage.".to_string()],
                files_scanned: None,
                indexed_pdf_reads: 2,
                live_pdf_fallbacks: 3,
                index_unavailable_fallbacks: 1,
            }
        });
        let handle = SearchHandle {
            rx,
            worker,
            log: None,
            metadata: None,
            catalog_elapsed_ms: 0,
            admission_wait_ms: Arc::new(AtomicU64::new(0)),
        }
        .with_catalog_elapsed_ms(7);

        let stats = handle.run(|_| async { true }).await;

        assert_eq!(stats.hyde_documents, vec!["The exact generated passage."]);
        assert_eq!(stats.catalog_elapsed_ms, 7);
        assert_eq!(stats.indexed_pdf_reads, 2);
        assert_eq!(stats.live_pdf_fallbacks, 3);
        assert_eq!(stats.index_unavailable_fallbacks, 1);
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

        let handle = start_search(
            query,
            vec![
                text_document(root.join("test1.txt")),
                text_document(root.join("test2.txt")),
            ],
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );

        let stats = handle
            .run(|_fm| async move {
                false // Abort immediately
            })
            .await;

        assert!((1..=2).contains(&stats.files_scanned));
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

        let handle = start_search(
            query,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            RetrievalSettings::default(),
            None,
            false,
        );
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
            metadata: None,
            catalog_elapsed_ms: 0,
            admission_wait_ms: Arc::new(AtomicU64::new(0)),
        };
        let errors = handle.finish().await;
        assert!(!errors.is_empty());
        assert!(errors[0].contains("search worker panicked"));
    }
}
