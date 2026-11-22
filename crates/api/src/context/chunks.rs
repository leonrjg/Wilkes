//! Chunk lookups: resolution, accumulation, similarity, search, and file
//! chunk, outline and figure exports.

use super::*;

/// One probe of a managed chunk search: a vector the caller already holds, or
/// the text it wants searched for.
///
/// Text is the better form where the caller has a choice. It is one round trip
/// rather than two, and it is the only form under which the query role can
/// reach the embedder at all.
#[derive(Clone, Debug)]
pub enum SearchProbeInput {
    Vector(Vec<f32>),
    Text(String),
}

impl AppContext {
    pub async fn accumulate_chunks(
        &self,
        groups: Vec<Vec<ChunkRef>>,
    ) -> Result<ChunkAccumulations, ConsumerError> {
        if groups.is_empty() {
            return Err(ConsumerError::untyped("Aggregate request names no groups"));
        }
        let total: usize = groups.iter().map(Vec::len).sum();
        if groups.len() > MAX_ACCUMULATE_GROUPS || total > MAX_ACCUMULATE_REFS {
            return Err(ConsumerError::untyped(
                "Aggregate request exceeds the documented request cap",
            ));
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let space = index
                .embedding_space_id()
                .map_err(|error| error.to_string())?;
            let groups: Vec<ChunkAccumulation> = index
                .accumulate_chunk_refs(&groups)
                .map_err(|error| error.to_string())?;
            Ok(ChunkAccumulations {
                embedding_space_id: space.0,
                dimension: index.status().dimension,
                groups,
            })
        })
        .await
        .map_err(|error| format!("Aggregate task panicked: {error}"))?
    }
    pub async fn resolve_chunks(
        &self,
        refs: Vec<ChunkRef>,
        neighbors: Option<NeighborWindow>,
    ) -> Result<ChunkResolution, ConsumerError> {
        if refs.is_empty() {
            return Err(ConsumerError::untyped(
                "Resolve request names no chunk refs",
            ));
        }
        if refs.len() > MAX_RESOLVE_REFS {
            return Err(ConsumerError::untyped(format!(
                "Resolve request names {} chunk refs; {MAX_RESOLVE_REFS} is the most one request \
                 may ask for.",
                refs.len(),
            )));
        }
        if let Some(window) = neighbors {
            if window.before == 0 && window.after == 0 {
                return Err(ConsumerError::untyped(
                    "Resolve request asks for neighbors with a window of none before and none \
                     after; omit `neighbors` to ask for none.",
                ));
            }
            let carried = refs.len().saturating_mul(
                1usize
                    .saturating_add(window.before)
                    .saturating_add(window.after),
            );
            if carried > MAX_RESOLVE_REFS {
                return Err(ConsumerError::untyped(format!(
                    "Resolve request names {} chunk refs with {} neighbor(s) before and {} after \
                     each, up to {carried} passages; {MAX_RESOLVE_REFS} is the most one reply \
                     may carry.",
                    refs.len(),
                    window.before,
                    window.after,
                )));
            }
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let chunks = index
                .managed_chunks_for_refs(&refs)
                .map_err(|error| error.to_string())?;
            let neighbors = neighbors
                .map(|window| index.managed_chunk_neighbors(&refs, window.before, window.after))
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok(ChunkResolution {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                chunks: chunks.into_iter().map(ChunkExport::from).collect(),
                neighbors: neighbors
                    .map(|entries| entries.into_iter().map(ChunkNeighbors::from).collect()),
            })
        })
        .await
        .map_err(|error| format!("Resolve task panicked: {error}"))?
    }
    pub async fn chunk_similarity(
        &self,
        probes: Vec<ProbeRequest>,
        chunk_refs: Vec<ChunkRef>,
    ) -> Result<ChunkSimilarities, ConsumerError> {
        if probes.is_empty() {
            return Err(ConsumerError::untyped("Similarity request names no probes"));
        }
        let total = chunk_refs.len() + probes.iter().map(|probe| probe.scope.len()).sum::<usize>();
        if probes.len() > MAX_SIMILARITY_PROBES || total > MAX_SIMILARITY_REFS {
            return Err(ConsumerError::untyped(
                "Similarity request exceeds the documented request cap",
            ));
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let wanted: HashSet<ChunkRef> = chunk_refs
                .iter()
                .cloned()
                .chain(probes.iter().flat_map(|probe| probe.scope.iter().cloned()))
                .collect();
            let resolved = index
                .resolve_chunk_refs(&wanted)
                .map_err(|error| error.to_string())?;
            let reverse: HashMap<i64, ChunkRef> = resolved
                .iter()
                .map(|(stable_ref, rowid)| (*rowid, stable_ref.clone()))
                .collect();
            let searched: Vec<i64> = chunk_refs
                .iter()
                .map(|stable_ref| resolved[stable_ref])
                .collect();
            let probes: Vec<wilkes_core::embed::index::db::SimilarityProbe> = probes
                .into_iter()
                .map(|probe| wilkes_core::embed::index::db::SimilarityProbe {
                    vector: probe.vector,
                    scope: probe
                        .scope
                        .iter()
                        .map(|stable_ref| resolved[stable_ref])
                        .collect(),
                })
                .collect();
            let found = index
                .chunk_similarity(&probes, &searched)
                .map_err(|error| error.to_string())?;
            Ok(ChunkSimilarities {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                dimension: index.status().dimension,
                probes: found
                    .probes
                    .into_iter()
                    .map(|probe| ProbeSimilarity {
                        nearest_chunk_ref: probe
                            .nearest_chunk_id
                            .map(|rowid| reverse[&rowid].clone()),
                        similarity: probe.similarity,
                        scope_mean: probe.scope_mean,
                        scope_size: probe.scope_size,
                    })
                    .collect(),
                chunks: found
                    .chunks
                    .into_iter()
                    .map(|chunk| ChunkNearest {
                        chunk_ref: reverse[&chunk.chunk_id].clone(),
                        probe: chunk.probe,
                        similarity: chunk.similarity,
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Similarity task panicked: {error}"))?
    }
    pub async fn chunk_search(
        &self,
        probes: Vec<SearchProbeInput>,
        top_k: usize,
        min_similarity: f32,
    ) -> Result<ChunkSearch, ConsumerError> {
        if probes.is_empty() {
            return Err(ConsumerError::untyped("Search request names no probes"));
        }
        if probes.len() > MAX_SEARCH_PROBES || top_k == 0 || top_k > MAX_SEARCH_TOP_K {
            return Err(ConsumerError::untyped(
                "Search request exceeds the documented request cap",
            ));
        }
        let probes = self.resolve_search_probes(probes).await?;
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let hits = index
                .managed_chunk_search(&probes, top_k, min_similarity)
                .map_err(|error| error.to_string())?;
            Ok(ChunkSearch {
                embedding_space_id: index
                    .embedding_space_id()
                    .map_err(|error| error.to_string())?
                    .0,
                dimension: index.status().dimension,
                probes: hits
                    .into_iter()
                    .map(|hits| ProbeSearch {
                        hits: hits
                            .into_iter()
                            .map(|hit| ChunkSearchHit {
                                chunk_ref: hit.chunk_ref,
                                snapshot_id: hit.snapshot_id.0,
                                rendition_id: hit.rendition_id.0,
                                ordinal: hit.ordinal,
                                similarity: hit.similarity,
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Search task panicked: {error}"))?
    }
    /// Turn a mixed probe list into vectors, embedding the text ones in the
    /// query role and keeping the caller's order.
    ///
    /// The texts go in one batch because they must land in one space: two
    /// embed calls are two chances for a model to be swapped between them.
    pub(super) async fn resolve_search_probes(
        &self,
        probes: Vec<SearchProbeInput>,
    ) -> Result<Vec<Vec<f32>>, String> {
        let texts: Vec<String> = probes
            .iter()
            .filter_map(|probe| match probe {
                SearchProbeInput::Text(text) => Some(text.clone()),
                SearchProbeInput::Vector(_) => None,
            })
            .collect();
        if texts.is_empty() {
            return Ok(probes
                .into_iter()
                .map(|probe| match probe {
                    SearchProbeInput::Vector(vector) => vector,
                    SearchProbeInput::Text(_) => unreachable!("no text probes"),
                })
                .collect());
        }
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err("A text probe is empty and names nothing to search for".to_string());
        }
        let embedded = self.embed_texts_in_role(texts, EmbedRole::Query).await?;
        let mut embedded = embedded.vectors.into_iter();
        probes
            .into_iter()
            .map(|probe| match probe {
                SearchProbeInput::Vector(vector) => Ok(vector),
                SearchProbeInput::Text(_) => embedded
                    .next()
                    .ok_or_else(|| "Embedder returned fewer vectors than text probes".to_string()),
            })
            .collect()
    }
    /// One indexed document's chunks, in extraction order, with the ordinals
    /// every chunk export speaks in.
    ///
    /// Shared by [`Self::export_file_chunks`] and [`Self::export_chunk_text`]
    /// rather than written out twice. An ordinal is a position in *this*
    /// ordering and has no meaning apart from it, so a second copy of the sort
    /// would be a second definition of what "chunk 12" is — and a consumer that
    /// stored an ordinal from one export and redeemed it against the other
    /// would be reading whatever the drift left there.
    pub(super) async fn indexed_chunks(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<(PathBuf, Vec<ExportedChunk>), String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before exporting chunks.",
        )?;
        let (root, path) = self.export_file_path(root, path, "Chunk export").await?;

        let index_arc = self.index.lock().clone();
        let task_root = root.clone();
        let task_path = path.clone();
        let mut chunks = tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard.as_ref().ok_or_else(|| {
                "Semantic index unavailable. Build or restore the semantic index first.".to_string()
            })?;
            index
                .topic_chunks_for_file(&task_root, &task_path)
                .map_err(|error| format!("Could not load indexed chunks: {error:#}"))
        })
        .await
        .map_err(|error| format!("Chunk export task panicked: {error}"))??;

        // Stable reading order: extraction position, not row id.
        chunks.sort_by_key(|chunk| chunk.extraction_byte_range.start);
        let chunks = chunks
            .into_iter()
            .enumerate()
            .map(|(ordinal, chunk)| {
                // The route resolved an addressable index before reaching
                // here, so a passage without a ref is a fault rather than a
                // state — and naming it is worth more than a null would be.
                let (Some(chunk_ref), Some(text_sha256)) = (chunk.chunk_ref, chunk.text_sha256)
                else {
                    return Err(format!(
                        "Chunk at byte {} carries no stable reference; rebuild this index.",
                        chunk.extraction_byte_range.start
                    ));
                };
                Ok(ExportedChunk {
                    chunk: ChunkExport {
                        chunk_ref,
                        ordinal,
                        text: chunk.chunk_text,
                        text_sha256,
                        byte_range: chunk.extraction_byte_range,
                        origin: chunk.origin,
                    },
                    embedding: chunk.embedding,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok((path, chunks))
    }
    /// Canonicalize one library document for an export route. All file export
    /// endpoints share this boundary so a new read surface cannot accidentally
    /// weaken the library-root or supported-file restrictions — and so does a
    /// managed import that names a library file, which is one more reader.
    pub async fn export_file_path(
        &self,
        root: PathBuf,
        path: PathBuf,
        label: &str,
    ) -> Result<(PathBuf, PathBuf), String> {
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&root)?;
        Self::ensure_path_in_library(&root, &library_roots, &format!("{label} root"))?;
        let (path, _) =
            Self::canonicalize_supported_file(&root, &path, &settings.supported_extensions, label)?;
        Self::ensure_path_in_library(&path, &library_roots, &format!("{label} file"))?;
        Ok((root, path))
    }
    /// Export one document's declared outline without loading an index or an
    /// embedder. The outline retains the document's native page/byte locators;
    /// callers that need chunk ordinals should use [`Self::export_file_chunks`].
    pub async fn export_file_outline(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<FileOutlineExport, String> {
        let (_, path) = self.export_file_path(root, path, "Outline export").await?;
        let outline_path = path.clone();
        let declared_outline = tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            wilkes_core::extract::document_outline(&outline_path, &registry)
                .map_err(|error| format!("Could not read the document outline: {error:#}"))
        })
        .await
        .map_err(|error| format!("Outline export task panicked: {error}"))??;

        Ok(FileOutlineExport {
            file_path: path,
            outline: declared_outline.entries,
            extraction: declared_outline.diagnostics,
        })
    }
    /// Export one indexed document's chunks with their locators and stored
    /// vectors — the source of segment positions for consumers that must
    /// never re-extract (Underdog requirement E1). Read-only over the live
    /// semantic index.
    pub async fn export_file_chunks(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<FileChunkExport, String> {
        let model_id = self
            .embedder
            .lock()
            .clone()
            .map(|embedder| embedder.model_id().to_string());

        let (path, chunks) = self.indexed_chunks(root, path).await?;
        let dimension = chunks.first().map(|chunk| chunk.embedding.len());

        // The declared outline, resolved against the chunks just exported.
        //
        // Read from the file rather than the index: an outline is what the
        // author wrote, and the index stores what was extracted. It rides
        // along with an export instead of needing an endpoint and a second
        // round trip — the export is already the request that wants to know
        // where this document's sections begin. A file whose outline cannot be
        // read is reported as an error and not as an absent outline: "this
        // document declares no sections" is a claim consumers act on.
        let outline_path = path.clone();
        let outline = tokio::task::spawn_blocking(move || {
            let registry = wilkes_core::extract::production_registry();
            wilkes_core::extract::document_outline(&outline_path, &registry)
                .map_err(|error| format!("Could not read the document outline: {error:#}"))
        })
        .await
        .map_err(|error| format!("Outline task panicked: {error}"))??;
        let outline = resolve_outline(&outline.entries, &chunks);

        Ok(FileChunkExport {
            file_path: path,
            model_id,
            dimension,
            outline,
            chunks,
        })
    }
    /// One document's pictures: where each sits, how big it is, and the digest
    /// its pixels must match. No passage is involved — this is the browse half
    /// of the figure surface, and `chunk_figures` is the half that links.
    pub async fn export_file_figures(
        &self,
        root: PathBuf,
        path: PathBuf,
    ) -> Result<FileFigureExport, String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before exporting figures.",
        )?;
        let (_, path) = self.export_file_path(root, path, "Figure export").await?;

        let index_arc = self.index.lock().clone();
        let task_path = path.clone();
        let (inventory_known, figures) = tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard.as_ref().ok_or_else(|| {
                "Semantic index unavailable. Build or restore the semantic index first.".to_string()
            })?;
            index
                .figures_for_path(&task_path)
                .map_err(|error| format!("Could not read the figure inventory: {error:#}"))?
                .ok_or_else(|| {
                    format!(
                        "{} is not in this index; nothing can be said about its figures.",
                        task_path.display()
                    )
                })
        })
        .await
        .map_err(|error| format!("Figure export task panicked: {error}"))??;

        Ok(FileFigureExport {
            file_path: path,
            inventory_known,
            figures: figures.into_iter().map(ExportedFigure::from).collect(),
        })
    }
    /// The figures these passages bear on.
    ///
    /// `window` is bytes of slack outside a passage, and defaults to none: a
    /// widening is something the caller asks for, and the nearest figure has
    /// first claim on a budget the caller is spending.
    pub async fn chunk_figures(
        &self,
        refs: Vec<ChunkRef>,
        window: usize,
    ) -> Result<ChunkFigureResolution, ConsumerError> {
        if refs.is_empty() {
            return Err(ConsumerError::untyped("Figure request names no chunk refs"));
        }
        if refs.len() > MAX_RESOLVE_REFS {
            return Err(ConsumerError::untyped(format!(
                "Figure request names {} chunk refs; {MAX_RESOLVE_REFS} is the most one request \
                 may ask for.",
                refs.len(),
            )));
        }
        let index_arc = self.index.lock().clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let linked = index
                .figures_for_chunk_refs(&refs, window)
                .map_err(|error| error.to_string())?;
            Ok(ChunkFigureResolution {
                embedding_space_id: Some(
                    index
                        .embedding_space_id()
                        .map_err(|error| error.to_string())?
                        .0,
                ),
                chunks: linked
                    .into_iter()
                    .map(|entry| ChunkFigureExport {
                        chunk_ref: entry.chunk_ref.0,
                        inventory_known: entry.inventory_known,
                        figures: entry
                            .figures
                            .into_iter()
                            .map(|linked| LinkedFigureExport {
                                relation: linked.link.relation,
                                byte_distance: linked.link.byte_distance,
                                figure: ExportedFigure::from(linked.image),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
        .await
        .map_err(|error| format!("Figure task panicked: {error}"))?
    }
    /// One figure's pixels, re-derived from the document the rendition was
    /// extracted from and checked against the digest it recorded.
    ///
    /// The retained snapshot is preferred over the indexed path wherever the
    /// corpus kept one: it is immutable, so it is still the document the
    /// rendition was built from, which a workspace file on disk may no longer
    /// be. When neither can be opened, or when what is there hashes to
    /// something else, the request fails — a figure that is not the one that
    /// was read is worse than no figure.
    pub async fn figure_pixels(
        &self,
        address: FigureAddress,
        area_id: String,
        max_edge: Option<u32>,
    ) -> Result<FigurePixelsExport, ConsumerError> {
        if let Some(edge) = max_edge {
            if edge == 0 || edge > MAX_FIGURE_EDGE {
                return Err(ConsumerError::untyped(format!(
                    "max_edge must be between 1 and {MAX_FIGURE_EDGE}; got {edge}"
                )));
            }
        }
        let address = match address {
            FigureAddress::File { root, path } => {
                let (_, path) = self
                    .export_file_path(root, path, "Figure request")
                    .await
                    .map_err(ConsumerError::untyped)?;
                FigureAddress::File {
                    root: PathBuf::new(),
                    path,
                }
            }
            chunk => chunk,
        };

        let index_arc = self.index.lock().clone();
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            let index = guard
                .as_ref()
                .ok_or_else(|| "Semantic index unavailable".to_string())?;

            let document = match &address {
                FigureAddress::File { path, .. } => {
                    wilkes_core::embed::index::db::FigureDocument::Path(path.as_path())
                }
                FigureAddress::Chunk(chunk_ref) => {
                    wilkes_core::embed::index::db::FigureDocument::Chunk(chunk_ref)
                }
            };
            let source = index
                .figure_source(document, &area_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!("This index holds no figure {area_id} for that document.")
                })?;

            let path = source
                .managed_snapshot_relative_path
                .as_ref()
                .map(|relative| data_dir.join(relative))
                .filter(|snapshot| snapshot.exists())
                .unwrap_or(source.file_path);

            let rendered = wilkes_core::figure::render_figure(&path, &source.image, max_edge)
                .map_err(|error| format!("{error:#}"))?;
            Ok(FigurePixelsExport {
                bytes: rendered.png,
                media_type: "image/png",
                width: rendered.width,
                height: rendered.height,
                source_width: rendered.source_width,
                source_height: rendered.source_height,
                image_sha256: source.image.image_sha256,
            })
        })
        .await
        .map_err(|error| format!("Figure render task panicked: {error}"))?
    }
    /// Every document Wilkes serves under one library root, each with the
    /// number of passages the index holds for it.
    ///
    /// This is the browse half of the export surface. Wilkes decides what
    /// counts as a document — its supported extensions, its size limit, its
    /// ignore rules — and its index decides what can be exported, so a
    /// consumer that walked the directory itself would be reimplementing two
    /// rules it does not own and would disagree with `export_file_chunks` the
    /// moment either changed. `/api/files` cannot answer this: it is confined
    /// to the uploads directory, which a real library root never is.
    ///
    /// The listing is the filesystem's, the counts are the index's, and they
    /// are reported together rather than merged into a verdict: a file present
    /// but unindexed is a normal state with a fix in Wilkes, and only the
    /// caller knows whether it wants to show it, hide it or refuse it.
    pub async fn export_library_files(&self, root: PathBuf) -> Result<LibraryFileExport, String> {
        // The same refusal the chunk export makes, for the same reason: while
        // the index is being rewritten its counts describe neither the old
        // index nor the new one, and a listing that invited a person to pick
        // from them would be handing out exports that are about to fail.
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before listing library files.",
        )?;
        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Library listing root")?;

        let listing = self
            .list_files(root.clone())
            .await
            .map_err(|error| format!("Could not list library files: {error:#}"))?;

        let index_arc = self.index.lock().clone();
        let counts_root = root.clone();
        let counts = tokio::task::spawn_blocking(move || {
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            // No index is not an error here. The directory still has documents
            // in it, and "none of them is indexed yet" is exactly what a
            // person who has not built the index should be told — by seeing
            // their files with nothing behind them, not by an empty screen.
            let Some(index) = guard.as_ref() else {
                return Ok::<_, String>(std::collections::HashMap::new());
            };
            index
                .indexed_chunk_counts_for_root(&counts_root)
                .map_err(|error| format!("Could not read indexed passage counts: {error:#}"))
        })
        .await
        .map_err(|error| format!("Library listing task panicked: {error}"))??;

        let mut files: Vec<LibraryFile> = listing
            .files
            .into_iter()
            .map(|entry| LibraryFile {
                chunk_count: counts.get(&entry.path).copied().unwrap_or(0),
                path: entry.path,
                size_bytes: entry.size_bytes,
                extension: entry.extension,
                modified_at_ms: entry.modified_at_ms,
                title: entry.title,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(LibraryFileExport { root, files })
    }
}
