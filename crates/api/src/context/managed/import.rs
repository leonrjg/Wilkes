//! Importing documents and projections into the managed corpus.

use crate::context::*;

/// One managed import waiting for, or holding, its corpus's import lock —
/// counted for the corpus status. Builds are not counted here: a projection's
/// catch-up runs in the build slot, and the status reads it off that.
pub(in crate::context) struct PendingManagedImport<'a>(pub(in crate::context) &'a AtomicU64);

impl<'a> PendingManagedImport<'a> {
    pub(in crate::context) fn new(counter: &'a AtomicU64) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for PendingManagedImport<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AppContext {
    pub(in crate::context) fn retain_managed_snapshot(
        data_dir: &Path,
        source: &Path,
    ) -> anyhow::Result<(PathBuf, PathBuf, String)> {
        let source = std::fs::canonicalize(source)
            .map_err(|error| anyhow::anyhow!("External source cannot be read: {error}"))?;
        anyhow::ensure!(source.is_file(), "External source is not a regular file");
        let before = std::fs::metadata(&source)?;
        let managed_sources = data_dir.join("managed_sources");
        let temporary_dir = managed_sources.join(".imports");
        std::fs::create_dir_all(&temporary_dir)?;
        let temporary = temporary_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::copy(&source, &temporary)?;
        let after = std::fs::metadata(&source)?;
        let copied = std::fs::metadata(&temporary)?;
        let source_sha256 = wilkes_core::embed::identity::sha256_file(&temporary)?;
        let source_after_sha256 = wilkes_core::embed::identity::sha256_file(&source)?;
        if before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || copied.len() != before.len()
            || source_after_sha256 != source_sha256
        {
            let _ = std::fs::remove_file(&temporary);
            consumer_bail!(
                ConsumerErrorCode::SourceChangedDuringImport,
                "the source changed while it was being copied",
            );
        }
        let snapshot_dir = managed_sources.join(&source_sha256);
        std::fs::create_dir_all(&snapshot_dir)?;

        // Identical bytes already retained under another original name reuse
        // that immutable copy; display provenance remains in the index row.
        let source_extension = source.extension().and_then(|value| value.to_str());
        let existing = std::fs::read_dir(&snapshot_dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.is_file()
                    && path.extension().and_then(|value| value.to_str()) == source_extension
            });
        let snapshot = if let Some(existing) = existing {
            if wilkes_core::embed::identity::sha256_file(&existing)? != source_sha256 {
                let _ = std::fs::remove_file(&temporary);
                consumer_bail!(
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    "retained snapshot digest mismatch",
                );
            }
            let _ = std::fs::remove_file(&temporary);
            existing
        } else {
            let destination = snapshot_dir.join(Self::sanitize_managed_source_name(&source));
            match std::fs::rename(&temporary, &destination) {
                Ok(()) => destination,
                Err(_error) if destination.exists() => {
                    let destination_sha256 = wilkes_core::embed::identity::sha256_file(
                        &destination,
                    )
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&temporary);
                    })?;
                    if destination_sha256 != source_sha256 {
                        let _ = std::fs::remove_file(&temporary);
                        consumer_bail!(
                            ConsumerErrorCode::DocumentIndexIncomplete,
                            "retained snapshot digest mismatch",
                        );
                    }
                    let _ = std::fs::remove_file(&temporary);
                    destination
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error.into());
                }
            }
        };
        let mut permissions = std::fs::metadata(&snapshot)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&snapshot, permissions)?;
        let relative = snapshot.strip_prefix(data_dir)?.to_path_buf();
        Ok((snapshot, relative, source_sha256))
    }
    pub async fn import_managed_document(
        self: &Arc<Self>,
        corpus_id: String,
        idempotency_key: String,
        source_path: PathBuf,
        source_workspace: Option<Arc<AppContext>>,
        original_source_provenance: serde_json::Value,
    ) -> Result<ManagedDocumentExport, ConsumerError> {
        let _pending = PendingManagedImport::new(&self.managed_pending_imports);
        let _import_guard = self.managed_import_lock.lock().await;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err(ConsumerError::new(
                ConsumerErrorCode::IdempotencyKeyConflict,
                "idempotency key must contain 1 to 256 bytes",
            ));
        }
        let settings = self.settings().await;
        // Asked of `FileType::detect` rather than of `supported_extensions`
        // directly, because that list is the *text* extensions and a document
        // is not in it — a copy of the rule here refused every PDF and EPUB
        // the one gate admits. See `FileType::detect`.
        if FileType::detect(&source_path, &settings.supported_extensions).is_none() {
            return Err(ConsumerError::untyped(format!(
                "Unsupported managed import file: {}",
                source_path.display()
            )));
        }
        let metadata = std::fs::metadata(&source_path)
            .map_err(|error| format!("External source cannot be read: {error}"))?;
        if metadata.len() > settings.max_file_size {
            return Err(ConsumerError::untyped(format!(
                "Managed import exceeds the configured {} byte file limit",
                settings.max_file_size
            )));
        }
        let (snapshot_path, relative_path, source_sha256) = {
            let data_dir = self.data_dir.clone();
            let source_path = source_path.clone();
            tokio::task::spawn_blocking(move || {
                Self::retain_managed_snapshot(&data_dir, &source_path)
            })
            .await
            .map_err(|error| format!("Snapshot task panicked: {error}"))?
            .map_err(|error| format!("Could not retain managed snapshot: {error:#}"))?
        };
        let expected_identity = {
            let guard = self.index.lock().clone();
            let guard = guard
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| {
                    ConsumerError::new(
                        ConsumerErrorCode::ManagedWorkspaceNotFound,
                        "runtime is not ready",
                    )
                })?
                .embedding_space_identity()
                .map_err(|error| error.to_string())?
        };
        let expected_space = expected_identity.id();
        let recipe = ExtractionRecipe::for_path(
            &snapshot_path,
            &wilkes_core::extract::production_registry(),
            settings.semantic.chunk_size,
            settings.semantic.chunk_overlap,
        );

        // Bound to a `let` before the `if let`, not inlined into it. A block
        // used as an `if let` scrutinee lives until the end of the whole `if
        // let` statement, so the index lock guard inside it would still be
        // held across the `.await` below — and a guard that reaches an await
        // makes the future non-Send, which surfaces a route at a time as
        // "handler does not implement Handler". `managed_export` became async
        // when the outline read moved to the blocking pool; this is what that
        // move requires of its callers.
        let existing = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?
                .managed_document_for_import_key(&idempotency_key, &source_sha256, &recipe.id())
                .map_err(|error| error.to_string())?
        };
        if let Some(existing) = existing {
            return Self::managed_export(corpus_id, existing, snapshot_path, true).await;
        }

        // Bound to a `let` before the `if let`, not inlined into it. A block
        // used as an `if let` scrutinee lives until the end of the whole `if
        // let` statement, so the index lock guard inside it would still be
        // held across the `.await` below — and a guard that reaches an await
        // makes the future non-Send, which surfaces a route at a time as
        // "handler does not implement Handler". `managed_export` became async
        // when the outline read moved to the blocking pool; this is what that
        // move requires of its callers.
        //
        // The rendition already held for these bytes is adopted only if this
        // runtime would produce it. The recipe covers the chunk size and
        // overlap, the extractor version and the image analyzer, and any one
        // of them differing moves the passages — so a rendition read under
        // another recipe is a different rendition of the same document, not
        // this import's answer. Adopting it anyway is how a recipe change came
        // to mean nothing: every re-extraction of an already-imported document
        // handed back the passages it was run to replace, and bound its fresh
        // idempotency key to them, carrying the stale recipe forward into a
        // new binding. Falling through re-extracts, which is the one thing a
        // recipe version exists to make happen.
        let existing = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let existing = index
                .managed_document_for_path(&snapshot_path)
                .map_err(|error| error.to_string())?
                .filter(|held| held.extraction_recipe_id == recipe.id());
            if let Some(existing) = existing.as_ref() {
                index
                    .bind_managed_import_key(&idempotency_key, existing)
                    .map_err(|error| error.to_string())?;
            }
            existing
        };
        if let Some(existing) = existing {
            return Self::managed_export(corpus_id, existing, snapshot_path, true).await;
        }

        let adopted = if let Some(source_context) = source_workspace {
            let source_index_arc = source_context.index.lock().clone();
            let source_guard = source_index_arc
                .lock()
                .map_err(|_| "Source semantic index lock was poisoned")?;
            source_guard
                .as_ref()
                .map(|source_index| {
                    source_index.verified_file_for_adoption(
                        &source_sha256,
                        &recipe,
                        &expected_space,
                        &snapshot_path,
                    )
                })
                .transpose()
                .map_err(|error| format!("Could not verify source rendition: {error:#}"))?
                .flatten()
        } else {
            None
        };

        let reused = adopted.is_some();
        // Extracted, never embedded. The canonical corpus owns passages and
        // their identity; the coordinates for them belong to its projections,
        // and each one computes its own. Asking for an embedder here meant
        // every import loaded the model the corpus was *created* under —
        // frozen at creation, unreachable from settings — and embedded every
        // chunk into an index that nothing reads once a store serves a
        // projection. Two model loads and two passes per document, one of them
        // for no reader.
        //
        // A whole-file adoption is still taken when one is offered: it brings
        // the rendition, which is the expensive half and the half this corpus
        // actually owns. Whatever vectors ride along with it are simply not
        // stored.
        let prepared = if let Some(mut prepared) = adopted {
            for (_, vector) in prepared.chunks.iter_mut() {
                vector.clear();
            }
            prepared
        } else {
            let snapshot_for_task = snapshot_path.clone();
            let chunk_size = settings.semantic.chunk_size;
            let chunk_overlap = settings.semantic.chunk_overlap;
            tokio::task::spawn_blocking(move || {
                let extractors = wilkes_core::extract::production_registry();
                SemanticIndex::prepare_file_without_vectors(
                    &snapshot_for_task,
                    &extractors,
                    chunk_size,
                    chunk_overlap,
                )
            })
            .await
            .map_err(|error| format!("Managed extraction task panicked: {error}"))?
            .map_err(|error| format!("Could not extract managed snapshot: {error:#}"))?
        };
        let expected_chunks = prepared.chunks.len();
        if expected_chunks == 0 {
            return Err(ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "document produced no chunks",
            ));
        }
        let managed = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            let managed = index
                .write_file_with_recipe(
                    prepared,
                    &recipe,
                    Some(&relative_path),
                    Some(&original_source_provenance),
                    true,
                    reused,
                    Some(&idempotency_key),
                )
                .map_err(|error| format!("Could not publish managed rendition: {error:#}"))?;
            managed
        };
        if managed.source_sha256 != source_sha256 || managed.chunks.len() != expected_chunks {
            return Err(ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "publication verification failed",
            ));
        }
        Self::managed_export(corpus_id, managed, snapshot_path, reused).await
    }
    /// Materialize one embedding projection from the canonical admitted
    /// rendition. The immutable source, extracted text, chunk boundaries, and
    /// stable refs remain owned by `canonical`; this context computes only its
    /// model-specific vectors and projection rows.
    /// Vectors this workspace's index can lend for the given chunk texts.
    ///
    /// Synchronous on purpose, and separate from its async caller for the same
    /// reason: it holds the index lock, and a lock guard that reaches an
    /// `.await` makes the whole future non-Send. Confining it here means the
    /// guard cannot outlive the lookup no matter what the caller does after.
    ///
    /// Space equality is checked, and it is the only thing checked. That is
    /// what a vector's readability actually turns on — same engine, same
    /// model, same width — and after the identity cut it is the whole of what
    /// a space id says. Anything that cannot be answered is answered as
    /// "nothing to adopt", said out loud, and the caller embeds instead.
    /// Vectors this workspace's index can lend, and — when it can lend none —
    /// which of the several possible reasons that was.
    ///
    /// "0 of 605 chunks adopted" is a number with four causes behind it and no
    /// way to tell them apart, which is the diagnosability hole this whole
    /// investigation kept falling into. Each one is named here, at the level
    /// the count itself is logged at, so a miss explains itself the first time
    /// rather than after a round trip.
    pub(in crate::context) fn adoptable_vectors(
        &self,
        expected: &wilkes_core::embed::identity::EmbeddingSpaceIdentity,
        text_hashes: &[String],
        source_sha256: &str,
        expected_recipe: &str,
        label: &str,
    ) -> std::collections::HashMap<String, Vec<f32>> {
        let empty = std::collections::HashMap::new;
        let index_arc = self.index.lock().clone();
        let guard = match index_arc.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!("{label} index lock was poisoned; embedding instead");
                return empty();
            }
        };
        let Some(index) = guard.as_ref() else {
            tracing::info!("{label} has no index open, so there is nothing to adopt");
            return empty();
        };
        let space = match index.embedding_space_id() {
            Ok(space) => space,
            Err(error) => {
                tracing::info!("{label} names no embedding space, so nothing to adopt: {error}");
                return empty();
            }
        };
        if space != expected.id() {
            tracing::info!(
                "{label} is space {} and this projection is {} ({} {} @{}); nothing to adopt",
                space.as_str(),
                expected.id().as_str(),
                expected.engine.as_str(),
                expected.model_id,
                expected.dimension,
            );
            return empty();
        }
        let found = match index.vectors_for_text_hashes(text_hashes) {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!("could not read vectors from {label}: {error:#}");
                return empty();
            }
        };
        if found.is_empty() {
            // Same space, same document, no shared text. That is chunking:
            // the boundaries moved, so the passages are different passages,
            // and their vectors are for spans this corpus does not have. Not a
            // refusal to relax — the two indexes genuinely hold different
            // things — but the remedy is concrete, so name it.
            match index.renditions_for_source(source_sha256) {
                Ok(renditions) if renditions.is_empty() => tracing::info!(
                    "{label} shares this projection's space but has never indexed this \
                     document, so there is nothing of it to adopt"
                ),
                Ok(renditions) => tracing::info!(
                    "{label} shares this projection's space and holds this document as {}, \
                     but read under {} — this corpus reads it as {}, so the passages are \
                     different passages and no vector is reusable. An extraction recipe \
                     covers the chunk size and overlap, the extractor version, and the image \
                     analyzer, and any one of them differing moves the text; re-index that \
                     workspace under this runtime to make them the same passages.",
                    renditions
                        .iter()
                        .map(|(_, chunks)| format!("{chunks} chunks"))
                        .collect::<Vec<_>>()
                        .join(" and "),
                    renditions
                        .iter()
                        .map(|(recipe, _)| recipe.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    expected_recipe,
                ),
                Err(error) => {
                    tracing::warn!("could not describe {label}'s renditions: {error:#}")
                }
            }
        }
        found
    }
    /// Brings one projection level with the canonical corpus for one document,
    /// and answers nothing but whether it succeeded.
    ///
    /// Deliberately not a `ManagedDocumentExport`. An export carries the
    /// document's declared outline, and reading that outline is a full read of
    /// the snapshot from disk — the text layer of every page, sanitized —
    /// which this path has no consumer for: catching up is a write, and its
    /// one caller drops whatever it is handed. Returning an export made an
    /// idempotent no-op cost a complete parse of every document in the corpus,
    /// once per call, to build a value that was discarded a frame later.
    /// `import_managed_document` is the path whose export is an HTTP response
    /// body, and it is the only one that builds one.
    pub async fn import_managed_projection(
        self: &Arc<Self>,
        idempotency_key: String,
        canonical: &Arc<Self>,
        canonical_snapshot_path: PathBuf,
        original_source_provenance: serde_json::Value,
        source_workspace: Option<&Arc<Self>>,
    ) -> Result<(), ConsumerError> {
        let _import_guard = self.managed_import_lock.lock().await;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err(ConsumerError::new(
                ConsumerErrorCode::IdempotencyKeyConflict,
                "idempotency key must contain 1 to 256 bytes",
            ));
        }
        let source_sha256 = wilkes_core::embed::identity::sha256_file(&canonical_snapshot_path)
            .map_err(|error| format!("Could not verify canonical managed snapshot: {error:#}"))?;
        let expected_identity = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| {
                    ConsumerError::new(
                        ConsumerErrorCode::ManagedWorkspaceNotFound,
                        "runtime is not ready",
                    )
                })?
                .embedding_space_identity()
                .map_err(|error| error.to_string())?
        };

        // The canonical rendition first, because everything below is *about*
        // it — including whether this import has already been done. It costs a
        // read of passages already in the database and no extraction, which is
        // the whole point of this path.
        //
        // Its recipe id comes from the rendition and is never recomputed here.
        // A projection that derived one from its own runtime would write a
        // different `rendition_id` for the same passages, and every space's
        // `chunk_ref`s have to be the canonical's.
        let (mut prepared, canonical_recipe_id) = {
            let index_arc = canonical.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Canonical semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| {
                    ConsumerError::new(
                        ConsumerErrorCode::ManagedWorkspaceNotFound,
                        "canonical index is unavailable",
                    )
                })?
                .managed_file_structure_for_reembedding(
                    &canonical_snapshot_path,
                    &canonical_snapshot_path,
                )
                .map_err(|error| format!("Could not read canonical rendition: {error:#}"))?
                .ok_or_else(|| {
                    ConsumerError::new(
                        ConsumerErrorCode::DocumentIndexIncomplete,
                        "canonical snapshot is not admitted",
                    )
                })?
        };

        // Bound to a `let` before the `if let`, not inlined into it. A block
        // used as an `if let` scrutinee lives until the end of the whole `if
        // let` statement, so the index lock guard inside it would still be
        // alive for the whole body — and a guard that reaches an await makes
        // the future non-Send, which surfaces a route at a time as "handler
        // does not implement Handler". That is what happened here while the
        // body awaited `managed_export`; the binding is what keeps a body that
        // awaits again from reintroducing it.
        let existing = {
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            guard
                .as_ref()
                .ok_or_else(|| "Managed index unavailable".to_string())?
                .managed_document_for_import_key(
                    &idempotency_key,
                    &source_sha256,
                    &canonical_recipe_id,
                )
                .map_err(|error| error.to_string())?
        };
        if let Some(existing) = existing {
            // The whole of the reuse path. Said rather than passed over in
            // silence because "already held" and "embedded just now" cost
            // different minutes, and a catch-up that is entirely no-ops should
            // be readable in the log as one.
            tracing::info!(
                "projection import: this key already holds {} chunks, nothing to do",
                existing.chunks.len()
            );
            return Ok(());
        }
        if prepared.chunks.is_empty() {
            return Err(ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "document produced no chunks",
            ));
        }
        // Coordinates this corpus already has, taken by content.
        //
        // The chunks are the canonical rendition's and stay that way — their
        // ordinals, byte ranges and `chunk_ref`s are what every citation
        // points at, and none of that comes from here. Only the vectors are
        // adopted, matched per chunk by the hash of its text, which is the
        // whole of the evidence needed: an embedding is a function of its text
        // and its model, so the same text in the same space is the same
        // vector wherever it was computed.
        //
        // Two places are asked. The workspace the document was imported from,
        // which is the case this exists for — a library the user already
        // indexed under this very model. And this projection's own index,
        // which catches a passage repeated across documents for free.
        //
        // Space equality *is* checked, because it is the one thing that
        // genuinely decides whether a vector may be read here at all. After
        // the identity cut that is engine, model and width — not a fingerprint
        // of a model cache, and not a recipe version.
        let text_hashes: Vec<String> = prepared
            .chunks
            .iter()
            .map(|(chunk, _)| wilkes_core::embed::identity::sha256_bytes(chunk.text.as_bytes()))
            .collect();
        // One vector per distinct text hash, whatever its origin: adopted from
        // the source workspace or this space below, or embedded fresh further
        // down. Every chunk is then filled by looking its hash up here, so two
        // chunks with the same text share one vector instead of competing for
        // it.
        let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
        if source_workspace.is_none() {
            // The import named no library to adopt from — a plain path import,
            // or a caller that resolved one and did not pass it on. Worth
            // saying, because it is the difference between "nothing to reuse"
            // and "nowhere was asked".
            tracing::info!(
                "no source workspace was offered for this import, so only this space is asked"
            );
        }
        for (label, candidate) in [
            ("source workspace", source_workspace),
            ("this space", Some(self)),
        ] {
            let Some(candidate) = candidate else { continue };
            for (hash, embedding) in candidate.adoptable_vectors(
                &expected_identity,
                &text_hashes,
                &source_sha256,
                &canonical_recipe_id,
                label,
            ) {
                vectors.entry(hash).or_insert(embedding);
            }
        }

        let adopted_chunks = text_hashes
            .iter()
            .filter(|hash| vectors.contains_key(*hash))
            .count();
        // Owned, because the embedding runs on the blocking pool and cannot
        // borrow `prepared`. `Embedder` is a blocking trait — the worker-backed
        // implementation waits on its subprocess by blocking the calling
        // thread — so an async caller has to leave the runtime thread first.
        let (missing_hashes, missing): (Vec<String>, Vec<String>) = texts_to_embed(
            prepared.chunks.iter().map(|(chunk, _)| chunk.text.as_str()),
            &text_hashes,
            &vectors,
        )
        .into_iter()
        .unzip();
        let missing_count = missing.len();
        tracing::info!(
            "projection import: {} of {} chunks adopted, {} distinct texts to embed for the other {}",
            adopted_chunks,
            prepared.chunks.len(),
            missing_count,
            prepared.chunks.len() - adopted_chunks
        );
        let computed = if missing.is_empty() {
            Vec::new()
        } else {
            let embedder = self.embedder.lock().clone().ok_or_else(|| {
                ConsumerError::new(
                    ConsumerErrorCode::ManagedWorkspaceNotFound,
                    "managed embedder is unavailable",
                )
            })?;
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = missing.iter().map(String::as_str).collect();
                embedder.embed_passages(&refs)
            })
            .await
            .map_err(|error| format!("Canonical rendition embedding task panicked: {error}"))?
            .map_err(|error| format!("Could not embed canonical rendition: {error:#}"))?
        };
        if computed.len() != missing_count {
            return Err(ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                format!(
                    "embedder returned {} vectors for {} texts it was asked to embed",
                    computed.len(),
                    missing_count
                ),
            ));
        }
        vectors.extend(missing_hashes.into_iter().zip(computed));
        fill_chunk_vectors(&mut prepared.chunks, &text_hashes, &vectors)?;
        let expected_chunks = prepared.chunks.len();
        let managed = {
            let index_arc = self.index.lock().clone();
            let mut guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned")?;
            let index = guard
                .as_mut()
                .ok_or_else(|| "Managed index unavailable".to_string())?;
            index
                .write_file_with_recipe_id(
                    prepared,
                    &canonical_recipe_id,
                    None,
                    Some(&original_source_provenance),
                    true,
                    false,
                    Some(&idempotency_key),
                )
                .map_err(|error| format!("Could not publish embedding projection: {error:#}"))?
        };
        if managed.source_sha256 != source_sha256 || managed.chunks.len() != expected_chunks {
            return Err(ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "projection verification failed",
            ));
        }
        Ok(())
    }
}
