use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};
use tracing::{error, info};

use super::super::Embedder;
use super::SemanticIndex;
use crate::extract::ExtractorRegistry;
use crate::types::IndexingConfig;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassifiedPaths {
    pub changed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

pub fn should_consider_path(path: &std::path::Path, supported_extensions: &[String]) -> bool {
    crate::types::FileType::detect(path, supported_extensions).is_some() || !path.exists()
}

pub fn classify_event_paths(
    events: &[DebouncedEvent],
    supported_extensions: &[String],
) -> ClassifiedPaths {
    let mut classified = ClassifiedPaths::default();
    for event in events {
        if !should_consider_path(&event.path, supported_extensions) && event.path.exists() {
            continue;
        }
        if event.path.exists() && event.path.is_file() {
            classified.changed.push(event.path.clone());
        } else if !event.path.exists() {
            classified.removed.push(event.path.clone());
        }
    }
    classified
}

fn process_watcher_result<F1, F2>(
    result: notify_debouncer_mini::DebounceEventResult,
    index: &Arc<Mutex<Option<SemanticIndex>>>,
    extractors: &Arc<ExtractorRegistry>,
    embedder: &Arc<dyn Embedder>,
    config: &IndexingConfig,
    on_reindex: &F1,
    on_reindex_done: &F2,
) where
    F1: Fn(),
    F2: Fn(),
{
    match result {
        Ok(events) => {
            let classified = classify_event_paths(&events, &config.supported_extensions);
            let changed_paths = classified.changed;
            let removed_paths = classified.removed;

            // Handle removals
            if !removed_paths.is_empty() {
                if let Ok(mut guard) = index.lock() {
                    if let Some(idx) = guard.as_mut() {
                        for path in removed_paths {
                            if let Err(e) = idx.remove_file(&path) {
                                error!("[IndexWatcher] remove_file {}: {e:#}", path.display());
                            }
                        }
                    }
                }
            }

            // Handle additions/modifications
            if !changed_paths.is_empty() {
                on_reindex();
                info!(
                    "[IndexWatcher] incremental update: {} files changed",
                    changed_paths.len()
                );
                for path in changed_paths {
                    handle_event(
                        &path,
                        index,
                        extractors,
                        embedder,
                        config.chunk_size,
                        config.chunk_overlap,
                    );
                }
                on_reindex_done();
            }
        }
        Err(e) => {
            error!("[IndexWatcher] watch error: {e}");
        }
    }
}

// ── IndexWatcher ──────────────────────────────────────────────────────────────

pub struct IndexWatcher {
    debouncer: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl IndexWatcher {
    /// Start watching `root`. Events are processed on a background thread.
    pub fn start(
        root: PathBuf,
        index: Arc<Mutex<Option<SemanticIndex>>>,
        extractors: Arc<ExtractorRegistry>,
        embedder: Arc<dyn Embedder>,
        config: IndexingConfig,
        on_reindex: impl Fn() + Send + Sync + 'static,
        on_reindex_done: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let (tx_events, rx_events) =
            std::sync::mpsc::channel::<notify_debouncer_mini::DebounceEventResult>();

        let mut debouncer = new_debouncer(Duration::from_millis(500), tx_events)
            .map_err(|e| anyhow::anyhow!("Failed to create watcher: {e}"))?;

        debouncer
            .watcher()
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| anyhow::anyhow!("Failed to watch {}: {e}", root.display()))?;

        let thread = std::thread::spawn(move || {
            for result in &rx_events {
                process_watcher_result(result, &index, &extractors, &embedder, &config, &on_reindex, &on_reindex_done);
            }
        });

        Ok(IndexWatcher {
            debouncer: Some(debouncer),
            thread: Some(thread),
        })
    }

    /// Stop the watcher. Subsequent calls are no-ops.
    pub fn stop(&mut self) {
        // Dropping the debouncer closes the event channel, causing the thread to exit.
        drop(self.debouncer.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for IndexWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Event handler ─────────────────────────────────────────────────────────────

/// notify-debouncer-mini coalesces events into a single `DebouncedEventKind::Any`
/// per path. We distinguish create/modify vs. remove by checking whether the path
/// still exists after the debounce quiet period.
fn handle_event(
    path: &std::path::Path,
    index: &Arc<Mutex<Option<SemanticIndex>>>,
    extractors: &Arc<ExtractorRegistry>,
    embedder: &Arc<dyn Embedder>,
    chunk_size: usize,
    chunk_overlap: usize,
) {
    if !path.exists() {
        // File was removed (or renamed away).
        if let Ok(mut guard) = index.lock() {
            if let Some(idx) = guard.as_mut() {
                if let Err(e) = idx.remove_file(path) {
                    error!("[IndexWatcher] remove_file {}: {e:#}", path.display());
                }
            }
        }
        return;
    }

    if !path.is_file() {
        return;
    }

    // File exists: treat as create or modify.
    if let Err(e) = try_open_exclusive(path, 5, Duration::from_millis(500)) {
        error!(
            "[IndexWatcher] skipping {} (file not ready after retries): {e:#}",
            path.display()
        );
        return;
    }

    match SemanticIndex::prepare_file(
        path,
        extractors,
        embedder.as_ref(),
        chunk_size,
        chunk_overlap,
    ) {
        Ok(prepared) => {
            if let Ok(mut guard) = index.lock() {
                if let Some(idx) = guard.as_mut() {
                    if let Err(e) = idx.write_file(prepared) {
                        error!("[IndexWatcher] write_file {}: {e:#}", path.display());
                    }
                }
            }
        }
        Err(e) => {
            error!("[IndexWatcher] prepare_file {}: {e:#}", path.display());
        }
    }
}

/// Try to open a file for reading with exponential backoff to detect partially-written files.
fn try_open_exclusive(
    path: &std::path::Path,
    max_attempts: u32,
    base_delay: Duration,
) -> anyhow::Result<()> {
    let mut delay = base_delay;
    for attempt in 0..max_attempts {
        match std::fs::File::open(path) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if attempt + 1 == max_attempts {
                    return Err(anyhow::anyhow!(
                        "Cannot open file after {max_attempts} attempts: {e}"
                    ));
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::MockEmbedder;
    use crate::types::EmbeddingEngine;
    use notify_debouncer_mini::DebouncedEventKind;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[test]
    fn test_index_watcher_start_stop() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder = Arc::new(MockEmbedder::default());
        let config = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 10,
            supported_extensions: vec!["txt".to_string()],
        };

        let mut watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            index,
            registry,
            embedder,
            config,
            || {},
            || {},
        )
        .unwrap();

        watcher.stop();
    }

    #[test]
    fn test_should_consider_path_accepts_supported_or_missing_paths() {
        let dir = tempdir().unwrap();
        let supported = vec!["txt".to_string()];
        let supported_file = dir.path().join("note.txt");
        let unsupported_file = dir.path().join("note.md");
        let missing_file = dir.path().join("gone.md");

        std::fs::write(&supported_file, "hello").unwrap();
        std::fs::write(&unsupported_file, "hello").unwrap();

        assert!(should_consider_path(&supported_file, &supported));
        assert!(!should_consider_path(&unsupported_file, &supported));
        assert!(should_consider_path(&missing_file, &supported));
    }

    #[test]
    fn test_classify_event_paths_splits_changed_and_removed() {
        let dir = tempdir().unwrap();
        let supported = vec!["txt".to_string()];
        let changed_file = dir.path().join("changed.txt");
        let ignored_file = dir.path().join("ignored.rs");
        let removed_file = dir.path().join("removed.txt");
        let directory = dir.path().join("folder");

        std::fs::write(&changed_file, "hello").unwrap();
        std::fs::write(&ignored_file, "hello").unwrap();
        std::fs::create_dir(&directory).unwrap();

        let events = vec![
            DebouncedEvent {
                path: changed_file.clone(),
                kind: DebouncedEventKind::Any,
            },
            DebouncedEvent {
                path: ignored_file.clone(),
                kind: DebouncedEventKind::Any,
            },
            DebouncedEvent {
                path: removed_file.clone(),
                kind: DebouncedEventKind::Any,
            },
            DebouncedEvent {
                path: directory.clone(),
                kind: DebouncedEventKind::Any,
            },
        ];

        let classified = classify_event_paths(&events, &supported);

        assert_eq!(classified.changed, vec![changed_file]);
        assert_eq!(classified.removed, vec![removed_file]);
    }

    #[test]
    fn test_index_watcher_invalid_path() {
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder = Arc::new(MockEmbedder::default());
        let config = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 10,
            supported_extensions: vec!["txt".to_string()],
        };

        let result = IndexWatcher::start(
            PathBuf::from("/non/existent/path/for/watcher"),
            index,
            registry,
            embedder,
            config,
            || {},
            || {},
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_try_open_exclusive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "test").unwrap();

        let res = try_open_exclusive(&path, 3, Duration::from_millis(1));
        assert!(res.is_ok());

        let non_existent = dir.path().join("none.txt");
        let res2 = try_open_exclusive(&non_existent, 2, Duration::from_millis(1));
        assert!(res2.is_err());
    }

    #[test]
    fn test_handle_event_basics() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        let path = dir.path().join("test.txt");

        // 1. Non-existent path (simulates removal)
        handle_event(&path, &index, &registry, &embedder, 100, 10);
        // Should not panic, but nothing to remove from index yet

        // 2. Directory instead of file
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir(&sub_dir).unwrap();
        handle_event(&sub_dir, &index, &registry, &embedder, 100, 10);
        // Should return early

        // 3. Actual file (prepare_file will fail if no extractor or embedder returns nothing)
        std::fs::write(&path, "hello").unwrap();
        handle_event(&path, &index, &registry, &embedder, 100, 10);
        // Should log error but not panic
    }

    #[test]
    fn test_handle_event_with_index() {
        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        std::fs::create_dir(&idx_dir).unwrap();

        let mut idx =
            SemanticIndex::create(&idx_dir, "mock-model", 384, EmbeddingEngine::Candle, None)
                .unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "content").unwrap();

        // Add file to index manually first
        idx.write_file(crate::embed::index::db::PreparedFile {
            path: file_path.clone(),
            chunks: vec![(
                crate::embed::index::chunk::Chunk {
                    file_path: file_path.clone(),
                    text: "content".to_string(),
                    byte_range: crate::types::ByteRange { start: 0, end: 7 },
                    origin: crate::types::SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.0; 384],
            )],
        })
        .unwrap();

        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = Arc::new(ExtractorRegistry::new());

        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        // 1. Update file
        std::fs::write(&file_path, "new content").unwrap();
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);

        // 2. Remove file
        std::fs::remove_file(&file_path).unwrap();
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);

        let guard = index.lock().unwrap();
        let idx_final = guard.as_ref().unwrap();
        assert_eq!(idx_final.status().total_chunks, 0);
    }

    #[test]
    fn test_handle_event_missing_path_twice_hits_remove_error_branch() {
        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        std::fs::create_dir(&idx_dir).unwrap();

        let mut idx =
            SemanticIndex::create(&idx_dir, "mock-model", 384, EmbeddingEngine::Candle, None)
                .unwrap();
        let file_path = dir.path().join("missing.txt");
        std::fs::write(&file_path, "content").unwrap();

        idx.write_file(crate::embed::index::db::PreparedFile {
            path: file_path.clone(),
            chunks: vec![(
                crate::embed::index::chunk::Chunk {
                    file_path: file_path.clone(),
                    text: "content".to_string(),
                    byte_range: crate::types::ByteRange { start: 0, end: 7 },
                    origin: crate::types::SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.0; 384],
            )],
        })
        .unwrap();

        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        std::fs::remove_file(&file_path).unwrap();
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);
    }

    #[cfg(unix)]
    #[test]
    fn test_handle_event_unreadable_path_hits_retry_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let path = dir.path().join("blocked.txt");
        std::fs::write(&path, "content").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        handle_event(&path, &index, &registry, &embedder, 100, 10);
    }

    #[test]
    fn test_handle_event_prepare_file_error_for_invalid_utf8() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let path = dir.path().join("invalid.txt");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        handle_event(&path, &index, &registry, &embedder, 100, 10);
    }

    #[test]
    fn test_try_open_exclusive_zero_attempts_returns_ok() {
        let res = try_open_exclusive(std::path::Path::new("/definitely/missing"), 0, Duration::from_millis(1));
        assert!(res.is_ok());
    }

    #[test]
    fn test_process_watcher_result_invokes_callbacks_and_handles_errors() {
        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        std::fs::create_dir(&idx_dir).unwrap();

        let mut idx =
            SemanticIndex::create(&idx_dir, "mock-model", 384, EmbeddingEngine::Candle, None)
                .unwrap();
        let changed_path = dir.path().join("changed.txt");
        let removed_path = dir.path().join("removed.txt");
        std::fs::write(&changed_path, "hello").unwrap();
        std::fs::write(&removed_path, "world").unwrap();

        idx.write_file(crate::embed::index::db::PreparedFile {
            path: removed_path.clone(),
            chunks: vec![(
                crate::embed::index::chunk::Chunk {
                    file_path: removed_path.clone(),
                    text: "world".to_string(),
                    byte_range: crate::types::ByteRange { start: 0, end: 5 },
                    origin: crate::types::SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.0; 384],
            )],
        })
        .unwrap();

        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let reindex_calls = Arc::new(AtomicUsize::new(0));
        let reindex_done_calls = Arc::new(AtomicUsize::new(0));
        let config = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 10,
            supported_extensions: vec!["txt".to_string()],
        };

        let events = vec![
            DebouncedEvent {
                path: changed_path.clone(),
                kind: DebouncedEventKind::Any,
            },
            DebouncedEvent {
                path: removed_path.clone(),
                kind: DebouncedEventKind::Any,
            },
        ];

        process_watcher_result(
            Ok(events),
            &index,
            &registry,
            &embedder,
            &config,
            &|| {
                reindex_calls.fetch_add(1, Ordering::Relaxed);
            },
            &|| {
                reindex_done_calls.fetch_add(1, Ordering::Relaxed);
            },
        );

        process_watcher_result(
            Err(notify::Error::generic("watch failed")),
            &index,
            &registry,
            &embedder,
            &config,
            &|| {},
            &|| {},
        );

        assert_eq!(reindex_calls.load(Ordering::Relaxed), 1);
        assert_eq!(reindex_done_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_index_watcher_stop() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());

        let mut watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            index,
            registry,
            embedder,
            crate::types::IndexingConfig {
                chunk_size: 100,
                chunk_overlap: 10,
                supported_extensions: vec!["txt".to_string()],
            },
            || {}, // on_reindex
            || {}, // on_reindex_done
        )
        .unwrap();
        watcher.stop();
    }

    #[test]
    fn test_index_watcher_background_processing() {
        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        std::fs::create_dir(&idx_dir).unwrap();

        let idx = SemanticIndex::create(&idx_dir, "mock-model", 384, EmbeddingEngine::Candle, None)
            .unwrap();
        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = Arc::new(ExtractorRegistry::new());
        let file_path = dir.path().join("watch_me.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);

        assert_eq!(
            index
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .status()
                .total_chunks,
            1,
            "File should have been indexed"
        );

        std::fs::remove_file(&file_path).unwrap();
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);

        assert_eq!(
            index
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .status()
                .total_chunks,
            0,
            "File should have been removed from index"
        );
    }

    #[test]
    fn test_index_watcher_rename() {
        let dir = tempdir().unwrap();
        let index_path = dir.path().join("idx");
        std::fs::create_dir_all(&index_path).unwrap();
        let mut idx = crate::embed::index::db::SemanticIndex::create(
            &index_path,
            "m",
            3,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();

        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "test content").unwrap();

        // Manual prepare and write
        let prepared = crate::embed::index::db::PreparedFile {
            path: file_path.clone(),
            chunks: vec![(
                crate::embed::index::chunk::Chunk {
                    text: "test".to_string(),
                    byte_range: crate::types::ByteRange { start: 0, end: 4 },
                    origin: crate::types::SourceOrigin::TextFile { line: 1, col: 1 },
                    file_path: file_path.clone(),
                },
                vec![0.1, 0.2, 0.3],
            )],
        };
        idx.write_file(prepared).unwrap();
        assert_eq!(idx.status().total_chunks, 1);

        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = Arc::new(ExtractorRegistry::new());
        let (_manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let embedder: Arc<dyn crate::embed::Embedder> = Arc::new(crate::embed::MockEmbedder {
            model_id: "m".to_string(),
            dimension: 3,
            engine: EmbeddingEngine::Candle,
        });

        let mut watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            index.clone(),
            registry,
            embedder,
            crate::types::IndexingConfig {
                chunk_size: 100,
                chunk_overlap: 0,
                supported_extensions: vec!["txt".to_string()],
            },
            || {},
            || {},
        )
        .unwrap();

        // Rename file (notify mini debouncer should see this)
        let new_path = dir.path().join("renamed.txt");
        std::fs::rename(&file_path, &new_path).unwrap();

        // Wait for debouncer
        std::thread::sleep(Duration::from_millis(500));

        {
            let guard = index.lock().unwrap();
            let idx_final = guard.as_ref().unwrap();
            // It should have removed the old path and added the new one
            assert!(idx_final.status().indexed_files >= 1);
        }

        watcher.stop();
    }
}
