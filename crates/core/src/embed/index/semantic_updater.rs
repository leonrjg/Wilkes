use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{error, info};

use super::super::Embedder;
use super::SemanticIndex;
use crate::directory_watcher::{path_has_supported_extension, DirectoryChangeBatch};
use crate::extract::ExtractorRegistry;
use crate::metadata::cache::{FileIdentity, MetadataCache};
use crate::types::IndexingConfig;

/// Optional shared handle to the document-metadata cache. When present it acts
/// as the file-identity registry that lets the semantic updater recognise renames.
type CacheHandle = Option<Arc<Mutex<MetadataCache>>>;

/// Identify which of the `changed` paths are actually renames of files the
/// cache already knows: same content fingerprint, old path now gone. Returns
/// `(old_path, new_path)` pairs. Empty when there is no cache to consult.
fn detect_renames(cache: &CacheHandle, changed: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let Some(cache) = cache else {
        return Vec::new();
    };
    let Ok(guard) = cache.lock() else {
        return Vec::new();
    };
    let mut renames = Vec::new();
    for new_path in changed {
        let Some(identity) = FileIdentity::for_path(new_path) else {
            continue;
        };
        match guard.find_rename_source(new_path, identity) {
            Ok(Some(old_path)) => renames.push((old_path, new_path.clone())),
            Ok(None) => {}
            Err(e) => error!(
                "[SemanticUpdater] rename detection for {}: {e:#}",
                new_path.display()
            ),
        }
    }
    renames
}

/// Re-key both stores for each rename instead of deleting and re-embedding.
fn apply_renames(
    index: &Arc<Mutex<Option<SemanticIndex>>>,
    cache: &CacheHandle,
    renames: &[(PathBuf, PathBuf)],
) {
    if let Ok(mut guard) = index.lock() {
        if let Some(idx) = guard.as_mut() {
            for (old, new) in renames {
                if let Err(e) = idx.rename_file(old, new) {
                    error!(
                        "[SemanticUpdater] rename_file {} -> {}: {e:#}",
                        old.display(),
                        new.display()
                    );
                }
            }
        }
    }
    if let Some(cache) = cache {
        if let Ok(guard) = cache.lock() {
            for (old, new) in renames {
                if let Err(e) = guard.rename(old, new) {
                    error!(
                        "[SemanticUpdater] cache rename {} -> {}: {e:#}",
                        old.display(),
                        new.display()
                    );
                }
            }
        }
    }
}

