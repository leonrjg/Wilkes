//! Building the index: extracting, chunking and embedding files, and writing
//! them in batches.

use super::*;

/// What a build's path list is, relative to the root it is building.
///
/// This is not a preference. It decides whether the root's membership after
/// the build is *replaced by* what the build produced or *added to* it, and
/// getting that wrong either strands documents outside the root or deletes
/// coverage the build never looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildScope {
    /// Every indexable file under the root. Reaching the end of this list means
    /// the root contains exactly what the build produced, so anything not in it
    /// is gone from disk and should go from the root too.
    WholeRoot,
    /// A chosen subset — the documents a previous job never reached, or the
    /// ones the user asked to retry. The build knows nothing about the rest of
    /// the root, so it must not speak for them.
    Subset,
}

/// One document's settled verdict, as [`BuildReporter::settle_batch`] takes it.
pub(super) type SettledDocument = (PathBuf, DocumentOutcome, Option<String>, Option<i64>);

/// Why a batch did not finish.
pub(super) enum BatchStop {
    /// The build is being stopped. The batch's documents were killed, not
    /// broken, and are left unfinished for a continuation to pick up.
    Stopped,
    /// The batch was attempted and failed. Its documents are settled as failed
    /// and the error ends the build.
    Failed(anyhow::Error),
}

/// Whether a merge may speak for the whole root or only for what it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RootMembership {
    /// The source holds the root's entire membership; drop what it omits.
    Authoritative,
    /// The source holds part of it; leave everything else as it was.
    Additive,
}

/// How a build reports itself, and how it is stopped.
pub struct BuildOptions {
    /// The one channel through which the build names documents and stages.
    pub reporter: BuildReporter,
    /// Raised to stop the build. Checked between documents; the workers are
    /// killed separately, and the kill is what ends work already in flight.
    pub cancel_flag: Arc<AtomicBool>,
    pub scope: BuildScope,
}

impl BuildOptions {
    /// A build over every indexable file under the root.
    pub fn new(reporter: BuildReporter, cancel_flag: Arc<AtomicBool>) -> Self {
        Self {
            reporter,
            cancel_flag,
            scope: BuildScope::WholeRoot,
        }
    }

    /// A build over a chosen subset of the root's documents.
    pub fn over_subset(reporter: BuildReporter, cancel_flag: Arc<AtomicBool>) -> Self {
        Self {
            reporter,
            cancel_flag,
            scope: BuildScope::Subset,
        }
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }
}

/// One file extracted and waiting for a batch to fill before it is embedded.
///
/// This is a `PreparedFile` minus the embeddings, which is exactly what a
/// cross-file batch has to hold: the chunks are known, but which call will
/// embed them is not decided until the queue is full.
pub(super) struct PendingFile {
    pub(super) path: PathBuf,
    pub(super) recipe: ExtractionRecipe,
    pub(super) full_text: String,
    pub(super) chunks: Vec<Chunk>,
    pub(super) retained: RetainedExtraction,
}

/// A document's chunks, each with its vector, in chunk order.
pub(super) type EmbeddedChunks = Vec<(Chunk, Vec<f32>)>;

pub struct PreparedFile {
    pub path: PathBuf,
    /// Pairs of (chunk metadata, embedding vector), ready to write.
    pub chunks: Vec<(Chunk, Vec<f32>)>,
    /// The document's full extracted text, stored verbatim so exact (grep)
    /// search can scan it without re-extracting the source file. Empty when the
    /// document yielded no text.
    pub full_text: String,
    /// What this extraction leaves behind for the index to keep: the
    /// reading's own page areas, and where every picture the pages draw sits.
    /// Ranges into `full_text`, which is why the two are only ever written
    /// together.
    pub retained: RetainedExtraction,
}

