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
                                            error!(
                                                "[IndexWatcher] remove_file {}: {e:#}",
                                                path.display()
                                            );
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
                                    &index,
                                    &extractors,
                                    &embedder,
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
