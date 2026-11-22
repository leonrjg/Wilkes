//! Document metadata: the cache, background fills, Zotero and DOI lookups.

use super::*;

pub(super) fn metadata_source_preference(source: &MetadataSourcePreference) -> MetadataSource {
    MetadataSource::from(source)
}

impl AppContext {
    /// Lazily open (once) the shared document-metadata cache. Returns `None` if
    /// it cannot be opened; callers then simply skip metadata population.
    pub(super) fn metadata_cache(&self) -> Option<Arc<Mutex<MetadataCache>>> {
        let mut guard = self.metadata_cache.lock();
        if let Some(cache) = guard.as_ref() {
            return Some(Arc::clone(cache));
        }
        match MetadataCache::open(&self.data_dir) {
            Ok(cache) => {
                let arc = Arc::new(Mutex::new(cache));
                *guard = Some(Arc::clone(&arc));
                Some(arc)
            }
            Err(e) => {
                error!("metadata cache open failed: {e:#}");
                None
            }
        }
    }
    /// Extract document metadata for the given paths off the request path,
    /// upsert it into the cache, and emit `file-metadata-updated` so the UI can
    /// fill in and re-sort. A file whose content matches a stale cache row
    /// (a rename) is re-keyed instead of re-extracted.
    pub(super) fn spawn_metadata_fill(
        &self,
        paths: Vec<PathBuf>,
        settings: Settings,
        cache: Arc<Mutex<MetadataCache>>,
    ) {
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            let primary_source = metadata_source_preference(&settings.primary_metadata_source);
            // Pass 1: file-based extraction (blocking). Emits immediately so the
            // list gets publication dates fast, mirroring the on-open viewer's
            // fast first paint before the Zotero upgrade.
            let exts = settings.supported_extensions.clone();
            let cache1 = Arc::clone(&cache);
            let pass1_primary_source = primary_source.clone();
            let pass1 = tokio::task::spawn_blocking(move || {
                let primary_source = pass1_primary_source;
                let registry = crate::commands::metadata::build_registry(exts);
                let mut eligible: Vec<(PathBuf, FileIdentity, DocumentMetadata)> = Vec::new();
                let mut updates: Vec<serde_json::Value> = Vec::new();
                for path in paths {
                    let Some(identity) = FileIdentity::for_path(&path) else {
                        continue;
                    };
                    match extract_or_rekey(&cache1, &registry, &path, identity) {
                        FillOutcome::Extracted(metadata) => {
                            updates.push(metadata_update_json_from_cache(
                                &cache1,
                                &path,
                                identity,
                                &primary_source,
                                &metadata,
                            ));
                            eligible.push((path, identity, metadata));
                        }
                        // A rename hit already carries composed metadata (whose
                        // Zotero source, if any, was preserved by re-keying), so
                        // it is not eligible for a fresh Zotero resolve.
                        FillOutcome::Renamed(metadata) => {
                            updates.push(metadata_update_json_from_cache(
                                &cache1,
                                &path,
                                identity,
                                &primary_source,
                                &metadata,
                            ));
                        }
                    }
                }
                (eligible, updates)
            })
            .await;

            let (eligible, updates) = match pass1 {
                Ok(v) => v,
                Err(e) => {
                    error!("metadata fill task failed: {e}");
                    return;
                }
            };
            if !updates.is_empty() {
                events.emit("file-metadata-updated", serde_json::json!(updates));
            }

            if eligible.is_empty() {
                return;
            }

            let mut composed = eligible;

            // Pass 2: authoritative Zotero override (async). Fetch the attachment
            // list once for the whole batch, then resolve each file locally.
            if settings.integrations.zotero.enabled {
                let client = ZoteroClient::from_settings(&settings.integrations.zotero);
                let attachments = match client.attachment_items().await {
                    Ok(a) => Some(a),
                    Err(e) => {
                        info!("metadata fill: zotero attachment fetch failed: {e:#}");
                        None
                    }
                };
                if let Some(attachments) = attachments {
                    let mut z_updates: Vec<serde_json::Value> = Vec::new();
                    for (path, identity, metadata) in composed.iter_mut() {
                        if let Some(z) =
                            zotero_override_for(&client, path, metadata, &attachments).await
                        {
                            if let Ok(guard) = cache.lock() {
                                if let Err(e) =
                                    guard.upsert(path, *identity, &z, MetadataSource::Zotero)
                                {
                                    error!(
                                        "metadata cache zotero upsert {}: {e:#}",
                                        path.display()
                                    );
                                }
                            }
                            *metadata = z;
                            z_updates.push(metadata_update_json_from_cache(
                                &cache,
                                path,
                                *identity,
                                &primary_source,
                                metadata,
                            ));
                        }
                    }
                    if !z_updates.is_empty() {
                        events.emit("file-metadata-updated", serde_json::json!(z_updates));
                    }
                }
            }

            // No mirror: passes 3 and 4 ask only providers that can look a DOI
            // up, and a catalogue answered from the mirror is not one.
            let registry = IntegrationRegistry::from_settings(&settings.integrations, None);

            // Pass 3: a DOI by title. A document that names no DOI, neither in
            // its file nor in Zotero, is searched for by its title, in turn, at
            // each provider pass 4 will ask about it, and the first that names
            // exactly one work under that title gives the DOI. Nothing is
            // written here: the DOI is kept only once pass 4's lookup of it
            // answers, under the provider that answered.
            let doi_finders: Vec<_> = registry
                .literature_sources()
                .filter(|entry| entry.enabled && entry.source().capabilities().lookup)
                .collect();
            for (path, _, metadata) in composed.iter_mut() {
                if metadata.doi.is_some() {
                    continue;
                }
                let Some(title) = metadata.title.clone() else {
                    continue;
                };
                for entry in &doi_finders {
                    if let Some(doi) = doi_by_title(
                        entry.source().as_ref(),
                        path,
                        &title,
                        metadata.author.as_deref(),
                    )
                    .await
                    {
                        metadata.doi = Some(doi);
                        break;
                    }
                }
            }

            // Pass 4: provider enrichment. Every enabled provider that can look
            // a work up by DOI is asked about each file that has one, and what
            // it answers is written under that provider's id, keyed by the
            // composed DOI. Which providers exist, and what each can do, is
            // the registry's answer; nothing here names one.
            for entry in registry.literature_sources().filter(|entry| entry.enabled) {
                let source = Arc::clone(entry.source());
                let capabilities = source.capabilities();
                if !capabilities.lookup && !capabilities.references {
                    continue;
                }
                let provider = entry.id().to_string();
                let mut updates: Vec<serde_json::Value> = Vec::new();
                for (path, identity, metadata) in composed.iter_mut() {
                    let mut enriched = false;
                    if capabilities.lookup {
                        if let Some(work) = enrichment_for(source.as_ref(), metadata).await {
                            metadata.providers.insert(provider.clone(), work);
                            if let Ok(guard) = cache.lock() {
                                if let Err(e) = guard.upsert(
                                    path,
                                    *identity,
                                    metadata,
                                    MetadataSource::provider(&provider),
                                ) {
                                    error!(
                                        "metadata cache {provider} upsert {}: {e:#}",
                                        path.display()
                                    );
                                }
                            }
                            enriched = true;
                        }
                    }
                    // Populate the citation graph from the same pass. Edges are
                    // keyed by DOI, so this is independent of whether the
                    // referenced papers are in the library. Asked only of a
                    // work the provider knows, when it can say so.
                    if capabilities.references && (enriched || !capabilities.lookup) {
                        if let Some(doi) = metadata.doi.clone() {
                            match source.references(&doi).await {
                                Ok(targets) => {
                                    if let Ok(guard) = cache.lock() {
                                        if let Err(e) =
                                            guard.replace_citations(&doi, &provider, &targets)
                                        {
                                            error!(
                                                "metadata cache citations {}: {e:#}",
                                                path.display()
                                            );
                                        }
                                    }
                                    enriched = true;
                                }
                                Err(e) => info!("{provider} references {doi}: {e:#}"),
                            }
                        }
                    }
                    if enriched {
                        updates.push(metadata_update_json_from_cache(
                            &cache,
                            path,
                            *identity,
                            &primary_source,
                            metadata,
                        ));
                    }
                }
                if !updates.is_empty() {
                    events.emit("file-metadata-updated", serde_json::json!(updates));
                }
            }
        });
    }
    pub async fn get_file_metadata(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        let s = self.get_settings().await;
        crate::commands::metadata::get_file_metadata(path, s.supported_extensions).await
    }
    /// Richest available metadata for one document, in one authoritative order.
    /// Cache-first: when the file is cached and unchanged, return the composed
    /// record (title / author / DOI / publication date plus any Semantic
    /// Scholar or OpenAlex enrichment) exactly as the file list resolves it. On
    /// a cache miss fall back to [`Self::resolve_file_metadata`] (extraction +
    /// Zotero override), which yields the bibliographic basics for a file the
    /// cache has not processed yet. No third composition is introduced — this
    /// only chooses between the two the app already owns.
    pub async fn document_metadata_full(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        if let (Some(cache), Some(identity)) =
            (self.metadata_cache(), FileIdentity::for_path(&path))
        {
            let primary =
                metadata_source_preference(&self.get_settings().await.primary_metadata_source);
            let cached = cache
                .lock()
                .map_err(|_| anyhow::anyhow!("metadata cache lock poisoned"))?
                .get_valid_with_primary(&path, identity, primary)?;
            if let Some(cached) = cached {
                return Ok(cached.metadata);
            }
        }
        self.resolve_file_metadata(path).await
    }
    pub async fn zotero_status(&self) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_status(s).await
    }
    /// Authoritative document metadata: file-based extraction overridden by the
    /// Zotero library record when the file resolves to an item. This is the
    /// single owner of that composition — both the on-open viewer and the
    /// background tabulation resolve through it (the fill batches the Zotero
    /// attachment fetch, but composes identically).
    pub async fn resolve_file_metadata(&self, path: PathBuf) -> anyhow::Result<DocumentMetadata> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::resolve_file_metadata(s, path).await
    }
    /// Re-derive metadata for every cached file in place. Backs the manual
    /// "refresh metadata" action.
    ///
    /// Refresh never clears: clearing then repopulating loses any field the
    /// re-derivation fails to reproduce — a Zotero item that has since been
    /// removed from the library, or any field at all while Zotero is
    /// unreachable. Instead we re-run the same extraction + Zotero-override fill
    /// used on listing over the known files, writing through the merging
    /// [`MetadataCache::upsert`], which only overwrites a field when the new
    /// derivation actually produced a value and never lets file extraction
    /// clobber authoritative Zotero fields.
    ///
    /// Callers are responsible for confining explicit paths to the document
    /// roots they expose. The server does this at its HTTP boundary; desktop
    /// libraries normally live outside Wilkes' private application data
    /// directory.
    pub async fn refresh_file_metadata(&self, path: Option<PathBuf>) -> anyhow::Result<()> {
        let s = self.get_settings().await;
        let Some(cache) = self.metadata_cache() else {
            return Ok(());
        };
        let paths = if let Some(path) = path {
            vec![path]
        } else {
            match cache.lock() {
                Ok(guard) => guard.all_paths()?,
                Err(_) => return Ok(()),
            }
        };
        if !paths.is_empty() {
            self.spawn_metadata_fill(paths, s, cache);
        }
        Ok(())
    }
    pub async fn zotero_add_item(
        &self,
        path: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::AddOutcome> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_add_item(s, path).await
    }
    pub async fn zotero_generate_citation(
        &self,
        path: PathBuf,
    ) -> anyhow::Result<wilkes_core::types::CitationResult> {
        let s = self.get_settings().await;
        crate::commands::integrations::zotero::zotero_generate_citation(s, path).await
    }

    // ── Internal helpers ──────────────────────────────────────────────────────
    pub(super) fn spawn_provider_backfill(&self, settings: Settings) {
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let paths = match cache.lock() {
            Ok(guard) => guard.all_paths().unwrap_or_default(),
            Err(_) => return,
        };
        if !paths.is_empty() {
            self.spawn_metadata_fill(paths, settings, cache);
        }
    }
    /// Re-resolve every file-sourced cache row against Zotero and upgrade the
    /// matches to authoritative Zotero rows. Runs after Zotero becomes usable so
    /// already-tabulated files gain library data without re-extraction.
    pub(super) fn spawn_zotero_backfill(&self, settings: Settings) {
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let events = Arc::clone(&self.events);
        tokio::spawn(async move {
            let rows = match cache.lock() {
                Ok(guard) => guard
                    .list_by_source(MetadataSource::File)
                    .unwrap_or_default(),
                Err(_) => return,
            };
            if rows.is_empty() {
                return;
            }
            let client = ZoteroClient::from_settings(&settings.integrations.zotero);
            let attachments = match client.attachment_items().await {
                Ok(a) => a,
                Err(e) => {
                    info!("zotero backfill: attachment fetch failed: {e:#}");
                    return;
                }
            };
            let mut updates: Vec<serde_json::Value> = Vec::new();
            let primary_source = metadata_source_preference(&settings.primary_metadata_source);
            for row in rows {
                if let Some(z) =
                    zotero_override_for(&client, &row.path, &row.metadata, &attachments).await
                {
                    if let Ok(guard) = cache.lock() {
                        if let Err(e) =
                            guard.upsert(&row.path, row.identity, &z, MetadataSource::Zotero)
                        {
                            error!("zotero backfill upsert {}: {e:#}", row.path.display());
                        }
                    }
                    updates.push(metadata_update_json_from_cache(
                        &cache,
                        &row.path,
                        row.identity,
                        &primary_source,
                        &z,
                    ));
                }
            }
            if !updates.is_empty() {
                events.emit("file-metadata-updated", serde_json::json!(updates));
            }
        });
    }

    // ── Generation ────────────────────────────────────────────────────────────
}

