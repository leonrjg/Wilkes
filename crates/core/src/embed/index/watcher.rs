use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use tracing::{error, info};

use crate::extract::ExtractorRegistry;
use super::super::Embedder;
use super::SemanticIndex;

// ── IndexWatcher ──────────────────────────────────────────────────────────────

pub struct WatcherConfig {
    pub manager: crate::embed::worker::manager::WorkerManager,
    pub model_id: String,
    pub data_dir: PathBuf,
    pub device: String,
    pub supported_extensions: Vec<String>,
}

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
        embedder: Option<Arc<dyn Embedder>>,
        config: Option<WatcherConfig>,
        chunk_size: usize,
        chunk_overlap: usize,
        supported_extensions: Vec<String>,
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
                            // Only care about supported extensions
                            if crate::types::FileType::detect(&event.path, &supported_extensions).is_none() && event.path.exists() {
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
                            if let Some(ref emb) = embedder {
                                for path in changed_paths {
                                    handle_event(&path, &index, &extractors, emb, chunk_size, chunk_overlap);
                                }
                            } else if let Some(ref cfg) = config {
                                // SBERT engine: spawn worker for the batch
                                info!("[IndexWatcher] Spawning SBERT worker for incremental update: {} files", changed_paths.len());
                                if let Err(e) = spawn_sbert_worker(cfg, &changed_paths, chunk_size, chunk_overlap) {
                                    error!("[IndexWatcher] Failed to spawn SBERT worker: {e:#}");
                                }
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

fn spawn_sbert_worker(
    cfg: &WatcherConfig,
    paths: &[PathBuf],
    chunk_size: usize,
    chunk_overlap: usize,
) -> anyhow::Result<()> {
    use crate::embed::worker::ipc::WorkerRequest;
    use crate::types::EmbeddingEngine;

    let request = WorkerRequest {
        mode: "build".to_string(),
        root: PathBuf::new(), // Not used for incremental
        engine: EmbeddingEngine::SBERT,
        model: cfg.model_id.clone(),
        data_dir: cfg.data_dir.clone(),
        chunk_size: Some(chunk_size),
        chunk_overlap: Some(chunk_overlap),
        device: cfg.device.clone(),
        paths: Some(paths.to_vec()),
        texts: None,
        supported_extensions: cfg.supported_extensions.clone(),
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let cmd = crate::embed::worker::manager::ManagerCommand::Submit {
        req: request,
        reply: tx,
    };

    cfg.manager.blocking_send(cmd)
        .map_err(|e| anyhow::anyhow!("Failed to send to worker manager: {e}"))?;

    // Wait for worker to finish (incremental updates should be fast)
    while let Some(event) = rx.blocking_recv() {
        match event {
            crate::embed::worker::ipc::WorkerEvent::Done => break,
            crate::embed::worker::ipc::WorkerEvent::Error(e) => anyhow::bail!("SBERT worker error: {e}"),
            _ => {}
        }
    }

    Ok(())
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

    #[test]
    fn test_index_watcher_start_stop() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        
        let mut watcher = IndexWatcher::start(
            dir.path().to_path_buf(),
            index,
            registry,
            None,
            None,
            100,
            10,
            vec!["txt".to_string()],
            || {},
            || {},
        ).unwrap();

        watcher.stop();
    }
}
