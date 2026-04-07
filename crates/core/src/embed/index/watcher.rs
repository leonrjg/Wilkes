use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use tracing::{error, info};

use crate::extract::ExtractorRegistry;
use crate::types::IndexingConfig;
use super::super::Embedder;
use super::SemanticIndex;

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
                        let mut changed_paths = Vec::new();
                        let mut removed_paths = Vec::new();

                        for event in events {
                            if crate::types::FileType::detect(&event.path, &config.supported_extensions).is_none() && event.path.exists() {
                                continue;
                            }

                            if event.path.exists() && event.path.is_file() {
                                changed_paths.push(event.path.clone());
                            } else if !event.path.exists() {
                                removed_paths.push(event.path.clone());
                            }
                        }

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
                            info!("[IndexWatcher] incremental update: {} files changed", changed_paths.len());
                            for path in changed_paths {
                                handle_event(&path, &index, &extractors, &embedder, config.chunk_size, config.chunk_overlap);
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

    match SemanticIndex::prepare_file(path, extractors, embedder.as_ref(), chunk_size, chunk_overlap) {
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
fn try_open_exclusive(path: &std::path::Path, max_attempts: u32, base_delay: Duration) -> anyhow::Result<()> {
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
    use tempfile::tempdir;
    use crate::types::EmbeddingEngine;

    struct StubEmbedder;
    impl Embedder for StubEmbedder {
        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> { Ok(vec![]) }
        fn model_id(&self) -> &str { "stub" }
        fn dimension(&self) -> usize { 1 }
        fn engine(&self) -> EmbeddingEngine { EmbeddingEngine::Candle }
    }

    #[test]
    fn test_index_watcher_start_stop() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder = Arc::new(StubEmbedder);
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
        ).unwrap();

        watcher.stop();
    }

    #[test]
    fn test_index_watcher_invalid_path() {
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder = Arc::new(StubEmbedder);
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
        let embedder: Arc<dyn Embedder> = Arc::new(StubEmbedder);
        
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
        // Should log error but not panic (prepare_file fails because StubEmbedder returns 0 vectors)
    }
}