/// Outcome of filling one file's metadata: either freshly extracted (and thus
/// eligible for a Zotero upgrade), or recovered by re-keying a renamed file's
/// existing row (already composed, so not re-resolved).
pub(super) enum FillOutcome {
    Extracted(DocumentMetadata),
    Renamed(DocumentMetadata),
}

/// Whether any enabled provider can be asked for a work's references.
pub(super) fn references_provider_enabled(settings: &Settings) -> bool {
    IntegrationRegistry::from_settings(&settings.integrations, None)
        .literature_sources()
        .any(|entry| entry.enabled && entry.source().capabilities().references)
}

/// JSON payload entry for the `file-metadata-updated` event.
pub(super) fn metadata_update_json(
    path: &Path,
    metadata: &DocumentMetadata,
    metadata_conflicts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "path": path.to_string_lossy(),
        "title": metadata.title,
        "author": metadata.author,
        "doi": metadata.doi,
        "publication_date": metadata.created_at,
        "citation_count": metadata.citation_count(),
        "metadata_conflicts": metadata_conflicts,
    })
}

pub(super) fn metadata_update_json_from_cache(
    cache: &Arc<Mutex<MetadataCache>>,
    path: &Path,
    identity: FileIdentity,
    primary_source: &MetadataSource,
    fallback: &DocumentMetadata,
) -> serde_json::Value {
    match cache
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .get_valid_with_primary(path, identity, primary_source.clone())
                .ok()
        })
        .flatten()
    {
        Some(cached) => metadata_update_json(
            path,
            &cached.metadata,
            serde_json::to_value(cached.conflicts).unwrap_or_else(|_| serde_json::json!({})),
        ),
        None => metadata_update_json(path, fallback, serde_json::json!({})),
    }
}

