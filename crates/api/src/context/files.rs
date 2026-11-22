//! Library files: listing, opening, renaming, importing, previews, and the
//! directory watcher.

use super::*;

pub(super) fn configured_library_roots(settings: &Settings) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = settings.last_directory.clone() {
        roots.push(root);
    }
    for root in settings
        .favorites
        .iter()
        .chain(settings.recent_dirs.iter())
        .cloned()
    {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

pub(super) fn library_roots(settings: &Settings) -> (Vec<PathBuf>, Vec<String>) {
    let roots = configured_library_roots(settings);
    let mut errors = Vec::new();
    let mut canonical: Vec<PathBuf> = Vec::new();
    for root in roots {
        match std::fs::canonicalize(&root) {
            Ok(root) if root.is_dir() => {
                if !canonical.contains(&root) {
                    canonical.push(root);
                }
            }
            Ok(_) => errors.push(format!(
                "Library path is not a directory: {}",
                root.display()
            )),
            Err(err) => errors.push(format!(
                "Library directory is unavailable: {} ({err})",
                root.display()
            )),
        }
    }
    canonical.sort_by_key(|root| root.components().count());
    let mut covered = Vec::<PathBuf>::new();
    for root in canonical {
        if !covered.iter().any(|parent| root.starts_with(parent)) {
            covered.push(root);
        }
    }
    (covered, errors)
}

impl AppContext {
    /// What one document's reading surface needs to show a match.
    ///
    /// Routed through the context rather than called as a free function so a
    /// PDF preview can carry the areas whose text this workspace's reading
    /// owns. Those live in the index, which is the context's to hand out.
    pub async fn preview(
        &self,
        match_ref: wilkes_core::types::MatchRef,
    ) -> anyhow::Result<wilkes_core::types::PreviewData> {
        let index = self.index.lock().clone();
        crate::commands::preview::preview(match_ref, Some(index)).await
    }
    /// One random-access comic page, without a document conversion.
    pub async fn comic_page(&self, path: std::path::PathBuf, page: u32) -> anyhow::Result<Vec<u8>> {
        crate::commands::preview::comic_page(path, page).await
    }
    /// The installed font that stands in for one a PDF names, by id.
    pub async fn resolve_pdf_font(
        &self,
        request: wilkes_core::pdf_edit::fonts::FontRequest,
    ) -> anyhow::Result<String> {
        crate::commands::pdf_edit::resolve_font(request).await
    }
    pub async fn pdf_font_bytes(&self, id: String) -> anyhow::Result<Vec<u8>> {
        crate::commands::pdf_edit::font_bytes(id).await
    }
    /// Write a PDF with a reader's edits applied to a new file. The source is
    /// never the destination.
    pub async fn save_pdf_edits(
        &self,
        source: std::path::PathBuf,
        destination: std::path::PathBuf,
        export: wilkes_core::pdf_edit::EditExport,
    ) -> anyhow::Result<()> {
        crate::commands::pdf_edit::save_edits(source, destination, export).await
    }
    pub async fn export_pdf_edits(
        &self,
        source: std::path::PathBuf,
        export: wilkes_core::pdf_edit::EditExport,
    ) -> anyhow::Result<Vec<u8>> {
        crate::commands::pdf_edit::export_edits(source, export).await
    }
    /// The pages of a book, as a PDF the reader can draw.
    ///
    /// A separate call from `preview` because these are megabytes: the preview
    /// carries the outline, the links and where to open, and the interface
    /// asks for the pages once it knows it needs them.
    pub async fn surrogate_bytes(&self, path: std::path::PathBuf) -> anyhow::Result<Vec<u8>> {
        crate::commands::preview::surrogate_bytes(path).await
    }
    /// The first page of a file on disk, as a PNG fitted inside `max_edge`.
    ///
    /// What a file list draws, and what [`Self::managed_document_cover`] draws
    /// once it has found the retained snapshot. A list asks for the same
    /// covers every time it is shown, so these are kept.
    pub async fn file_cover(
        &self,
        path: std::path::PathBuf,
        max_edge: u32,
    ) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            (1..=MAX_FIGURE_EDGE).contains(&max_edge),
            "max_edge must be between 1 and {MAX_FIGURE_EDGE}; got {max_edge}"
        );
        crate::commands::preview::file_cover(self.cover_cache_dir.clone(), path, max_edge).await
    }
    pub async fn list_files(
        &self,
        root: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        self.list_files_filtered(root, None, &[], None).await
    }
    pub async fn list_files_filtered(
        &self,
        root: PathBuf,
        collection_id: Option<&str>,
        tag_ids: &[String],
        collection_expression: Option<&str>,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        self.list_files_filtered_with_ignore(
            root,
            collection_id,
            tag_ids,
            collection_expression,
            true,
            None,
        )
        .await
    }
    /// List the searchable files under `root`, enriched with metadata, tags and
    /// collection eligibility.
    ///
    /// `only_path` narrows the enumeration step to a single known file. The
    /// enrichment and filtering below are unchanged by it — they simply operate
    /// on a one-entry listing — so a targeted query pays for one file instead of
    /// walking the whole root to discard all but one result.
    pub(super) async fn list_files_filtered_with_ignore(
        &self,
        root: PathBuf,
        collection_id: Option<&str>,
        tag_ids: &[String],
        collection_expression: Option<&str>,
        respect_gitignore: bool,
        only_path: Option<PathBuf>,
    ) -> anyhow::Result<wilkes_core::types::FileListResponse> {
        let s = self.get_settings().await;
        let mut response = match only_path {
            Some(path) => {
                crate::commands::files::list_single_file(
                    path,
                    s.supported_extensions.clone(),
                    s.max_file_size,
                )
                .await?
            }
            None => {
                crate::commands::files::list_files_with_ignore(
                    root.clone(),
                    s.supported_extensions.clone(),
                    s.max_file_size,
                    respect_gitignore,
                )
                .await?
            }
        };

        // Populate document metadata from the cache and
        // schedule background extraction for anything not yet cached.
        let mut misses: Vec<PathBuf> = Vec::new();
        if let Some(cache) = self.metadata_cache() {
            let primary_source = metadata_source_preference(&s.primary_metadata_source);
            if let Ok(guard) = cache.lock() {
                for entry in response.files.iter_mut() {
                    let Some(identity) = FileIdentity::from_entry(entry) else {
                        continue;
                    };
                    match guard.get_valid_with_primary(
                        &entry.path,
                        identity,
                        primary_source.clone(),
                    ) {
                        Ok(Some(cached)) => {
                            let meta = cached.metadata;
                            let citation_count = meta.citation_count();
                            entry.title = meta.title;
                            entry.author = meta.author;
                            entry.doi = meta.doi;
                            entry.publication_date = meta.created_at;
                            entry.citation_count = citation_count;
                            entry.metadata_conflicts = cached
                                .conflicts
                                .into_iter()
                                .map(|(key, values)| {
                                    (
                                        key,
                                        values
                                            .into_iter()
                                            .map(|value| MetadataConflictValue {
                                                source: value.source,
                                                value: value.value,
                                            })
                                            .collect(),
                                    )
                                })
                                .collect();
                        }
                        Ok(None) => misses.push(entry.path.clone()),
                        Err(e) => error!("metadata cache read {}: {e:#}", entry.path.display()),
                    }
                }
            }

            if !misses.is_empty() {
                self.spawn_metadata_fill(misses, s.clone(), cache);
            }
        }

        let store = self.research_store()?;
        let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
        store.enrich_files(&mut response.files)?;
        if !tag_ids.is_empty() {
            response.files.retain(|entry| {
                tag_ids
                    .iter()
                    .all(|id| entry.tags.iter().any(|tag| tag.id == *id))
            });
            response.omitted.retain(|entry| {
                tag_ids
                    .iter()
                    .all(|id| entry.file.tags.iter().any(|tag| tag.id == *id))
            });
        }
        if let Some(collection_id) = collection_id {
            let eligible = store.eligible_paths(collection_id, &root, &response.files)?;
            response
                .files
                .retain(|entry| eligible.contains(&entry.path));
            response
                .omitted
                .retain(|entry| eligible.contains(&entry.file.path));
        }
        if let Some(expression) = collection_expression.filter(|value| !value.trim().is_empty()) {
            let eligible =
                store.eligible_paths_for_expression(expression, &root, &response.files)?;
            response
                .files
                .retain(|entry| eligible.contains(&entry.path));
            response
                .omitted
                .retain(|entry| eligible.contains(&entry.file.path));
        }

        Ok(response)
    }
    /// Opens a caller-authorized document path. Path confinement belongs to the
    /// transport boundary (for example MCP library roots or server uploads),
    /// because desktop library documents normally live outside `data_dir`.
    pub async fn open_file(&self, path: PathBuf) -> anyhow::Result<PreviewData> {
        let s = self.get_settings().await;
        let index = self.index.lock().clone();
        crate::commands::files::open_file(path, s.supported_extensions, Some(index)).await
    }
    pub async fn rename_file(&self, path: PathBuf, new_name: String) -> anyhow::Result<PathBuf> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let old = path.clone();
        let new = crate::commands::files::rename_file(path, new_name).await?;
        self.rekey_research_path(&old, &new)?;
        self.rekey_index_path(&old, &new)?;
        Ok(new)
    }
    pub fn rekey_research_path(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .rekey_document(old, new)
    }
    /// Rewrites the semantic index keys for a renamed file or directory so its
    /// embeddings are preserved in place. A no-op when no index is loaded.
    pub fn rekey_index_path(&self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let index_arc = self.index.lock().clone();
        let mut guard = index_arc.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(index) = guard.as_mut() {
            index.rename_file(old, new)?;
        }
        Ok(())
    }
    pub async fn import_files_into_current_root(
        &self,
        paths: Vec<PathBuf>,
        root: PathBuf,
        folder: Option<String>,
        mode: crate::commands::files::FileImportMode,
    ) -> anyhow::Result<Vec<PathBuf>> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        let s = self.get_settings().await;
        let Some(current_root) = s.last_directory.clone() else {
            anyhow::bail!("Choose a directory before importing files");
        };
        let root_canonical = std::fs::canonicalize(&root).map_err(|err| {
            anyhow::anyhow!("Root directory not found: {} ({err})", root.display())
        })?;
        let current_root_canonical = std::fs::canonicalize(&current_root).map_err(|err| {
            anyhow::anyhow!(
                "Current root directory not found: {} ({err})",
                current_root.display()
            )
        })?;
        if root_canonical != current_root_canonical {
            anyhow::bail!(
                "Import target must be the current root: {}",
                current_root.display()
            );
        }

        let imported = crate::commands::files::import_files_into_root(
            paths,
            root,
            folder,
            s.supported_extensions,
            mode,
        )
        .await?;
        self.emit_file_list_changed(&root_canonical);
        Ok(imported)
    }
    pub(super) fn stop_directory_watcher(&self) {
        if let Some(mut w) = self.directory_watcher.lock().take() {
            w.stop();
        }
    }
    /// Whether this context belongs to a workspace an application owns rather
    /// than a person (Underdog's semantic corpus is the only one today).
    ///
    /// Such a workspace has exactly one writer of its semantic index: the
    /// managed import API, which publishes rows carrying the document identity
    /// its consumer's stable chunk refs are derived from. The interactive
    /// indexing machinery — the directory watcher and the background reindex it
    /// feeds — must never run there. Its root is `managed_sources`, which every
    /// import writes into, so it would otherwise re-index each imported
    /// document through the identity-less path and strip the very refs the
    /// import just handed out.
    pub(super) fn is_application_managed(&self) -> bool {
        if self.workspace_path == self.settings_path {
            return false;
        }
        crate::workspace::read_manifest(&self.workspace_path)
            .map(|manifest| manifest.is_application_managed())
            .unwrap_or(false)
    }
    /// Whether the user may only read this workspace. The same condition as
    /// [`Self::is_application_managed`], named for the caller that asks it:
    /// a workspace whose sole writer is an application's import API is
    /// read-only to everybody else.
    pub fn is_read_only(&self) -> bool {
        self.is_application_managed()
    }
    /// The one gate every user-initiated write to this workspace's documents
    /// or index passes through.
    ///
    /// An application-managed corpus is now listed and searchable like any
    /// other workspace, so a person can activate it and reach every read path
    /// the UI offers. What used to stop them — the workspace being absent from
    /// the listing and refused by `switch` — protected the corpus only by
    /// making it unreachable, which cost the reads as well as the writes. This
    /// is the protection stated where it belongs: on the write.
    ///
    /// Deliberately not applied to [`Self::import_managed_document`] and the
    /// rest of the managed corpus API. That caller *is* the workspace's owner;
    /// it is every other caller that must be turned away.
    pub fn ensure_writable(&self) -> Result<(), ConsumerError> {
        if self.is_application_managed() {
            return Err(ConsumerError::new(
                ConsumerErrorCode::ManagedWorkspaceProtected,
                "this workspace is owned by another application and can only be read",
            ));
        }
        Ok(())
    }
    pub(super) fn start_directory_watcher(self: &Arc<Self>, root: PathBuf) {
        self.stop_directory_watcher();
        if self.is_application_managed() {
            info!(
                "not watching {}: an application-managed corpus is written only by its import API",
                root.display()
            );
            return;
        }
        if !root.exists() || !root.is_dir() {
            error!("directory watcher root is invalid: {}", root.display());
            return;
        }

        let ctx = Arc::clone(self);
        let runtime = tokio::runtime::Handle::current();
        match DirectoryWatcher::start(root.clone(), move |batch| {
            ctx.emit_file_list_changed(&batch.root);
            let ctx = Arc::clone(&ctx);
            runtime.spawn(async move {
                Arc::clone(&ctx)
                    .process_directory_change_for_semantic(batch.clone())
                    .await;
                // After the semantic index has taken the batch, so a document
                // it just indexed again is copied rather than read twice.
                ctx.request_text_index_batch(batch);
            });
        }) {
            Ok(watcher) => *self.directory_watcher.lock() = Some(watcher),
            Err(err) => error!("directory watcher start failed: {err:#}"),
        }
    }
    /// A root that joined the library has documents the text index has never
    /// read, and one that left has rows nothing will search.
    pub(super) fn on_library_roots_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if configured_library_roots(before) != configured_library_roots(after) {
            self.request_text_index_reconcile();
        }
    }
    pub(super) fn on_directory_setting_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if before.last_directory == after.last_directory {
            return;
        }
        match after.last_directory.clone() {
            Some(root) => self.start_directory_watcher(root),
            None => self.stop_directory_watcher(),
        }
    }
    pub(super) fn emit_file_list_changed(&self, root: &Path) {
        self.events.emit(
            "file-list-changed",
            serde_json::json!({ "root": root.display().to_string() }),
        );
    }
    pub(super) async fn process_directory_change_for_semantic(
        self: Arc<Self>,
        batch: DirectoryChangeBatch,
    ) {
        let settings = self.get_settings().await;
        if !settings.search_prefer_semantic {
            return;
        }

        let Some(embedder) = self.embedder.lock().clone() else {
            return;
        };
        let index_arc = Arc::clone(&*self.index.lock());
        let has_index = index_arc
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        if !has_index {
            return;
        }
        // Invalidate before the updater can mutate rows. A concurrently
        // finishing tree build will observe the revision change and discard
        // its stale result instead of installing it.
        self.invalidate_topic_tree_cache();
        if let Ok(mut guard) = index_arc.lock() {
            if let Some(idx) = guard.as_mut() {
                if let Err(e) = idx.activate_root(&batch.root) {
                    error!(
                        "activate semantic update root {}: {e:#}",
                        batch.root.display()
                    );
                    return;
                }
            }
        }

        let registry = wilkes_core::extract::production_registry();
        let registry = Arc::new(registry);
        let cache = self.metadata_cache();
        let config = Self::restore_state_indexing_config(&settings);
        let ev1 = Arc::clone(&self.events);
        let ev2 = Arc::clone(&self.events);
        let ev3 = Arc::clone(&self.events);

        let result = tokio::task::spawn_blocking(move || {
            process_directory_change(
                batch,
                &index_arc,
                &cache,
                &registry,
                &embedder,
                &config,
                &move || ev1.emit("manager-event", serde_json::json!("Reindexing")),
                &move |done, total| {
                    emit_reindex_progress(ev3.as_ref(), ReindexProgress { done, total })
                },
                &move || ev2.emit("manager-event", serde_json::json!("ReindexingDone")),
            );
        })
        .await;

        if let Err(err) = result {
            error!("process_directory_change_for_semantic task panicked: {err}");
        }
    }
}
