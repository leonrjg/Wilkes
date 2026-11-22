//! Upkeep of the full-text index.

use super::*;

/// What the document text index has been asked to catch up on.
#[derive(Default)]
pub(super) struct TextIndexRequests {
    /// Compare with the whole library. Subsumes any batches.
    pub(super) library: bool,
    pub(super) batches: Vec<DirectoryChangeBatch>,
}

impl TextIndexRequests {
    pub(super) fn is_empty(&self) -> bool {
        !self.library && self.batches.is_empty()
    }
}

#[derive(Default)]
pub(super) struct TextIndexUpkeep {
    pub(super) requests: PLMutex<TextIndexRequests>,
    pub(super) running: AtomicBool,
    pub(super) cancel: PLMutex<Arc<AtomicBool>>,
}

/// The library's roots as configured, before any of them is checked: the open
/// directory, the favorites and the recent directories.
/// What the indexing toast reads. Emitted by full builds and by the watcher's
/// passes alike, so it shows a count whichever is rewriting the index.
pub(super) fn emit_reindex_progress(events: &dyn EventEmitter, progress: ReindexProgress) {
    match serde_json::to_value(progress) {
        Ok(payload) => events.emit("reindex-progress", payload),
        Err(e) => error!("could not serialise reindex progress {progress:?}: {e}"),
    }
}