impl SemanticIndex {
    /// Embed one queue of extracted files in a single call and write them.
    ///
    /// The whole queue is one batch because that is the point of queueing: the
    /// per-call cost is paid once for as many chunks as the model will take.
    /// A non-fatal embedder failure therefore costs the whole group, and says
    /// so by naming every file in it — a group is what was attempted, and
    /// reporting anything narrower would claim a per-file attempt that did not
    /// happen.
    ///
    /// Returns one settled outcome per file in the queue. It returns them
    /// rather than logging them because this is the only place that knows
    /// which files of a batch were written and which were skipped, and a
    /// skipped file that is only logged is a hole in the corpus that nothing
    /// can later name.
    pub(super) fn embed_and_write(
        idx: &mut Self,
        embedder: &dyn Embedder,
        queue: Vec<PendingFile>,
    ) -> anyhow::Result<Vec<SettledDocument>> {
        if queue.is_empty() {
            return Ok(Vec::new());
        }
        let batch_failed = |queue: &[PendingFile], detail: String| -> Vec<SettledDocument> {
            queue
                .iter()
                .map(|file| {
                    (
                        file.path.clone(),
                        DocumentOutcome::Failed,
                        Some(detail.clone()),
                        None,
                    )
                })
                .collect()
        };
        let texts: Vec<&str> = queue
            .iter()
            .flat_map(|file| file.chunks.iter().map(|c| c.text.as_str()))
            .collect();
        let total = texts.len();

        let embeddings = match embedder.embed_passages(&texts) {
            Ok(embeddings) => embeddings,
            Err(e) => {
                if Self::is_fatal_embedder_error(&e) {
                    return Err(e.context(format!(
                        "Fatal embedder error while indexing {} file(s) starting at {}",
                        queue.len(),
                        queue[0].path.display()
                    )));
                }
                error!(
                    "[SemanticIndex::build] skipping {} file(s) because embed_passages failed \
                     for their batch: {e:#}; skipped: {}",
                    queue.len(),
                    queue
                        .iter()
                        .map(|f| f.path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                return Ok(batch_failed(&queue, format!("{e:#}")));
            }
        };
        if embeddings.len() != total {
            error!(
                "[SemanticIndex::build] skipping {} file(s): embedder returned {} embeddings for \
                 {total} chunks",
                queue.len(),
                embeddings.len()
            );
            return Ok(batch_failed(
                &queue,
                format!(
                    "the embedder returned {} embeddings for {total} chunks",
                    embeddings.len()
                ),
            ));
        }

        let mut settled = Vec::with_capacity(queue.len());
        let mut embeddings = embeddings.into_iter();
        for file in queue {
            let chunk_count = file.chunks.len() as i64;
            let taken: Vec<Vec<f32>> = embeddings.by_ref().take(file.chunks.len()).collect();
            let prepared = PreparedFile {
                path: file.path.clone(),
                chunks: file.chunks.into_iter().zip(taken).collect(),
                full_text: file.full_text,
                retained: file.retained,
            };
            match idx.write_file_with_recipe(prepared, &file.recipe, None, None, false, false, None)
            {
                Ok(_) => {
                    settled.push((file.path, DocumentOutcome::Indexed, None, Some(chunk_count)))
                }
                Err(e) => {
                    error!(
                        "[SemanticIndex::build] skipping {}: failed to write index entry: {e:#}",
                        file.path.display()
                    );
                    settled.push((
                        file.path,
                        DocumentOutcome::Failed,
                        Some(format!("{e:#}")),
                        None,
                    ));
                }
            }
        }
        Ok(settled)
    }
    /// Embed and write one batch, and settle every document in it.
    ///
    /// Splitting this from [`Self::embed_and_write`] keeps one decision in one
    /// place: whether a batch that raised was *defeated* or *ended*. They look
    /// alike from inside the embedder and mean opposite things to the user.
    pub(super) fn flush_batch(
        idx: &mut Self,
        embedder: &dyn Embedder,
        queue: Vec<PendingFile>,
        options: &BuildOptions,
        done_units: &mut usize,
        total_units: usize,
    ) -> Result<(), BatchStop> {
        if queue.is_empty() {
            return Ok(());
        }
        let paths: Vec<PathBuf> = queue.iter().map(|file| file.path.clone()).collect();
        match Self::embed_and_write(idx, embedder, queue) {
            Ok(settled) => {
                // Each document in the batch spends its embedding unit here,
                // whatever its outcome: it is finished with either way.
                *done_units += paths.len();
                options
                    .reporter
                    .settle_batch(&settled, *done_units, total_units);
                Ok(())
            }
            Err(e) => {
                // A raised cancel flag and a killed worker say the same thing:
                // this batch was ended, not defeated. Its documents stay
                // unfinished, so a continuation picks them up rather than a
                // retry being offered for a failure that never happened — and
                // the loop stops here rather than submitting again, which would
                // find no process and start one.
                if options.cancelled() || crate::worker::fault::is_worker_gone(&e) {
                    info!(
                        "[SemanticIndex::build] embedding batch of {} document(s) ended: {e:#}",
                        paths.len()
                    );
                    return Err(BatchStop::Stopped);
                }
                let detail = format!("{e:#}");
                let settled: Vec<SettledDocument> = paths
                    .into_iter()
                    .map(|path| (path, DocumentOutcome::Failed, Some(detail.clone()), None))
                    .collect();
                *done_units += settled.len();
                options
                    .reporter
                    .settle_batch(&settled, *done_units, total_units);
                Err(BatchStop::Failed(e))
            }
        }
    }
    /// Full build: creates the database at `data_dir`, indexes every path, and
    /// returns the open index.
    ///
    /// Three passes, in this order: reuse what is unchanged, read every
    /// remaining document's figures, then extract and embed. Recognition is a
    /// pass of its own rather than a step inside the per-file loop because
    /// the two models it alternated with are large and slow in opposite ways.
    /// A document whose figures take minutes to read left the embedder idle
    /// past its timeout, and a batch of embeddings did the same to the
    /// recognizer, so a corpus of illustrated PDFs paid a model reload per
    /// alternation while holding both models resident in between. Separated,
    /// each pass keeps one model loaded, and the recognizer is released
    /// before the first embedding is asked for.
    ///
    /// What makes the second read of a document cheap is the annotation
    /// cache: the recognition pass fills it, and the extraction the embedding
    /// pass performs reads the answers back instead of recognizing again. A
    /// figure whose analysis came back partial is deliberately not cached, so
    /// that one case can still reach the recognizer during the embedding pass
    /// — a failure re-tried, not a schedule that quietly went back to
    /// interleaving.
    /// # Stopping
    ///
    /// Cancellation is a kill: the caller raises `options.cancel_flag` and then
    /// kills the workers, and the kill is what ends inference already running.
    /// This function's part is to notice between documents and stop asking for
    /// more — never to retry, and never to submit again after the flag is up,
    /// which would find no process and start one.
    ///
    /// # What a stopped build leaves behind
    ///
    /// Everything it finished. A build fills a temporary database and publishes
    /// it at the end, and this used to mean that stopping threw away every
    /// document read so far — a four-hundred-page book re-read from page one
    /// because the user paused overnight. The temporary database is still the
    /// write boundary; what changed is that an interrupted build publishes it
    /// *additively*, so the documents it completed are saved and the ones it
    /// never reached keep whatever coverage they already had. The job journal,
    /// not the index, is what records that the root is only partly rebuilt.
    pub fn build(
        data_dir: &Path,
        root_path: &Path,
        paths: &[PathBuf],
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        options: BuildOptions,
        indexing: &IndexingConfig,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let reporter = &options.reporter;

        let final_path = db_path(data_dir);
        let tmp_path = data_dir.join("semantic_index.db.tmp");

        // Reuse compatible global embeddings while keeping the temporary index
        // as the atomic root-membership boundary.
        let embedding_identity = embedder.embedding_space_identity();
        let reusable = Self::open_exact(data_dir, &embedding_identity).ok();

        let mut idx = Self::create_at_path_exact(&tmp_path, &embedding_identity, Some(root_path))?;

        // Extract into a queue and embed the queue, not one file at a time.
        // A corpus of small documents otherwise sends the model batches of one
        // or two chunks and pays the whole per-call cost — tokenizer setup,
        // worker round trip, JSON framing — for each of them. The queue is
        // flushed as soon as it holds a model batch, so peak memory is still
        // bounded to one file's chunks plus one batch's worth of others.
        //
        // A model with no preferred batch size is one whose vectors depend on
        // what else was in the batch, so it keeps the one-file-per-batch
        // grouping it had; regrouping its chunks would change them.
        let batch_target = embedder.preferred_batch_size();

        // Progress is counted in units of work, not files, because a document
        // is visited more than once and a bar that counted files would sit
        // still for whole passes. Each path costs one unit to extract and one
        // to embed — embedding is usually the slow half, and remotely it is
        // a network round trip, so a count that moved only on extraction
        // reached its end while the model was still working — plus one more
        // when its extractor enriches images. Known before any work starts,
        // so the denominator never moves under the user.
        let reads_figures = |path: &Path| -> bool {
            extractors
                .find(path, None)
                .is_some_and(|extractor| !extractor.image_analyzer_identity().is_empty())
        };
        let units_of = |path: &Path| -> usize { 2 + usize::from(reads_figures(path)) };
        let total_units: usize = paths.iter().map(|path| units_of(path)).sum();
        let mut done_units = 0usize;

        // Cancellation leaves each pass rather than the function, because the
        // work already done has to be published on the way out. `stopped` is
        // raised once and never lowered; every pass checks it before starting.
        let mut stopped = false;

        // ── Pass 1: what is unchanged is reused ─────────────────────────────
        //
        // First, so that a document the previous index already read costs
        // neither a decode nor a place in the recognition pass.
        let mut pending: Vec<(PathBuf, ExtractionRecipe)> = Vec::new();
        for path in paths.iter() {
            if options.cancelled() {
                stopped = true;
                break;
            }
            reporter.stage(path, DocumentStage::Checking, done_units, total_units);

            let extraction_recipe = ExtractionRecipe::for_path(
                path,
                extractors,
                indexing.chunk_size,
                indexing.chunk_overlap,
            );
            if let Some(source) = reusable.as_ref() {
                match idx.reuse_unchanged_file_from(source, path, extractors, &extraction_recipe) {
                    // Reused whole: every unit this path was going to cost is
                    // spent, the recognition one included.
                    Ok(true) => {
                        done_units += units_of(path);
                        reporter.settle(
                            path,
                            DocumentOutcome::Reused,
                            None,
                            None,
                            done_units,
                            total_units,
                        );
                        continue;
                    }
                    Ok(false) => {}
                    // A reuse that could not be decided is not a failure of the
                    // document: it falls through and is read from scratch, and
                    // its real outcome is whatever that reading produces.
                    Err(e) => error!(
                        "[SemanticIndex::build] could not reuse {}: {e:#}",
                        path.display()
                    ),
                }
            }
            pending.push((path.clone(), extraction_recipe));
        }

        // ── Pass 2: every figure in the corpus, before any embedding ────────
        let to_recognize: Vec<&PathBuf> = pending
            .iter()
            .map(|(path, _)| path)
            .filter(|path| reads_figures(path))
            .collect();
        if !stopped && !to_recognize.is_empty() {
            info!(
                "[SemanticIndex::build] recognition pass over {} document(s)",
                to_recognize.len()
            );
            for path in to_recognize.iter() {
                if options.cancelled() {
                    stopped = true;
                    break;
                }
                reporter.stage(path, DocumentStage::ReadingFigures, done_units, total_units);
                // The reading itself is dropped. What is kept is the
                // annotation cache it filled, which pass 3 reads back.
                //
                // A document whose figures could not be read is not settled
                // here: it still goes on to pass 3, where it is read without
                // them, and that reading is its outcome.
                if let Err(e) = Self::extract_content(path, extractors) {
                    error!(
                        "[SemanticIndex::build] could not read {} for recognition: {e:#}",
                        path.display()
                    );
                }
                done_units += 1;
            }
            // No more images this build: the recognizer's weights go now
            // rather than at whatever moment its idle timeout arrives, and
            // the embedding pass has the machine to itself. On the cancelled
            // path too — an abandoned build must not leave them resident.
            extractors.release_image_analyzers();
        }

        // ── Pass 3: extract and embed ───────────────────────────────────────
        //
        // `flush` settles a whole batch at once, because embedding is asked for
        // a batch at a time and a document's outcome is not known until the
        // call it was part of returns.
        let mut queue: Vec<PendingFile> = Vec::new();
        let mut queued_chunks = 0usize;
        let mut embed_failure: Option<anyhow::Error> = None;
        if !stopped {
            for (path, extraction_recipe) in pending.iter() {
                if options.cancelled() {
                    stopped = true;
                    break;
                }
                reporter.stage(path, DocumentStage::Extracting, done_units, total_units);

                let extracted = Self::extract_chunks(
                    path,
                    extractors,
                    indexing.chunk_size,
                    indexing.chunk_overlap,
                );
                done_units += 1;
                let (full_text, chunks, retained) = match extracted {
                    Ok((text, c, retained)) if !c.is_empty() => (text, c, retained),
                    // A document that yielded no chunk is indexed by nothing;
                    // that is a reading, not a failure. Nothing is left to
                    // embed, so its embedding unit is spent with it.
                    Ok(_) => {
                        done_units += 1;
                        reporter.settle(
                            path,
                            DocumentOutcome::Empty,
                            None,
                            Some(0),
                            done_units,
                            total_units,
                        );
                        continue;
                    }
                    Err(e) => {
                        error!("[SemanticIndex::build] skipping {}: {e:#}", path.display());
                        done_units += 1;
                        reporter.settle(
                            path,
                            DocumentOutcome::Failed,
                            Some(&format!("{e:#}")),
                            None,
                            done_units,
                            total_units,
                        );
                        continue;
                    }
                };

                queued_chunks += chunks.len();
                queue.push(PendingFile {
                    path: path.clone(),
                    recipe: extraction_recipe.clone(),
                    full_text,
                    chunks,
                    retained,
                });
                reporter.stage(path, DocumentStage::Embedding, done_units, total_units);

                let full = match batch_target {
                    Some(target) => queued_chunks >= target,
                    None => true,
                };
                if full {
                    if options.cancelled() {
                        stopped = true;
                        break;
                    }
                    match Self::flush_batch(
                        &mut idx,
                        embedder,
                        std::mem::take(&mut queue),
                        &options,
                        &mut done_units,
                        total_units,
                    ) {
                        Ok(()) => queued_chunks = 0,
                        Err(BatchStop::Stopped) => {
                            stopped = true;
                            break;
                        }
                        Err(BatchStop::Failed(e)) => {
                            embed_failure = Some(e);
                            break;
                        }
                    }
                }
            }
        }
        if !stopped && embed_failure.is_none() && !queue.is_empty() {
            if options.cancelled() {
                stopped = true;
            } else {
                match Self::flush_batch(
                    &mut idx,
                    embedder,
                    queue,
                    &options,
                    &mut done_units,
                    total_units,
                ) {
                    Ok(()) => {}
                    Err(BatchStop::Stopped) => stopped = true,
                    Err(BatchStop::Failed(e)) => embed_failure = Some(e),
                }
            }
        }

        // A build that did not reach the end of its list cannot speak for the
        // documents it never looked at, and neither can one that was only ever
        // given part of the root.
        let membership =
            if stopped || embed_failure.is_some() || options.scope == BuildScope::Subset {
                RootMembership::Additive
            } else {
                RootMembership::Authoritative
            };

        idx.finish_active_root_build()?;
        idx.validate_embedding_space(&embedding_identity)?;
        let integrity: String = idx
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        anyhow::ensure!(
            integrity == "ok",
            "Temporary semantic index failed integrity check: {integrity}"
        );

        // Publishing nothing is not publishing. A build that finished no
        // document has an empty temporary database, and the replacement path
        // below would rename that emptiness over a working index. There is also
        // nothing to add, so there is nothing to do.
        //
        // Nothing to publish is not itself a verdict on the build. It is how a
        // cancel before the first document looks, and equally how a retry looks
        // when every document failed again — and those must not be reported as
        // each other.
        if membership == RootMembership::Additive
            && idx.status_for_root(Some(root_path)).indexed_files == 0
        {
            drop(idx);
            let _ = std::fs::remove_file(&tmp_path);
            let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
            let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
            if let Some(e) = embed_failure {
                return Err(e);
            }
            anyhow::ensure!(!stopped, "Index build cancelled");
            // The build ran to the end of its list and added nothing — every
            // document in scope failed, or yielded no text. The index it was
            // adding to, unchanged, is the answer; the journal is what says
            // what became of the documents.
            reporter.finished(total_units);
            let mut live = Self::open_exact(data_dir, &embedding_identity)?;
            live.activate_root(root_path)?;
            live.record_build_completion(start_time.elapsed())?;
            return Ok(live);
        }

        if !stopped && embed_failure.is_none() {
            reporter.finished(total_units);
        }

        if let Some(source) = reusable.as_ref() {
            source
                .conn
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        drop(reusable);

        let mut live = match Self::open_exact(data_dir, &embedding_identity) {
            Ok(mut live) => {
                live.merge_root_from(&idx, root_path, membership)?;
                drop(idx);
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
                let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
                live
            }
            Err(err) => {
                tracing::info!(
                    "SemanticIndex::build: replacing index because existing DB could not be reused: {err:#}"
                );
                drop(idx);

                let backup_path = replacement_backup_path(data_dir);
                anyhow::ensure!(
                    !backup_path.exists(),
                    "Cannot replace index while recovery backup exists at {}",
                    backup_path.display()
                );
                remove_sqlite_sidecars(&final_path);
                if final_path.exists() {
                    std::fs::rename(&final_path, &backup_path).with_context(|| {
                        format!(
                            "Failed to preserve existing index at {}",
                            backup_path.display()
                        )
                    })?;
                }
                if let Err(rename_error) = std::fs::rename(&tmp_path, &final_path) {
                    if backup_path.exists() {
                        let _ = std::fs::rename(&backup_path, &final_path);
                    }
                    return Err(rename_error).with_context(|| {
                        format!(
                            "Failed to publish replacement index from {}",
                            tmp_path.display()
                        )
                    });
                }
                Self::open_exact(data_dir, &embedding_identity)?
            }
        };
        live.activate_root(root_path)?;

        // The work is saved either way; what is not claimed either way is that
        // the corpus was built. `built_at` says a build ran to the end of its
        // list, and a stopped one did not.
        if let Some(e) = embed_failure {
            return Err(e);
        }
        anyhow::ensure!(!stopped, "Index build cancelled");

        live.record_build_completion(start_time.elapsed())?;
        Ok(live)
    }
    /// Record that a build finished now, and how long it took.
    ///
    /// Written to whichever database ends up live, at the end, because that is
    /// the only point at which both facts are true of it. Creating a file is
    /// not building an index and filling a temporary one is not either: a
    /// build that merges into an existing database drops the temporary it
    /// filled, so a stamp made at creation left the live database reporting
    /// the age of the file while its contents were rebuilt underneath. The
    /// elapsed time is taken here rather than before the merge so it covers
    /// the whole build the user waited through.
    pub(super) fn record_build_completion(
        &self,
        elapsed: std::time::Duration,
    ) -> anyhow::Result<()> {
        let finished_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?1)",
            params![finished_at.to_string()],
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('build_duration_ms', ?1)",
            params![(elapsed.as_millis() as u64).to_string()],
        )?;
        Ok(())
    }
    /// Extract a file's canonical content without chunking or embedding it.
    pub(super) fn extract_content(
        path: &Path,
        extractors: &ExtractorRegistry,
    ) -> anyhow::Result<crate::types::ExtractedContent> {
        let content = match extractors.find(path, None) {
            Some(ext) => ext.extract(path)?,
            None => {
                // Plain-text fallback: read raw bytes.
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                crate::types::ExtractedContent {
                    text,
                    source_map: crate::types::SourceMap {
                        segments: Vec::new(),
                    },
                    metadata: crate::types::FileMetadata {
                        path: path.to_path_buf(),
                        size_bytes: 0,
                        mime: None,
                        title: None,
                        page_count: None,
                    },
                    images: Vec::new(),
                }
            }
        };
        Ok(content)
    }
    /// Extract and chunk a file without embedding, returning both the document's
    /// full extracted text and its chunks. The text is stored verbatim so exact
    /// (grep) search can scan it without re-extracting the file.
    pub fn extract_chunks(
        path: &Path,
        extractors: &ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<(String, Vec<Chunk>, RetainedExtraction)> {
        let content = Self::extract_content(path, extractors)?;
        let chunks = chunk_content(&content, path.to_path_buf(), chunk_size, chunk_overlap);
        ensure_chunks_reconstruct(
            &content.text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .with_context(|| {
            format!(
                "DOCUMENT_INDEX_INCOMPLETE: chunks of {} do not rebuild its extracted content",
                path.display()
            )
        })?;
        let retained = RetainedExtraction::of(&content);
        Ok((content.text, chunks, retained))
    }
    /// Extract, chunk, and embed a file without holding the index lock.
    pub fn prepare_file(
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<PreparedFile> {
        let mut prepared =
            Self::prepare_file_without_vectors(path, extractors, chunk_size, chunk_overlap)?;
        if prepared.chunks.is_empty() {
            return Ok(prepared);
        }

        let texts: Vec<&str> = prepared
            .chunks
            .iter()
            .map(|(chunk, _)| chunk.text.as_str())
            .collect();
        let embeddings = embedder.embed_passages(&texts)?;

        anyhow::ensure!(
            embeddings.len() == prepared.chunks.len(),
            "Embedder returned {} embeddings for {} chunks",
            embeddings.len(),
            prepared.chunks.len()
        );

        for ((_, vector), embedding) in prepared.chunks.iter_mut().zip(embeddings) {
            *vector = embedding;
        }
        Ok(prepared)
    }
    /// Extract and chunk a file without embedding it.
    ///
    /// The canonical managed corpus owns retained sources, extraction,
    /// chunking and stable passage identity — and, since the projections
    /// landed, no coordinate system. Its chunks are what every space projects
    /// from and what every `chunk_ref` is derived from; its vectors were read
    /// by nothing once a store served a projection, and were computed on every
    /// import anyway. A corpus created under one model went on loading that
    /// model forever, because the canonical embedder is fixed at creation.
    ///
    /// So the canonical import stops asking for one. The chunks come back with
    /// empty vectors and [`Self::write_file_with_recipe`] stores no `vec_chunks`
    /// row for them, which is what "the canonical corpus holds no vectors"
    /// means in the schema.
    pub fn prepare_file_without_vectors(
        path: &Path,
        extractors: &ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<PreparedFile> {
        let (full_text, raw_chunks, retained) =
            Self::extract_chunks(path, extractors, chunk_size, chunk_overlap)?;
        Ok(PreparedFile {
            path: path.to_path_buf(),
            chunks: raw_chunks
                .into_iter()
                .map(|chunk| (chunk, Vec::new()))
                .collect(),
            full_text,
            retained,
        })
    }
    /// Copy an unchanged file's chunks and embeddings from another compatible
    /// index. Returns `false` when reuse is unsafe so the caller can embed it.
    pub(super) fn reuse_unchanged_file_from(
        &mut self,
        source: &SemanticIndex,
        path: &Path,
        extractors: &ExtractorRegistry,
        recipe: &ExtractionRecipe,
    ) -> anyhow::Result<bool> {
        if self.embedding_space_id()? != source.embedding_space_id()? {
            return Ok(false);
        }
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();
        let identity = Self::identity_for_path(path)?;
        let source_sha256 = sha256_file(path)?;
        let source_file = source
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms, full_text,
                        source_sha256, extraction_recipe_id, rendition_id,
                        extracted_content_sha256
                 FROM files
                 WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            source_file_id,
            size_bytes,
            modified_at_ms,
            full_text,
            stored_source_sha256,
            stored_recipe_id,
            stored_rendition_id,
            stored_extracted_content_sha256,
        )) = source_file
        else {
            return Ok(false);
        };
        if size_bytes != identity.size_bytes
            || modified_at_ms != identity.modified_at_ms
            || stored_source_sha256.as_deref() != Some(source_sha256.as_str())
            || stored_recipe_id.as_deref() != Some(recipe.id().as_str())
        {
            return Ok(false);
        }

        let Some(stored_rendition_id) = stored_rendition_id else {
            return Ok(false);
        };
        let recipe_id = recipe.id();
        let Some(chunks) = source.verified_rendition_chunks(
            source_file_id,
            &source_sha256,
            &recipe_id,
            &stored_rendition_id,
            path,
        )?
        else {
            return Ok(false);
        };

        // Pre-v4 rows have no stored full text. Some builds made those rows look
        // populated by coercing NULL to an empty string even though they still
        // had chunks. Extract only the missing text while retaining the valid
        // chunks and embeddings copied above.
        let full_text = match full_text {
            Some(text) if !text.is_empty() => text,
            _ => Self::extract_content(path, extractors)?.text,
        };
        if stored_extracted_content_sha256.as_deref()
            != Some(sha256_bytes(full_text.as_bytes()).as_str())
        {
            return Ok(false);
        }

        self.write_file_with_recipe(
            PreparedFile {
                path: path.to_path_buf(),
                chunks,
                full_text,
                // Copied like the chunks and for the same reason: the source
                // index read this very rendition, and re-deriving them here
                // would mean re-extracting the file the reuse exists to avoid.
                retained: source.retained_extraction_for_file(source_file_id)?,
            },
            recipe,
            None,
            None,
            false,
            false,
            None,
        )?;
        Ok(true)
    }
    /// One file's chunks and vectors as this index holds them, re-checked
    /// against what was recorded when they were written: each chunk's text
    /// against its stored hash, each vector for this index's width and for
    /// finite components, and the whole against the rendition id those chunks
    /// recompute to under `recipe_id`. `None` when any of it disagrees.
    ///
    /// The one reader both kinds of whole-file reuse go through — a build
    /// copying an unchanged file from the index it replaces, and a managed
    /// import adopting a rendition another workspace already read. The chunks
    /// come back addressed to `target_path`, which is where they are about to
    /// be written.
    pub(super) fn verified_rendition_chunks(
        &self,
        file_id: i64,
        source_sha256: &str,
        recipe_id: &str,
        stored_rendition_id: &str,
        target_path: &Path,
    ) -> anyhow::Result<Option<EmbeddedChunks>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_idx, c.byte_start, c.byte_end, c.origin_type,
                    c.page, c.line, c.col, c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h,
                    c.chunk_text, c.text_sha256, v.embedding
             FROM chunks c JOIN vec_chunks v ON v.rowid = c.id
             WHERE c.file_id = ?1 ORDER BY c.chunk_idx",
        )?;
        let rows = stmt
            .query_map(params![file_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                    row.get::<_, Option<f64>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Vec<u8>>(13)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut chunks = Vec::with_capacity(rows.len());
        let mut descriptors = Vec::with_capacity(rows.len());
        for (
            ordinal,
            byte_start,
            byte_end,
            origin_type,
            page,
            line,
            col,
            bbox_x,
            bbox_y,
            bbox_w,
            bbox_h,
            text,
            stored_text_sha256,
            embedding_blob,
        ) in rows
        {
            let Some(origin) = source_origin_from_parts(
                &origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
            ) else {
                return Ok(None);
            };
            let text_sha256 = sha256_bytes(text.as_bytes());
            if stored_text_sha256.as_deref() != Some(text_sha256.as_str()) {
                return Ok(None);
            }
            let embedding = f32_slice_from_bytes(&embedding_blob)?;
            if embedding.len() != self.dimension || embedding.iter().any(|value| !value.is_finite())
            {
                return Ok(None);
            }
            let byte_range = ByteRange {
                start: byte_start as usize,
                end: byte_end as usize,
            };
            descriptors.push(ChunkDescriptor {
                ordinal: ordinal as usize,
                text_sha256,
                byte_range: byte_range.clone(),
                origin: origin.clone(),
            });
            chunks.push((
                Chunk {
                    file_path: target_path.to_path_buf(),
                    text,
                    byte_range,
                    origin,
                },
                embedding,
            ));
        }
        let recomputed = rendition_id(&snapshot_id(source_sha256), recipe_id, &descriptors);
        if recomputed.as_str() != stored_rendition_id {
            return Ok(None);
        }
        Ok(Some(chunks))
    }
    /// Fill `full_text` for rows that carry chunks but no stored text — legacy
    /// rows indexed before schema v4 that have neither changed on disk nor been
    /// rebuilt since, so no existing path (fresh build, build-time reuse, or the
    /// incremental watcher) has ever backfilled them. Until filled, every
    /// reader of a rendition's text (summaries, completion context, superseded
    /// areas, the document text index's copy) finds nothing for them.
    ///
    /// **Reads with [`native_text_registry`](crate::extract::native_text_registry),
    /// and takes no registry so that no caller can hand it another one.** This
    /// runs on every index load, in the background, over every stale row at
    /// once, with nothing on screen: it is the last place in the application
    /// that should be starting recognition workers, and it was doing so once
    /// per legacy PDF. What it stores is therefore the page's own glyphs —
    /// the same reading those readers fall back to for these rows today.
    /// Filling them changes what a read costs, not what it finds.
    ///
    /// Only text is written; chunks and embeddings are left untouched. Extraction
    /// runs without holding the index lock — only the initial read of the stale
    /// set and each per-file write briefly take it — so concurrent search stays
    /// responsive. A row whose file is gone or whose on-disk identity no longer
    /// matches is skipped so stale text is never stored, and the write is guarded
    /// on that identity so a concurrent incremental refresh always wins. Returns
    /// the number of rows filled.
    pub fn backfill_missing_full_text(
        index: &Arc<std::sync::Mutex<Option<SemanticIndex>>>,
    ) -> usize {
        // Phase 1: snapshot the stale set under a brief lock, then release it so
        // extraction never blocks search.
        let stale: Vec<(i64, PathBuf, i64, i64)> = {
            let guard = match index.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(idx) = guard.as_ref() else {
                return 0;
            };
            let mut stmt = match idx.conn.prepare(
                "SELECT f.id, f.file_path, f.size_bytes, f.modified_at_ms
                   FROM files f
                  WHERE (f.full_text IS NULL OR f.full_text = '')
                    AND EXISTS (SELECT 1 FROM chunks c WHERE c.file_id = f.id)",
            ) {
                Ok(stmt) => stmt,
                Err(e) => {
                    error!("[SemanticIndex::backfill] prepare stale query: {e:#}");
                    return 0;
                }
            };
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            });
            match rows.and_then(|r| r.collect::<Result<Vec<_>, _>>()) {
                Ok(rows) => rows,
                Err(e) => {
                    error!("[SemanticIndex::backfill] read stale rows: {e:#}");
                    return 0;
                }
            }
        };
        if stale.is_empty() {
            return 0;
        }

        // Built once for the whole pass, and built here rather than accepted
        // as an argument: see this function's own doc comment.
        let extractors = crate::extract::native_text_registry();
        let mut filled = 0usize;
        for (file_id, path, size_bytes, modified_at_ms) in stale {
            // Phase 2: extract with no lock held. Skip files that vanished or
            // changed since indexing so we never persist text that no longer
            // matches what is on disk.
            let Some(identity) = FileIdentity::for_path(&path) else {
                continue;
            };
            if identity.size_bytes != size_bytes || identity.modified_at_ms != modified_at_ms {
                continue;
            }
            let text = match Self::extract_content(&path, &extractors) {
                Ok(content) => content.text,
                Err(e) => {
                    error!(
                        "[SemanticIndex::backfill] extract {}: {e:#}",
                        path.display()
                    );
                    continue;
                }
            };
            // A chunked row cannot legitimately extract to empty text; empty now
            // means the file changed underneath us, so skip rather than store an
            // empty string that would later read as "populated".
            if text.is_empty() {
                continue;
            }

            // Phase 3: write under a brief lock, guarded on the identity we read.
            // The guard makes a concurrent incremental refresh authoritative: if
            // it already rewrote the row (new identity + text), this update finds
            // no matching row and changes nothing.
            let guard = match index.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(idx) = guard.as_ref() else {
                break;
            };
            match idx.conn.execute(
                "UPDATE files
                    SET full_text = ?1
                  WHERE id = ?2
                    AND size_bytes = ?3
                    AND modified_at_ms = ?4
                    AND (full_text IS NULL OR full_text = '')",
                params![text, file_id, size_bytes, modified_at_ms],
            ) {
                Ok(changed) => filled += changed,
                Err(e) => error!("[SemanticIndex::backfill] write {}: {e:#}", path.display()),
            }
        }
        filled
    }
}
