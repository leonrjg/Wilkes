use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};
use tracing::error;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryChangeBatch {
    pub root: PathBuf,
    pub changed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Directories that now exist — an external rename target or a moved-in
    /// folder. The OS reports only the directory path, not its descendants, so
    /// consumers expand these to reconcile the files inside.
    pub appeared_dirs: Vec<PathBuf>,
}

impl DirectoryChangeBatch {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && self.appeared_dirs.is_empty()
    }
}

pub fn classify_directory_events(root: PathBuf, events: &[DebouncedEvent]) -> DirectoryChangeBatch {
    let mut batch = DirectoryChangeBatch {
        root,
        changed: Vec::new(),
        removed: Vec::new(),
        appeared_dirs: Vec::new(),
    };

    for event in events {
        if event.path.exists() {
            if event.path.is_dir() {
                batch.appeared_dirs.push(event.path.clone());
            } else if event.path.is_file() {
                batch.changed.push(event.path.clone());
            }
        } else {
            batch.removed.push(event.path.clone());
        }
    }

    batch
}

/// Recursively collect the supported files beneath `dir`, skipping symlinks so
/// the walk is cycle-safe. Used to expand an externally-appeared directory into
/// the descendant files a consumer must reconcile (e.g. re-key after a rename).
pub fn collect_supported_files(
    dir: &Path,
    supported_extensions: &[String],
    out: &mut Vec<PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            error!("[DirectoryWatcher] cannot read {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_supported_files(&path, supported_extensions, out);
        } else if file_type.is_file() && path_has_supported_extension(&path, supported_extensions) {
            out.push(path);
        }
    }
}

pub struct DirectoryWatcher {
    debouncer: Option<notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DirectoryWatcher {
    pub fn start(
        root: PathBuf,
        on_change: impl Fn(DirectoryChangeBatch) + Send + 'static,
    ) -> anyhow::Result<Self> {
        if !root.exists() {
            anyhow::bail!("Directory watch root not found: {}", root.display());
        }
        if !root.is_dir() {
            anyhow::bail!(
                "Directory watch root is not a directory: {}",
                root.display()
            );
        }

        let (tx_events, rx_events) =
            std::sync::mpsc::channel::<notify_debouncer_mini::DebounceEventResult>();

        let mut debouncer = new_debouncer(Duration::from_millis(500), tx_events)
            .map_err(|e| anyhow::anyhow!("Failed to create watcher: {e}"))?;

        debouncer
            .watcher()
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| anyhow::anyhow!("Failed to watch {}: {e}", root.display()))?;

        let thread_root = root.clone();
        let thread = std::thread::spawn(move || {
            for result in &rx_events {
                match result {
                    Ok(events) => {
                        let batch = classify_directory_events(thread_root.clone(), &events);
                        if !batch.is_empty() {
                            on_change(batch);
                        }
                    }
                    Err(e) => error!("[DirectoryWatcher] watch error: {e}"),
                }
            }
        });

        Ok(Self {
            debouncer: Some(debouncer),
            thread: Some(thread),
        })
    }

    pub fn stop(&mut self) {
        drop(self.debouncer.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for DirectoryWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn path_has_supported_extension(path: &Path, supported_extensions: &[String]) -> bool {
    crate::types::FileType::detect(path, supported_extensions).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_mini::DebouncedEventKind;
    use std::sync::mpsc;
    use tempfile::tempdir;

    #[test]
    fn test_directory_watcher_start_stop() {
        let dir = tempdir().unwrap();
        let mut watcher = DirectoryWatcher::start(dir.path().to_path_buf(), |_| {}).unwrap();
        watcher.stop();
    }

    #[test]
    fn test_directory_watcher_invalid_path() {
        let result = DirectoryWatcher::start(
            PathBuf::from("/non/existent/path/for/directory/watcher"),
            |_| {},
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_classify_directory_events_splits_changed_and_removed() {
        let dir = tempdir().unwrap();
        let changed_file = dir.path().join("changed.txt");
        let unsupported_file = dir.path().join("ignored.rs");
        let removed_file = dir.path().join("removed.txt");
        let directory = dir.path().join("folder");

        std::fs::write(&changed_file, "hello").unwrap();
        std::fs::write(&unsupported_file, "hello").unwrap();
        std::fs::create_dir(&directory).unwrap();

        let events = vec![
            DebouncedEvent {
                path: changed_file.clone(),
                kind: DebouncedEventKind::Any,
            },
            DebouncedEvent {
                path: unsupported_file.clone(),
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

        let batch = classify_directory_events(dir.path().to_path_buf(), &events);

        assert_eq!(batch.changed, vec![changed_file, unsupported_file]);
        assert_eq!(batch.removed, vec![removed_file]);
        assert_eq!(batch.appeared_dirs, vec![directory]);
    }

    #[test]
    fn test_directory_watcher_emits_create_remove_and_rename_batches() {
        let dir = tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut watcher = DirectoryWatcher::start(dir.path().to_path_buf(), move |batch| {
            let _ = tx.send(batch);
        })
        .unwrap();

        let created = dir.path().join("created.txt");
        std::fs::write(&created, "hello").unwrap();
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let renamed = dir.path().join("renamed.txt");
        std::fs::rename(&created, &renamed).unwrap();
        let second = rx.recv_timeout(Duration::from_secs(2)).unwrap();

        std::fs::remove_file(&renamed).unwrap();
        let third = rx.recv_timeout(Duration::from_secs(2)).unwrap();

        watcher.stop();

        assert!(!first.is_empty());
        assert!(!second.is_empty());
        assert!(!third.is_empty());
    }
}