impl AppContext {
    /// Lazily open (once) the document text index. `None` when it cannot be
    /// opened; exact search then extracts live and counts the files it did.
    pub(super) fn text_index(&self) -> Option<Arc<DocumentTextIndex>> {
        let mut guard = self.text_index.lock();
        if let Some(store) = guard.as_ref() {
            return Some(Arc::clone(store));
        }
        match DocumentTextIndex::open(&self.data_dir) {
            Ok(store) => {
                let store = Arc::new(store);
                *guard = Some(Arc::clone(&store));
                Some(store)
            }
            Err(e) => {
                error!("document text index open failed: {e:#}");
                None
            }
        }
    }
    /// Where exact search reads paged documents' text from.
    pub(super) fn stored_text(&self, settings: &Settings) -> StoredText {
        if !settings.grep_use_index {
            return StoredText::Disabled;
        }
        match self.text_index() {
            Some(store) => StoredText::Available(store),
            None => StoredText::Unavailable,
        }
    }
    /// Ask for the document text index to be compared with the whole library.
    pub(super) fn request_text_index_reconcile(self: &Arc<Self>) {
        self.request_text_index_upkeep(|requests| requests.library = true);
    }
    /// Ask for one watcher batch to be applied to the document text index.
    pub(super) fn request_text_index_batch(self: &Arc<Self>, batch: DirectoryChangeBatch) {
        self.request_text_index_upkeep(|requests| requests.batches.push(batch));
    }
    /// Queue upkeep, and start the runner unless one is running: there is
    /// never more than one writer of the store, and requests that arrive while
    /// it works are merged and taken on its next turn.
    pub(super) fn request_text_index_upkeep(
        self: &Arc<Self>,
        add: impl FnOnce(&mut TextIndexRequests),
    ) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        add(&mut self.text_index_upkeep.requests.lock());
        if self.text_index_upkeep.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let ctx = Arc::clone(self);
        tokio::spawn(async move { ctx.run_text_index_upkeep().await });
    }
    /// Stop the pass in progress and drop what was queued. Returns at once;
    /// a document being read when the flag is raised finishes in the
    /// background, and nothing after it starts.
    pub(super) fn cancel_text_index_upkeep(&self) {
        *self.text_index_upkeep.requests.lock() = TextIndexRequests::default();
        self.text_index_upkeep
            .cancel
            .lock()
            .store(true, Ordering::Relaxed);
    }
    pub(super) async fn run_text_index_upkeep(self: Arc<Self>) {
        loop {
            let requests = std::mem::take(&mut *self.text_index_upkeep.requests.lock());
            if requests.is_empty() {
                self.text_index_upkeep
                    .running
                    .store(false, Ordering::Release);
                // A request that arrived between the take and the store saw
                // the runner still running and left its work here.
                if self.text_index_upkeep.requests.lock().is_empty()
                    || self.text_index_upkeep.running.swap(true, Ordering::AcqRel)
                {
                    return;
                }
                continue;
            }
            self.text_index_upkeep_pass(requests).await;
        }
    }
    pub(super) async fn text_index_upkeep_pass(self: &Arc<Self>, requests: TextIndexRequests) {
        let settings = self.get_settings().await;
        if !settings.grep_use_index || self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let Some(store) = self.text_index() else {
            return;
        };
        if requests.library {
            // Renditions are copied from the semantic index rather than read
            // again, so a library pass opens it if one exists on disk.
            self.activate_index_from_disk().await;
        }
        // A fresh flag per pass, so a cancel stops this pass and no later one.
        let cancel = Arc::new(AtomicBool::new(false));
        *self.text_index_upkeep.cancel.lock() = Arc::clone(&cancel);
        let semantic = Arc::clone(&*self.index.lock());
        // Half the machine: the reading is background work, and the user may
        // be searching or reading while it runs.
        let threads = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(1);
        let started = std::time::Instant::now();

        let report = if requests.library {
            // A library pass covers whatever the batches named.
            let mut paths = Vec::new();
            let mut unlisted_roots = Vec::new();
            let (roots, _) = library_roots(&settings);
            for configured in configured_library_roots(&settings) {
                let available = std::fs::canonicalize(&configured)
                    .is_ok_and(|root| roots.iter().any(|listed| root.starts_with(listed)));
                if !available {
                    unlisted_roots.push(configured);
                }
            }
            for root in roots {
                match crate::commands::files::list_files_with_ignore(
                    root.clone(),
                    settings.supported_extensions.clone(),
                    settings.max_file_size,
                    settings.respect_gitignore,
                )
                .await
                {
                    Ok(listed) => paths.extend(
                        listed
                            .files
                            .into_iter()
                            .filter(|entry| entry.file_type == FileType::Paged)
                            .map(|entry| entry.path),
                    ),
                    Err(e) => {
                        error!("text index: list {}: {e:#}", root.display());
                        unlisted_roots.push(root);
                    }
                }
            }
            let library = LibraryDocuments {
                paths,
                unlisted_roots,
            };
            tokio::task::spawn_blocking(move || {
                reconcile_library(&store, Some(&semantic), &library, threads, &cancel)
            })
            .await
        } else {
            let extensions = settings.supported_extensions.clone();
            let max_file_size = settings.max_file_size;
            tokio::task::spawn_blocking(move || {
                let mut total = UpkeepReport::default();
                for batch in &requests.batches {
                    let changes = ChangedDocuments::from_batch(batch, &extensions, max_file_size);
                    let report =
                        apply_changes(&store, Some(&semantic), &changes, threads, &cancel)?;
                    total.copied += report.copied;
                    total.extracted += report.extracted;
                    total.failed += report.failed;
                    total.renamed += report.renamed;
                    total.removed += report.removed;
                    total.cancelled |= report.cancelled;
                    if total.cancelled {
                        break;
                    }
                }
                Ok(total)
            })
            .await
        };

        match report {
            Ok(Ok(report)) if report.changed_anything() || report.cancelled => info!(
                "text index: {report:?} in {} ms",
                started.elapsed().as_millis()
            ),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => error!("text index upkeep failed: {e:#}"),
            Err(e) => error!("text index upkeep panicked: {e}"),
        }
    }
    /// Converge `full_text` right after a restored-from-disk index is installed.
    /// Legacy rows that carry chunks but no stored text leave the readers of a
    /// rendition's text (summaries, completion context, superseded areas, the
    /// document text index's copy) with nothing; fill them once, in the
    /// background, off the index lock. Self-limiting: a cheap no-op query once nothing is
    /// stale, so it is safe to run on every load (startup and runtime toggle).
    ///
    /// Safe to run unattended because it runs no inference: the backfill reads
    /// with the native-text registry it builds itself, and there is no
    /// registry to pass it. See `SemanticIndex::backfill_missing_full_text`.
    pub(super) fn spawn_full_text_backfill(&self) {
        let index_arc = Arc::clone(&*self.index.lock());
        tokio::task::spawn_blocking(move || {
            let filled = SemanticIndex::backfill_missing_full_text(&index_arc);
            if filled > 0 {
                info!("full_text backfill: filled {filled} legacy row(s)");
            }
        });
    }
}
