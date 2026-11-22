//! Citation links, reading order and the citation graph.

use super::*;

/// The document text index's single writer: queued requests, whether the
/// runner is running, and the cancel flag of the pass in progress.
/// One library document as reading order takes it.
pub(super) struct ReadingOrderSource {
    /// The library root it was listed under: its folders are named from here.
    pub(super) library_root: PathBuf,
    pub(super) entry: FileEntry,
}

impl ReadingOrderSource {
    pub(super) fn labels(&self) -> wilkes_core::reading_order::Labels {
        let path = &self.entry.path;
        let folders = path
            .parent()
            .and_then(|parent| parent.strip_prefix(&self.library_root).ok())
            .map(|below| {
                below
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        wilkes_core::reading_order::Labels {
            title: self.entry.title.clone(),
            file_stem: path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            folders,
        }
    }
}

/// What reading order keeps between requests.
#[derive(Default)]
pub(super) struct ReadingOrderState {
    /// Counts requests. A request that finds a later one has started stops
    /// reading: only the latest answer is wanted.
    pub(super) generation: AtomicU64,
    pub(super) cache: PLMutex<ReadingOrderCache>,
}

/// Each document's text as read against one lexicon. Reading is the costly
/// part of an order (every document of the library, once), and it depends
/// only on the document and on the phrases looked for.
#[derive(Default)]
pub(super) struct ReadingOrderCache {
    /// Which lexicon the readings were taken against.
    pub(super) lexicon: u64,
    /// By path, with the size and modification time the file had when read.
    pub(super) readings: HashMap<PathBuf, ((u64, i64), Arc<wilkes_core::reading_order::Reading>)>,
}

impl AppContext {
    /// Citation neighbours of a document that are present in the library,
    /// resolved by DOI: the documents it references and the documents that
    /// cite it. Unlike `related_documents` this needs no semantic index — it
    /// reads the DOI-keyed citation graph from the metadata cache. Returns
    /// empty when the anchor has no known DOI or no cached edges.
    pub async fn citation_links(
        self: Arc<Self>,
        query: CitationLinksQuery,
    ) -> Result<CitationLinks, String> {
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Citation links root")?;
        let (source_path, _) = Self::canonicalize_supported_file(
            &root,
            &query.path,
            &settings.supported_extensions,
            "Citation links",
        )?;
        Self::ensure_path_in_library(&source_path, &library_roots, "Citation links file")?;

        let cache = self
            .metadata_cache()
            .ok_or_else(|| "Metadata cache is unavailable".to_string())?;
        let (paths, all_references) = {
            let guard = cache
                .lock()
                .map_err(|_| "Metadata cache lock failed".to_string())?;
            let Some(doi) = guard
                .doi_for_path(&source_path)
                .map_err(|e| e.to_string())?
            else {
                return Ok(CitationLinks::default());
            };
            (
                guard.citation_links(&doi).map_err(|e| e.to_string())?,
                guard.citation_references(&doi).map_err(|e| e.to_string())?,
            )
        };
        if paths.references.is_empty() && paths.cited_by.is_empty() && all_references.is_empty() {
            return Ok(CitationLinks::default());
        }

        // Resolve edge paths to the same metadata-enriched record shape as every
        // other document list. Citation edges span the whole library, so list
        // across all roots rather than a single scope.
        let mut entries = std::collections::HashMap::new();
        for listing_root in library_roots {
            let listed = self
                .list_files(listing_root)
                .await
                .map_err(|e| e.to_string())?;
            entries.extend(
                listed
                    .files
                    .into_iter()
                    .map(|entry| (entry.path.clone(), entry)),
            );
        }
        let resolve = |paths: Vec<PathBuf>| -> Vec<FileEntry> {
            paths
                .into_iter()
                .filter(|path| path != &source_path)
                .filter_map(|path| entries.get(&path).cloned())
                .collect()
        };
        Ok(CitationLinks {
            references: resolve(paths.references),
            cited_by: resolve(paths.cited_by),
            all_references,
        })
    }
    /// The documents of one library folder in the order they build on one
    /// another, read off their text ([`wilkes_core::reading_order`]). Runs no
    /// model and needs no semantic index.
    ///
    /// Every document of the library is read, not only the folder's: which
    /// single words are ordinary vocabulary is a fact about the library. A
    /// document's reading is kept, so the next order reads only what changed.
    /// Paged documents are read from the document text index, which fills in
    /// the background: one it has not reached yet is reported as having no
    /// text rather than being extracted here.
    pub async fn reading_order(
        self: Arc<Self>,
        query: ReadingOrderQuery,
    ) -> Result<ReadingOrder, String> {
        use wilkes_core::reading_order as reading;

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = std::fs::canonicalize(&query.root).map_err(|err| {
            format!(
                "Folder does not exist or cannot be accessed: {} ({err})",
                query.root.display()
            )
        })?;
        Self::ensure_path_in_library(&root, &library_roots, "Folder to order")?;
        let theme = match query
            .theme
            .as_deref()
            .map(str::trim)
            .filter(|theme| !theme.is_empty())
        {
            Some(theme) => Some(reading::theme_key(theme).ok_or_else(|| {
                format!(
                    "A theme is one to {} words: {theme}",
                    reading::MAX_SUBJECT_WORDS
                )
            })?),
            None => None,
        };

        let mut documents: Vec<ReadingOrderSource> = Vec::new();
        let mut seen = HashSet::new();
        for library_root in &library_roots {
            let listed = self
                .list_files(library_root.clone())
                .await
                .map_err(|e| e.to_string())?;
            for entry in listed.files {
                if seen.insert(entry.path.clone()) {
                    documents.push(ReadingOrderSource {
                        library_root: library_root.clone(),
                        entry,
                    });
                }
            }
        }
        documents.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));

        let metadata = self
            .metadata_cache()
            .ok_or_else(|| "Metadata cache is unavailable".to_string())?;
        let texts = self
            .text_index()
            .ok_or_else(|| "The document text index is unavailable".to_string())?;
        let generation = self.reading_order.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let ctx = Arc::clone(&self);
        let asked = query.root;
        tokio::task::spawn_blocking(move || {
            ctx.reading_order_blocking(asked, root, theme, documents, &metadata, &texts, generation)
        })
        .await
        .map_err(|e| format!("Reading order task panicked: {e}"))?
    }
    /// `asked` is the folder as the caller named it and `root` its canonical
    /// path. Documents are answered under `asked`, the way a listing of that
    /// folder names them, so the two can be joined by path.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reading_order_blocking(
        &self,
        asked: PathBuf,
        root: PathBuf,
        theme: Option<String>,
        documents: Vec<ReadingOrderSource>,
        metadata: &Mutex<MetadataCache>,
        texts: &DocumentTextIndex,
        generation: u64,
    ) -> Result<ReadingOrder, String> {
        use std::hash::{Hash, Hasher};
        use wilkes_core::reading_order as reading;

        let described: Vec<reading::Described> = documents
            .iter()
            .map(|document| reading::Described::from_labels(&document.labels()))
            .collect();
        let mut candidates: Vec<String> = described
            .iter()
            .flat_map(reading::Described::candidates)
            .collect();
        candidates.sort();
        candidates.dedup();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        candidates.hash(&mut hasher);
        let lexicon_id = hasher.finish();
        let lexicon = reading::Lexicon::new(candidates);
        let theme_lexicon = theme
            .as_ref()
            .map(|key| reading::Lexicon::new([key.clone()]));

        let known = {
            let mut cache = self.reading_order.cache.lock();
            if cache.lexicon != lexicon_id {
                cache.lexicon = lexicon_id;
                cache.readings.clear();
            }
            cache.readings.clone()
        };

        let in_scope: Vec<bool> = documents
            .iter()
            .map(|document| document.entry.path.starts_with(&root))
            .collect();
        let mut readings: Vec<Option<Arc<reading::Reading>>> = Vec::with_capacity(documents.len());
        let mut theme_shares: Vec<Option<f32>> = vec![None; documents.len()];
        let mut fresh = Vec::new();
        for (index, document) in documents.iter().enumerate() {
            if self.reading_order.generation.load(Ordering::Acquire) != generation {
                return Err("A newer reading order was asked for".to_string());
            }
            let entry = &document.entry;
            let stamp = entry
                .modified_at_ms
                .map(|modified| (entry.size_bytes, modified));
            let kept = stamp.and_then(|stamp| {
                known
                    .get(&entry.path)
                    .filter(|(read_at, _)| *read_at == stamp)
                    .map(|(_, reading)| Arc::clone(reading))
            });
            let themed = theme_lexicon.as_ref().filter(|_| in_scope[index]);
            if kept.is_some() && themed.is_none() {
                readings.push(kept);
                continue;
            }
            let text = match entry.file_type {
                FileType::Paged => texts.text_for_path(&entry.path).map_err(|e| {
                    format!(
                        "Stored text of {} could not be read: {e:#}",
                        entry.path.display()
                    )
                })?,
                FileType::PlainText => match std::fs::read(&entry.path) {
                    Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                    Err(e) => {
                        tracing::warn!(
                            "reading order: {} could not be read: {e}",
                            entry.path.display()
                        );
                        None
                    }
                },
            };
            let Some(text) = text else {
                readings.push(kept);
                continue;
            };
            if let Some(theme_lexicon) = themed {
                let read = reading::read_text(&text, theme_lexicon);
                theme_shares[index] = theme.as_deref().map(|key| read.share(key));
            }
            let read = match kept {
                Some(read) => read,
                None => {
                    let read = Arc::new(reading::read_text(&text, &lexicon));
                    if let Some(stamp) = stamp {
                        fresh.push((entry.path.clone(), (stamp, Arc::clone(&read))));
                    }
                    read
                }
            };
            readings.push(Some(read));
        }
        {
            let mut cache = self.reading_order.cache.lock();
            if cache.lexicon == lexicon_id {
                cache.readings.extend(fresh);
            }
        }

        // Citations between the folder's documents, where a provider has
        // answered for the citing one.
        let mut cites = Vec::new();
        {
            let guard = metadata
                .lock()
                .map_err(|_| "Metadata cache lock failed".to_string())?;
            let mut index_by_doi: HashMap<String, usize> = HashMap::new();
            for (index, document) in documents.iter().enumerate() {
                if !in_scope[index] {
                    continue;
                }
                if let Some(doi) = document.entry.doi.as_deref().and_then(normalize_doi) {
                    index_by_doi.entry(doi).or_insert(index);
                }
            }
            let dois: Vec<String> = index_by_doi.keys().cloned().collect();
            let fetched = guard.citations_fetched(&dois).map_err(|e| e.to_string())?;
            for doi in &dois {
                if !fetched.contains(doi) {
                    continue;
                }
                for reference in guard.citation_references(doi).map_err(|e| e.to_string())? {
                    if let Some(&cited) = index_by_doi.get(&reference.doi) {
                        cites.push((index_by_doi[doi], cited));
                    }
                }
            }
        }

        let names: Vec<String> = documents
            .iter()
            .map(|document| {
                document
                    .entry
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        let entries: Vec<reading::Entry<'_>> = (0..documents.len())
            .map(|index| reading::Entry {
                described: &described[index],
                reading: readings[index].as_deref(),
                in_scope: in_scope[index],
                publication: documents[index].entry.publication_date.as_deref(),
                name: &names[index],
            })
            .collect();
        let outcome = reading::order(&entries, &cites);

        let path_of = |index: usize| {
            let path = &documents[index].entry.path;
            match path.strip_prefix(&root) {
                Ok(below) => asked.join(below),
                Err(_) => path.clone(),
            }
        };
        let ordered = outcome
            .sequence
            .iter()
            .map(|&index| {
                let placed = &outcome.documents[index];
                ReadingOrderDocument {
                    path: path_of(index),
                    placement: match placed.placement {
                        reading::Placement::Foundation => ReadingPlacement::Foundation,
                        reading::Placement::Bridge => ReadingPlacement::Bridge,
                        reading::Placement::Specialised => ReadingPlacement::Specialised,
                        reading::Placement::Standalone => ReadingPlacement::Standalone,
                        reading::Placement::NoSubject => ReadingPlacement::NoSubject,
                        reading::Placement::NoText => ReadingPlacement::NoText,
                    },
                    subjects: placed
                        .subjects
                        .iter()
                        .map(|subject| subject.name.clone())
                        .collect(),
                    after: placed
                        .after
                        .iter()
                        .map(|link| ReadingOrderLink {
                            path: path_of(link.document),
                            reasons: link
                                .reasons
                                .iter()
                                .map(|reason| match reason {
                                    reading::Reason::Names(subjects) => ReadingOrderReason::Names {
                                        subjects: subjects
                                            .iter()
                                            .map(|subject| subject.name.clone())
                                            .collect(),
                                    },
                                    reading::Reason::Level => ReadingOrderReason::Level,
                                    reading::Reason::Volume => ReadingOrderReason::Volume,
                                    reading::Reason::Narrower => ReadingOrderReason::Narrower,
                                    reading::Reason::Cites => ReadingOrderReason::Cites,
                                })
                                .collect(),
                        })
                        .collect(),
                    needed_by: placed.needed_by,
                    theme_share: theme_shares[index],
                }
            })
            .collect();

        Ok(ReadingOrder {
            root: asked.clone(),
            documents: ordered,
            subjects: outcome
                .subjects
                .into_iter()
                .map(|(subject, documents)| ReadingOrderSubject {
                    name: subject.name,
                    documents,
                })
                .collect(),
            theme,
        })
    }
    /// The citation graph of every document under `query.root`, or under
    /// every library root when it is absent. Built from the same DOI-keyed
    /// edges as [`Self::citation_links`]; documents are listed the way the
    /// file list lists them, so a node carries the same enriched entry.
    pub async fn citation_graph(
        self: Arc<Self>,
        query: CitationGraphQuery,
    ) -> Result<CitationGraph, String> {
        let settings = self.settings().await;
        let scope = self
            .citation_scope(&settings, query.root.as_deref())
            .await?;
        let cache = self
            .metadata_cache()
            .ok_or_else(|| "Metadata cache is unavailable".to_string())?;
        let guard = cache
            .lock()
            .map_err(|_| "Metadata cache lock failed".to_string())?;

        let mut coverage = CitationCoverage {
            files: scope.files.len(),
            references_provider: references_provider_enabled(&settings),
            ..CitationCoverage::default()
        };
        let mut index_by_doi: HashMap<String, usize> = HashMap::new();
        let mut documents: Vec<CitationGraphDocument> = Vec::new();
        let mut without_doi = Vec::new();
        for entry in scope.files {
            if let Some(doi) = entry.doi.as_deref().and_then(normalize_doi) {
                let index = *index_by_doi.entry(doi.clone()).or_insert_with(|| {
                    documents.push(CitationGraphDocument {
                        doi,
                        entries: Vec::new(),
                        cited_by: Vec::new(),
                        references_fetched: false,
                    });
                    documents.len() - 1
                });
                documents[index].entries.push(entry);
                continue;
            }
            let Some(identity) = FileIdentity::from_entry(&entry) else {
                coverage.metadata_unavailable += 1;
                continue;
            };
            match guard.get_valid(&entry.path, identity) {
                Ok(Some(_)) => without_doi.push(entry),
                Ok(None) => coverage.pending_metadata += 1,
                Err(e) => return Err(format!("Metadata cache read failed: {e:#}")),
            }
        }

        let dois: Vec<String> = documents.iter().map(|d| d.doi.clone()).collect();
        let fetched = guard.citations_fetched(&dois).map_err(|e| e.to_string())?;
        let mut edges = Vec::new();
        let mut external: HashMap<String, ExternalCitedWork> = HashMap::new();
        for (source, doi) in dois.iter().enumerate() {
            if !fetched.contains(doi) {
                continue;
            }
            documents[source].references_fetched = true;
            for reference in guard.citation_references(doi).map_err(|e| e.to_string())? {
                match index_by_doi.get(&reference.doi) {
                    Some(&target) => {
                        edges.push(CitationGraphEdge { source, target });
                        documents[target].cited_by.push(source);
                    }
                    None => external
                        .entry(reference.doi.clone())
                        .or_insert_with(|| ExternalCitedWork {
                            library_entry: scope.library.get(&reference.doi).cloned(),
                            reference,
                            cited_by: Vec::new(),
                        })
                        .cited_by
                        .push(source),
                }
            }
        }
        drop(guard);
        coverage.with_doi = documents.len();
        coverage.references_fetched = fetched.len();

        let external_total = external.len();
        let threshold = query.min_external_citations.max(1);
        let mut external: Vec<ExternalCitedWork> = external
            .into_values()
            .filter(|work| work.cited_by.len() >= threshold)
            .collect();
        external.sort_by(|a, b| {
            b.cited_by
                .len()
                .cmp(&a.cited_by.len())
                .then_with(|| a.reference.doi.cmp(&b.reference.doi))
        });

        Ok(CitationGraph {
            documents,
            edges,
            external,
            external_total,
            without_doi,
            coverage,
        })
    }
    /// Ask the enabled providers again for the references of every document
    /// in the scope whose references none has answered for. Runs the same
    /// metadata fill a listing runs, in the background; its progress arrives
    /// as `file-metadata-updated`. Returns how many documents were queued.
    pub async fn fetch_missing_citations(
        self: Arc<Self>,
        query: CitationGraphQuery,
    ) -> Result<usize, String> {
        let settings = self.settings().await;
        if !references_provider_enabled(&settings) {
            return Err("No enabled integration fetches references".to_string());
        }
        let scope = self
            .citation_scope(&settings, query.root.as_deref())
            .await?;
        let cache = self
            .metadata_cache()
            .ok_or_else(|| "Metadata cache is unavailable".to_string())?;
        // One file per DOI: references are keyed by the work, not the copy.
        let mut path_by_doi: HashMap<String, PathBuf> = HashMap::new();
        for entry in scope.files {
            if let Some(doi) = entry.doi.as_deref().and_then(normalize_doi) {
                path_by_doi.entry(doi).or_insert(entry.path);
            }
        }
        let dois: Vec<String> = path_by_doi.keys().cloned().collect();
        let fetched = cache
            .lock()
            .map_err(|_| "Metadata cache lock failed".to_string())?
            .citations_fetched(&dois)
            .map_err(|e| e.to_string())?;
        let mut missing: Vec<PathBuf> = path_by_doi
            .into_iter()
            .filter(|(doi, _)| !fetched.contains(doi))
            .map(|(_, path)| path)
            .collect();
        missing.sort();
        let queued = missing.len();
        if queued > 0 {
            self.spawn_metadata_fill(missing, settings, cache);
        }
        Ok(queued)
    }
    /// The files a citation graph covers, and every library file by DOI for
    /// works the scope cites that live in another root. Lists every library
    /// root, as [`Self::citation_links`] does.
    pub(super) async fn citation_scope(
        &self,
        settings: &Settings,
        root: Option<&Path>,
    ) -> Result<CitationScope, String> {
        let (library_roots, _) = library_roots(settings);
        let root = match root {
            Some(root) => {
                let root = Self::canonicalize_search_root(root)?;
                Self::ensure_path_in_library(&root, &library_roots, "Citation graph root")?;
                Some(root)
            }
            None => None,
        };
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        let mut library: HashMap<String, FileEntry> = HashMap::new();
        for listing_root in library_roots {
            let listed = self
                .list_files(listing_root)
                .await
                .map_err(|e| e.to_string())?;
            for entry in listed.files {
                if !seen.insert(entry.path.clone()) {
                    continue;
                }
                let in_scope = root
                    .as_ref()
                    .is_none_or(|root| entry.path.starts_with(root));
                if let Some(doi) = entry.doi.as_deref().and_then(normalize_doi) {
                    library.entry(doi).or_insert_with(|| entry.clone());
                }
                if in_scope {
                    files.push(entry);
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(CitationScope { files, library })
    }
}

pub(super) struct CitationScope {
    pub(super) files: Vec<FileEntry>,
    pub(super) library: HashMap<String, FileEntry>,
}