/// Fill one file's file-based metadata. Prefers a cheap re-key when the same
/// content already exists in the cache under a stale (now-missing) path — a
/// rename. Otherwise extract fresh and upsert as a `File`-sourced row.
pub(super) fn extract_or_rekey(
    cache: &Arc<Mutex<MetadataCache>>,
    registry: &wilkes_core::metadata::MetadataExtractorRegistry,
    path: &Path,
    identity: FileIdentity,
) -> FillOutcome {
    if let Ok(guard) = cache.lock() {
        if let Ok(Some(old_path)) = guard.find_rename_source(path, identity) {
            if let Err(e) = guard.rename(&old_path, path) {
                error!(
                    "metadata cache rename {} -> {}: {e:#}",
                    old_path.display(),
                    path.display()
                );
            } else if let Ok(Some(meta)) = guard.get_valid(path, identity) {
                return FillOutcome::Renamed(meta);
            }
        }
    }

    let metadata = registry.extract_for(path, None).unwrap_or_default();
    if let Ok(guard) = cache.lock() {
        if let Err(e) = guard.upsert(path, identity, &metadata, MetadataSource::File) {
            error!("metadata cache upsert {}: {e:#}", path.display());
        }
    }
    FillOutcome::Extracted(metadata)
}

/// Best-effort Zotero override for one file against a pre-fetched attachment
/// list. Returns `None` when the file does not resolve or the lookup errors.
pub(super) async fn zotero_override_for(
    client: &ZoteroClient,
    path: &Path,
    file_based: &DocumentMetadata,
    attachments: &[ZoteroItem],
) -> Option<DocumentMetadata> {
    match crate::commands::integrations::zotero::resolve_override(
        client,
        path,
        file_based,
        attachments,
    )
    .await
    {
        Ok(opt) => opt,
        Err(e) => {
            info!("zotero override {}: {e:#}", path.display());
            None
        }
    }
}