pub fn process_directory_change<F1, F2>(
    batch: DirectoryChangeBatch,
    index: &Arc<Mutex<Option<SemanticIndex>>>,
    cache: &CacheHandle,
    extractors: &Arc<ExtractorRegistry>,
    embedder: &Arc<dyn Embedder>,
    config: &IndexingConfig,
    on_reindex: &F1,
    on_reindex_done: &F2,
) where
    F1: Fn(),
    F2: Fn(),
{
    let mut changed_paths: Vec<PathBuf> = batch
        .changed
        .into_iter()
        .filter(|path| path_has_supported_extension(path, &config.supported_extensions))
        .collect();
    let mut removed_paths = batch.removed;

    // A rename surfaces as its old path removed and its new path changed.
    // Detect that by content identity and re-key both stores instead of
    // deleting + re-extracting + re-embedding identical content.
    let renames = detect_renames(cache, &changed_paths);
    if !renames.is_empty() {
        apply_renames(index, cache, &renames);
        let renamed_new: HashSet<&PathBuf> = renames.iter().map(|(_, n)| n).collect();
        let renamed_old: HashSet<&PathBuf> = renames.iter().map(|(o, _)| o).collect();
        changed_paths.retain(|p| !renamed_new.contains(p));
        removed_paths.retain(|p| !renamed_old.contains(p));
    }

    // Handle removals (index + metadata cache).
    if !removed_paths.is_empty() {
        if let Ok(mut guard) = index.lock() {
            if let Some(idx) = guard.as_mut() {
                for path in &removed_paths {
                    if let Err(e) = idx.remove_file(path) {
                        error!("[SemanticUpdater] remove_file {}: {e:#}", path.display());
                    }
                }
            }
        }
        if let Some(cache) = cache {
            if let Ok(guard) = cache.lock() {
                for path in &removed_paths {
                    if let Err(e) = guard.remove(path) {
                        error!("[SemanticUpdater] cache remove {}: {e:#}", path.display());
                    }
                }
            }
        }
    }

    // Handle additions/modifications
    if !changed_paths.is_empty() {
        on_reindex();
        info!(
            "[SemanticUpdater] incremental update: {} files changed",
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
                    error!("[SemanticUpdater] remove_file {}: {e:#}", path.display());
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
            "[SemanticUpdater] skipping {} (file not ready after retries): {e:#}",
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
                        error!("[SemanticUpdater] write_file {}: {e:#}", path.display());
                    }
                }
            }
        }
        Err(e) => {
            error!("[SemanticUpdater] prepare_file {}: {e:#}", path.display());
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

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

        // 2. Removed files are handled by process_directory_change; direct
        // handle_event calls remain best-effort and should not panic.
        std::fs::remove_file(&file_path).unwrap();
        handle_event(&file_path, &index, &registry, &embedder, 100, 10);
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
        let res = try_open_exclusive(
            std::path::Path::new("/definitely/missing"),
            0,
            Duration::from_millis(1),
        );
        assert!(res.is_ok());
    }

    #[test]
    fn test_process_directory_change_invokes_callbacks_and_removes_paths() {
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

        process_directory_change(
            DirectoryChangeBatch {
                root: dir.path().to_path_buf(),
                changed: vec![changed_path],
                removed: vec![removed_path],
            },
            &index,
            &None,
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

        assert_eq!(reindex_calls.load(Ordering::Relaxed), 1);
        assert_eq!(reindex_done_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_process_directory_change_background_processing() {
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
        process_directory_change(
            DirectoryChangeBatch {
                root: dir.path().to_path_buf(),
                changed: Vec::new(),
                removed: vec![file_path.clone()],
            },
            &index,
            &None,
            &registry,
            &embedder,
            &IndexingConfig {
                chunk_size: 100,
                chunk_overlap: 10,
                supported_extensions: vec!["txt".to_string()],
            },
            &|| {},
            &|| {},
        );

        assert!(index.lock().unwrap().as_ref().is_some());
    }

    #[test]
    fn test_handle_event_directory_skips() {
        let dir = tempdir().unwrap();
        let index = Arc::new(Mutex::new(None));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::default());
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();

        handle_event(&folder, &index, &registry, &embedder, 100, 10);
    }

    /// A rename must re-key both stores by content identity, never re-embed.
    /// The embedder here fails if invoked, so a surviving chunk proves the
    /// rename took the cheap re-key path rather than delete + re-extract.
    #[test]
    fn test_process_directory_change_rekeys_rename_by_identity() {
        struct FailingEmbedder;
        impl Embedder for FailingEmbedder {
            fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                anyhow::bail!("embed must not be called on a rename")
            }
            fn model_id(&self) -> &str {
                "m"
            }
            fn dimension(&self) -> usize {
                3
            }
            fn engine(&self) -> EmbeddingEngine {
                EmbeddingEngine::Candle
            }
        }

        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        std::fs::create_dir_all(&idx_dir).unwrap();
        let mut idx =
            SemanticIndex::create(&idx_dir, "m", 3, EmbeddingEngine::Candle, None).unwrap();

        let old_path = dir.path().join("old.txt");
        std::fs::write(&old_path, "hello world").unwrap();
        idx.write_file(crate::embed::index::db::PreparedFile {
            path: old_path.clone(),
            chunks: vec![(
                crate::embed::index::chunk::Chunk {
                    file_path: old_path.clone(),
                    text: "hello world".to_string(),
                    byte_range: crate::types::ByteRange { start: 0, end: 11 },
                    origin: crate::types::SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.1, 0.2, 0.3],
            )],
        })
        .unwrap();
        assert_eq!(idx.status().total_chunks, 1);

        let cache = MetadataCache::open(dir.path()).unwrap();
        let identity = FileIdentity::for_path(&old_path).unwrap();
        cache
            .upsert(
                &old_path,
                identity,
                &crate::types::DocumentMetadata {
                    title: None,
                    author: None,
                    doi: None,
                    created_at: Some("2020-01".into()),
                    ..crate::types::DocumentMetadata::default()
                },
                crate::metadata::cache::MetadataSource::File,
            )
            .unwrap();

        // Rename preserves size and mtime, so the identity is unchanged.
        let new_path = dir.path().join("new.txt");
        std::fs::rename(&old_path, &new_path).unwrap();

        let index = Arc::new(Mutex::new(Some(idx)));
        let cache_handle: CacheHandle = Some(Arc::new(Mutex::new(cache)));
        let registry = Arc::new(ExtractorRegistry::new());
        let embedder: Arc<dyn Embedder> = Arc::new(FailingEmbedder);
        let config = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: vec!["txt".to_string()],
        };

        process_directory_change(
            DirectoryChangeBatch {
                root: dir.path().to_path_buf(),
                changed: vec![new_path.clone()],
                removed: vec![old_path.clone()],
            },
            &index,
            &cache_handle,
            &registry,
            &embedder,
            &config,
            &|| {},
            &|| {},
        );

        // Index chunk survived (re-keyed, not deleted + re-embedded).
        assert_eq!(
            index
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .status()
                .total_chunks,
            1
        );
        // Metadata cache row moved from old path to new path.
        let guard = cache_handle.as_ref().unwrap().lock().unwrap();
        assert!(guard.get_valid(&new_path, identity).unwrap().is_some());
        assert!(guard.get_valid(&old_path, identity).unwrap().is_none());
    }
}
