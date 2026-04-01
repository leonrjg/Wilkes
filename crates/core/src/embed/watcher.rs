use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;

use crate::extract::ExtractorRegistry;
use super::Embedder;
use super::index::SemanticIndex;

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
                        for event in events {
                            handle_event(&event.path, &index, &extractors, &embedder);
                        }
                    }
                    Err(e) => {
                        eprintln!("[IndexWatcher] watch error: {e}");
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
) {
    if !path.exists() {
        // File was removed (or renamed away).
        if let Ok(mut guard) = index.lock() {
            if let Some(idx) = guard.as_mut() {
                if let Err(e) = idx.remove_file(path) {
                    eprintln!("[IndexWatcher] remove_file {}: {e:#}", path.display());
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
        eprintln!(
            "[IndexWatcher] skipping {} (file not ready after retries): {e:#}",
            path.display()
        );
        return;
    }

    match SemanticIndex::prepare_file(path, extractors, embedder.as_ref()) {
        Ok(prepared) => {
            if let Ok(mut guard) = index.lock() {
                if let Some(idx) = guard.as_mut() {
                    if let Err(e) = idx.write_file(prepared) {
                        eprintln!("[IndexWatcher] write_file {}: {e:#}", path.display());
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("[IndexWatcher] prepare_file {}: {e:#}", path.display());
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