/// One provider's record of the work this document's DOI names, when the
/// document has one and the provider knows it. A provider that does not is
/// not a failure: most files are not indexed by most providers.
pub(super) async fn enrichment_for(
    source: &dyn LiteratureSource,
    metadata: &DocumentMetadata,
) -> Option<wilkes_core::types::ProviderWork> {
    let doi = metadata.doi.as_deref()?;
    match source.lookup(doi).await {
        Ok(work) => Some(work),
        Err(e) => {
            info!("{} lookup {doi}: {e:#}", source.id());
            None
        }
    }
}

/// How many results a title search reads: enough that a second work under the
/// same title shows up and makes the match ambiguous rather than wrong.
pub(super) const TITLE_SEARCH_LIMIT: usize = 10;

/// The DOI one provider's search names for a document's title, when exactly
/// one work it returns bears that title: see
/// [`wilkes_core::metadata::title::match_title`]. A provider that does not
/// know the work is not a failure.
pub(super) async fn doi_by_title(
    source: &dyn LiteratureSource,
    path: &Path,
    title: &str,
    author: Option<&str>,
) -> Option<String> {
    use wilkes_core::metadata::title::{match_title, TitleMatch};

    let results = match source.search(title, TITLE_SEARCH_LIMIT, 1).await {
        Ok(results) => results,
        Err(e) => {
            info!("{} title search for {}: {e:#}", source.id(), path.display());
            return None;
        }
    };
    match match_title(title, author, &results) {
        TitleMatch::Found(doi) => {
            info!(
                "{} names {doi} for {} by its title {title:?}",
                source.id(),
                path.display()
            );
            Some(doi)
        }
        TitleMatch::Ambiguous(dois) => {
            info!(
                "{} holds several works titled {title:?}, so {} is given none of {dois:?}",
                source.id(),
                path.display()
            );
            None
        }
        TitleMatch::NotFound => None,
    }
}
