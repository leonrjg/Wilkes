//! Topic trees over chunks: clustering, caching and labelling.

use super::*;

pub(super) struct TopicTreeCache {
    pub(super) root: PathBuf,
    pub(super) path: Option<PathBuf>,
    pub(super) requested_input_cap: usize,
    pub(super) index_revision: u64,
    pub(super) tree: WardTree,
    pub(super) sampled: Vec<TopicChunkData>,
    /// Retained only for a document-scoped tree. Root trees deliberately drop
    /// their input vectors after Ward construction because their O(n^2)
    /// similarity matrix is already the dominant cache allocation.
    pub(super) sampled_embeddings: Option<Vec<Vec<f32>>>,
    pub(super) total_chunk_count: usize,
    pub(super) total_document_count: usize,
    pub(super) sampled_document_count: usize,
    pub(super) input_cap: usize,
}

impl TopicTreeCache {
    pub(super) fn matches(
        &self,
        root: &Path,
        path: Option<&Path>,
        requested_input_cap: usize,
        index_revision: u64,
    ) -> bool {
        self.root == root
            && self.path.as_deref() == path
            && self.requested_input_cap == requested_input_cap
            && self.index_revision == index_revision
    }
}

#[derive(Default)]
pub(super) struct TopicTreeCaches {
    pub(super) root: Option<Arc<TopicTreeCache>>,
    pub(super) document: Option<Arc<TopicTreeCache>>,
}

impl TopicTreeCaches {
    pub(super) fn slot(&self, path: Option<&Path>) -> &Option<Arc<TopicTreeCache>> {
        if path.is_some() {
            &self.document
        } else {
            &self.root
        }
    }

    pub(super) fn set(&mut self, path: Option<&Path>, cache: Arc<TopicTreeCache>) {
        if path.is_some() {
            self.document = Some(cache);
        } else {
            self.root = Some(cache);
        }
    }
}

pub(super) struct TopicOperation {
    pub(super) cancel: CancellationToken,
    pub(super) cancel_flag: Arc<AtomicBool>,
    pub(super) label_task: Option<JoinHandle<()>>,
}

impl AppContext {
    pub(super) fn invalidate_topic_tree_cache(&self) {
        self.semantic_index_revision.fetch_add(1, Ordering::AcqRel);
        *self.topic_tree_caches.lock() = TopicTreeCaches::default();
    }
    pub(super) fn matching_topic_tree_cache(
        &self,
        root: &Path,
        path: Option<&Path>,
        requested_input_cap: usize,
        index_revision: u64,
    ) -> Option<Arc<TopicTreeCache>> {
        self.topic_tree_caches
            .lock()
            .slot(path)
            .as_ref()
            .filter(|cached| cached.matches(root, path, requested_input_cap, index_revision))
            .cloned()
    }
    pub(super) fn start_topic_operation(
        &self,
        request_id: &str,
    ) -> Result<(CancellationToken, Arc<AtomicBool>), String> {
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err("Invalid chunk-topic request id".to_string());
        }
        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let previous = self.topic_operations.lock().insert(
            request_id.to_string(),
            TopicOperation {
                cancel: cancel.clone(),
                cancel_flag: Arc::clone(&cancel_flag),
                label_task: None,
            },
        );
        if let Some(previous) = previous {
            previous.cancel_flag.store(true, Ordering::Relaxed);
            previous.cancel.cancel();
            if let Some(task) = previous.label_task {
                task.abort();
            }
        }
        Ok((cancel, cancel_flag))
    }
    pub(super) fn finish_topic_operation(&self, request_id: &str) {
        self.topic_operations.lock().remove(request_id);
    }
    pub fn cancel_chunk_topics(&self, request_id: &str) {
        if let Some(operation) = self.topic_operations.lock().remove(request_id) {
            operation.cancel_flag.store(true, Ordering::Relaxed);
            operation.cancel.cancel();
            if let Some(task) = operation.label_task {
                task.abort();
            }
        }
    }
    pub(super) async fn topic_tree_for(
        self: &Arc<Self>,
        root: PathBuf,
        path: Option<PathBuf>,
        requested_input_cap: usize,
        cancel: &CancellationToken,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<Arc<TopicTreeCache>, String> {
        loop {
            if cancel.is_cancelled() {
                return Err("Chunk topic operation cancelled".to_string());
            }
            let index_revision = self.semantic_index_revision.load(Ordering::Acquire);
            if let Some(cached) = self.matching_topic_tree_cache(
                &root,
                path.as_deref(),
                requested_input_cap,
                index_revision,
            ) {
                return Ok(cached);
            }

            let _build_guard = tokio::select! {
                guard = self.topic_tree_build_lock.lock() => guard,
                _ = cancel.cancelled() => {
                    return Err("Chunk topic operation cancelled".to_string());
                }
            };
            let index_revision = self.semantic_index_revision.load(Ordering::Acquire);
            if let Some(cached) = self.matching_topic_tree_cache(
                &root,
                path.as_deref(),
                requested_input_cap,
                index_revision,
            ) {
                return Ok(cached);
            }

            let index_arc = self.index.lock().clone();
            let build_root = root.clone();
            let build_path = path.clone();
            let build_cancel = Arc::clone(cancel_flag);
            let built = tokio::task::spawn_blocking(move || {
                // Release the database mutex before Ward starts. The bulk read
                // is the only part that needs the live semantic index.
                let all = {
                    let guard = index_arc
                        .lock()
                        .map_err(|_| "Semantic index lock was poisoned".to_string())?;
                    let index = guard.as_ref().ok_or_else(|| {
                        "Semantic index unavailable. Build or restore the semantic index first."
                            .to_string()
                    })?;
                    match build_path.as_deref() {
                        Some(path) => index.topic_chunks_for_file(&build_root, path),
                        None => index.topic_chunks_for_root(&build_root),
                    }
                    .map_err(|error| format!("Could not load indexed chunks: {error:#}"))?
                };

                if build_cancel.load(Ordering::Relaxed) {
                    return Err("Chunk topic operation cancelled".to_string());
                }

                let total_chunk_count = all.len();
                let input_cap = requested_input_cap.min(total_chunk_count);
                let total_document_count = all
                    .iter()
                    .map(|chunk| chunk.file_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let mut sampled = cap_topic_chunks(all, input_cap);
                let sampled_document_count = sampled
                    .iter()
                    .map(|chunk| chunk.file_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len();

                // Root caches need passage metadata, not another copy of every
                // vector, so they retain only the Ward similarity matrix.
                // Document caches keep their bounded vectors as the prototypes
                // needed to project local topics across the full library.
                let vectors: Vec<Vec<f32>> = sampled
                    .iter_mut()
                    .map(|chunk| std::mem::take(&mut chunk.embedding))
                    .collect();
                let tree = WardTree::build_with_cancel(&vectors, &build_cancel)
                    .map_err(|error| format!("Could not cluster indexed chunks: {error:#}"))?;
                let sampled_embeddings = build_path.is_some().then_some(vectors);

                Ok::<_, String>(Arc::new(TopicTreeCache {
                    root: build_root,
                    path: build_path,
                    requested_input_cap,
                    index_revision,
                    tree,
                    sampled,
                    sampled_embeddings,
                    total_chunk_count,
                    total_document_count,
                    sampled_document_count,
                    input_cap,
                }))
            })
            .await
            .map_err(|error| format!("Chunk topic task panicked: {error}"))??;

            if cancel.is_cancelled() {
                return Err("Chunk topic operation cancelled".to_string());
            }
            let mut caches = self.topic_tree_caches.lock();
            if self.semantic_index_revision.load(Ordering::Acquire) == index_revision {
                caches.set(path.as_deref(), Arc::clone(&built));
                return Ok(built);
            }
            // The index changed while Ward was building. Discard the stale
            // tree and retry against the new revision before serving results.
        }
    }
    pub async fn chunk_topics(
        self: Arc<Self>,
        request_id: String,
        query: ChunkTopicsQuery,
    ) -> Result<ChunkTopicsResult, String> {
        let (cancel, cancel_flag) = self.start_topic_operation(&request_id)?;
        let result = Arc::clone(&self)
            .chunk_topics_inner(&request_id, query, &cancel, &cancel_flag)
            .await;
        if result.is_err() {
            self.finish_topic_operation(&request_id);
        }
        result
    }
    pub(super) async fn chunk_topics_inner(
        self: Arc<Self>,
        request_id: &str,
        query: ChunkTopicsQuery,
        cancel: &CancellationToken,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<ChunkTopicsResult, String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before finding topics.",
        )?;
        if query.root.as_os_str().is_empty() {
            return Err("Choose a library root before finding topics.".to_string());
        }

        let settings = self.settings().await;
        let (library_roots, _) = library_roots(&settings);
        let root = Self::canonicalize_search_root(&query.root)?;
        Self::ensure_path_in_library(&root, &library_roots, "Chunk topics root")?;
        let path = match query.path {
            Some(path) => {
                let (path, _) = Self::canonicalize_supported_file(
                    &root,
                    &path,
                    &settings.supported_extensions,
                    "Chunk topics",
                )?;
                Self::ensure_path_in_library(&path, &library_roots, "Chunk topics file")?;
                Some(path)
            }
            None => None,
        };
        let requested_input_cap = settings.semantic.topic_cloud_input_cap.max(3);
        let granularity = query.granularity;
        let cached = self
            .topic_tree_for(root, path, requested_input_cap, cancel, cancel_flag)
            .await?;
        let cached_for_cut = Arc::clone(&cached);
        let cut_cancel = Arc::clone(cancel_flag);
        let (mut result, coverage_prototypes) = tokio::task::spawn_blocking(move || {
            let clustered = cached_for_cut
                .tree
                .cut_with_cancel(granularity, &cut_cancel)
                .map_err(|error| format!("Could not cut chunk topic tree: {error:#}"))?;
            let document_scoped = cached_for_cut.path.is_some();
            let document_embeddings =
                match (document_scoped, cached_for_cut.sampled_embeddings.as_ref()) {
                    (true, Some(embeddings)) => Some(embeddings),
                    (true, None) => {
                        return Err(
                            "Document topic cache is missing retained embeddings".to_string()
                        )
                    }
                    (false, _) => None,
                };
            let mut topics = Vec::with_capacity(clustered.clusters.len());
            let mut coverage_prototypes = Vec::with_capacity(clustered.clusters.len());
            for cluster in clustered.clusters {
                if let Some(embeddings) = document_embeddings {
                    coverage_prototypes.push(TopicCoveragePrototype {
                        mean_member_embedding: mean_normalized_embeddings(
                            embeddings,
                            &cluster.item_indices,
                        )?,
                        cohesion: cluster.cohesion,
                    });
                }
                let chunks: Vec<ChunkTopicMember> = cluster
                    .item_indices
                    .iter()
                    .map(|index| topic_member(&cached_for_cut.sampled[*index]))
                    .collect();
                let distinct_document_count = chunks
                    .iter()
                    .map(|chunk| &chunk.file_path)
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                topics.push(ChunkTopic {
                    cluster_key: chunk_cluster_key(chunks.iter().map(|chunk| chunk.chunk_id)),
                    chunk_count: chunks.len(),
                    distinct_document_count,
                    chunks,
                    representative_chunk_id: cached_for_cut.sampled[cluster.representative_index]
                        .chunk_id,
                    cohesion: cluster.cohesion,
                    library_coverage: None,
                    label: None,
                });
            }
            Ok::<_, String>((
                ChunkTopicsResult {
                    topics,
                    total_chunk_count: cached_for_cut.total_chunk_count,
                    sampled_chunk_count: cached_for_cut.sampled.len(),
                    total_document_count: cached_for_cut.total_document_count,
                    sampled_document_count: cached_for_cut.sampled_document_count,
                    input_cap: cached_for_cut.input_cap,
                },
                coverage_prototypes,
            ))
        })
        .await
        .map_err(|e| format!("Chunk topic task panicked: {e}"))??;

        if let Some(source_path) = cached.path.clone() {
            if self.semantic_index_revision.load(Ordering::Acquire) != cached.index_revision {
                return Err("Semantic index changed while calculating topic coverage".to_string());
            }
            let index_arc = self.index.lock().clone();
            let coverage_roots = library_roots;
            let coverage_cancel = Arc::clone(cancel_flag);
            let coverage = tokio::task::spawn_blocking(move || {
                let guard = index_arc
                    .lock()
                    .map_err(|_| "Semantic index lock was poisoned".to_string())?;
                let index = guard.as_ref().ok_or_else(|| {
                    "Semantic index unavailable. Build or restore the semantic index first."
                        .to_string()
                })?;
                index
                    .topic_library_coverage(
                        &coverage_roots,
                        &source_path,
                        &coverage_prototypes,
                        &coverage_cancel,
                    )
                    .map_err(|error| format!("Could not calculate topic coverage: {error:#}"))
            })
            .await
            .map_err(|error| format!("Topic coverage task panicked: {error}"))??;
            if self.semantic_index_revision.load(Ordering::Acquire) != cached.index_revision {
                return Err("Semantic index changed while calculating topic coverage".to_string());
            }
            if coverage.related_document_counts.len() != result.topics.len()
                || coverage.related_chunks.len() != result.topics.len()
            {
                return Err("Topic coverage result count does not match topic count".to_string());
            }
            for ((topic, related_document_count), chunks) in result
                .topics
                .iter_mut()
                .zip(coverage.related_document_counts)
                .zip(coverage.related_chunks)
            {
                topic.library_coverage = Some(TopicLibraryCoverage {
                    related_document_count,
                    eligible_document_count: coverage.eligible_document_count,
                    chunks,
                });
            }
        }

        self.attach_chunk_topic_labels(
            request_id,
            Arc::clone(cancel_flag),
            &mut result.topics,
            &cached.sampled,
        )
        .await;
        Ok(result)
    }
    /// Fill cached topic labels immediately and generate only the misses in the
    /// background. Late labels are patched by the membership-derived key.
    ///
    /// Reading the cache and generating the misses are separate acts with
    /// separate preconditions. A label already in the store was paid for by an
    /// earlier run and costs nothing to show, so it is read whenever this
    /// workspace names a model — including while generation is switched off.
    /// Only the misses need consent and a loaded generator, and only they are
    /// skipped when there is none. Gating the read on the generator too is what
    /// left every topic permanently nameless the moment generation was
    /// disabled, with the labels for those very clusters sitting in the store.
    pub(super) async fn attach_chunk_topic_labels(
        self: &Arc<Self>,
        request_id: &str,
        cancel_flag: Arc<AtomicBool>,
        topics: &mut [ChunkTopic],
        sampled: &[TopicChunkData],
    ) {
        if topics.is_empty() {
            self.finish_topic_operation(request_id);
            return;
        }
        let settings = self.generation_settings().await;
        // Whose labels this workspace is asking for: a question about the
        // settings, answerable with no generator loaded. Distinct from
        // `Generator::model_id` below, which names the model that actually
        // produced a label and so is what a new label is written under.
        let Some(configured_model_id) = settings
            .model
            .as_ref()
            .map(|model| model.model_id().to_string())
        else {
            self.finish_topic_operation(request_id);
            return;
        };
        let keys: Vec<String> = topics
            .iter()
            .map(|topic| topic.cluster_key.clone())
            .collect();
        let cached = match self.research_store() {
            Ok(store) => store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .cached_chunk_cluster_labels(
                    &keys,
                    &configured_model_id,
                    CHUNK_CLUSTER_LABEL_RECIPE_VERSION,
                ),
            Err(error) => Err(error),
        };
        let cached = match cached {
            Ok(cached) => cached,
            Err(error) => {
                error!("Could not read cached chunk-cluster labels: {error:#}");
                self.finish_topic_operation(request_id);
                return;
            }
        };

        let text_by_id: std::collections::HashMap<i64, &str> = sampled
            .iter()
            .map(|chunk| (chunk.chunk_id, chunk.chunk_text.as_str()))
            .collect();
        let mut pending = Vec::new();
        for topic in topics.iter_mut() {
            let members = chunk_label_inputs(topic, &text_by_id);
            if let Some(label) = cached.get(&topic.cluster_key) {
                let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                if validate_cluster_label(label, &refs).is_ok() {
                    topic.label = Some(label.clone());
                    continue;
                }
                warn!(
                    "Ignoring invalid cached chunk-topic label for {}",
                    topic.cluster_key
                );
            }
            pending.push((topic.cluster_key.clone(), members));
        }
        if pending.is_empty() {
            self.finish_topic_operation(request_id);
            return;
        }
        // Everything below runs a model, so it needs the user's consent and a
        // loaded generator. Without either, the misses stay unnamed and the
        // cache hits above are still attached — which is the whole point of
        // reading the store before asking this question.
        let generator = self.generator.lock().clone();
        let generator = match generator {
            Some(generator) if settings.enabled => generator,
            _ => {
                self.finish_topic_operation(request_id);
                return;
            }
        };
        let model_id = generator.model_id().to_string();
        let ctx = Arc::clone(self);
        let task_request_id = request_id.to_string();
        let task = tokio::spawn(async move {
            for (key, members) in pending {
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                let generator = Arc::clone(&generator);
                let generation_cancel = Arc::clone(&cancel_flag);
                let generated = tokio::task::spawn_blocking(move || {
                    let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                    cluster_label_stream(generator.as_ref(), &refs, &mut |_| {
                        if generation_cancel.load(Ordering::Relaxed) {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    })
                })
                .await;
                let label = match generated {
                    Ok(Ok(label)) => label,
                    Ok(Err(error)) => {
                        // The worker being gone ends the loop on its own,
                        // whether or not anyone raised the flag: the next
                        // iteration would start a replacement.
                        if wilkes_core::worker::fault::is_worker_gone(&error) {
                            info!(
                                "Stopping chunk-topic labelling: the generator is gone ({error:#})"
                            );
                            break;
                        }
                        if cancel_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        warn!("Could not label chunk topic {key}: {error:#}");
                        continue;
                    }
                    Err(error) => {
                        warn!("Chunk-topic label task failed for {key}: {error}");
                        continue;
                    }
                };
                if cancel_flag.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(store) = ctx.research_store() {
                    if let Err(error) = store
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .upsert_chunk_cluster_label(
                            &key,
                            &label,
                            &model_id,
                            CHUNK_CLUSTER_LABEL_RECIPE_VERSION,
                        )
                    {
                        error!("Could not cache chunk-topic label: {error:#}");
                    }
                }
                ctx.events.emit(
                    "chunk-topic-labelled",
                    serde_json::json!({
                        "request_id": task_request_id.clone(),
                        "cluster_key": key,
                        "label": label
                    }),
                );
            }
            ctx.finish_topic_operation(&task_request_id);
        });
        let mut operations = self.topic_operations.lock();
        if let Some(operation) = operations.get_mut(request_id) {
            operation.label_task = Some(task);
        } else {
            task.abort();
        }
    }
}

/// Content-derived cluster identity.
///
/// Same members and same text yields the same key, even across a granularity
/// change that happens to reproduce the group; any edit, addition or removal
/// yields a different one. That is exactly the invalidation wanted, with no
/// invalidation code — and it is why the key uses each member's `input_hash`
/// rather than its `updated_at`, which changes on edits that do not affect the
/// clustered text.
pub(super) fn cluster_key(members: &[(String, String)]) -> String {
    let mut parts: Vec<String> = members
        .iter()
        .map(|(id, input_hash)| format!("{id}:{input_hash}"))
        .collect();
    parts.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"v1\n");
    hasher.update(parts.join("\n").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Distribute a hard chunk budget across documents before Ward sees the input.
///
/// The round-robin quota makes every document contribute one chunk whenever
/// that is mathematically possible, then fills short documents completely
/// before repeatedly increasing larger ones. Within each document, evenly
/// spaced windows avoid turning the cap into a "document beginnings" sampler.
pub(super) fn cap_topic_chunks(chunks: Vec<TopicChunkData>, cap: usize) -> Vec<TopicChunkData> {
    if cap == 0 {
        return Vec::new();
    }
    if chunks.len() <= cap {
        return chunks;
    }
    let mut by_file: std::collections::BTreeMap<i64, Vec<TopicChunkData>> =
        std::collections::BTreeMap::new();
    for chunk in chunks {
        by_file.entry(chunk.file_id).or_default().push(chunk);
    }
    let groups: Vec<Vec<TopicChunkData>> = by_file.into_values().collect();

    // More documents than slots is the only case where "every document"
    // cannot coexist with a hard ceiling. Choose documents evenly across the
    // stable file ordering rather than silently preferring the first paths.
    if groups.len() > cap {
        return (0..cap)
            .map(|slot| {
                let group_index = slot.saturating_mul(groups.len()) / cap;
                let group = &groups[group_index];
                group[group.len() / 2].clone()
            })
            .collect();
    }

    let mut quotas = vec![1usize; groups.len()];
    let mut assigned = groups.len();
    while assigned < cap {
        let mut progressed = false;
        for (quota, group) in quotas.iter_mut().zip(&groups) {
            if assigned == cap {
                break;
            }
            if *quota < group.len() {
                *quota += 1;
                assigned += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    let mut sampled = Vec::with_capacity(assigned);
    for (group, quota) in groups.into_iter().zip(quotas) {
        for slot in 0..quota {
            // Midpoints of `quota` equal-width ranges, characteristically
            // selecting first/middle/last coverage as the quota grows.
            let index = ((slot.saturating_mul(2) + 1).saturating_mul(group.len()))
                / quota.saturating_mul(2);
            sampled.push(group[index.min(group.len() - 1)].clone());
        }
    }
    sampled
}

pub(super) fn topic_member(chunk: &TopicChunkData) -> ChunkTopicMember {
    ChunkTopicMember {
        chunk_id: chunk.chunk_id,
        file_path: chunk.file_path.clone(),
        chunk_text: chunk.chunk_text.clone(),
        extraction_byte_range: chunk.extraction_byte_range.clone(),
        origin: chunk.origin.clone(),
    }
}

pub(super) fn mean_normalized_embeddings(
    embeddings: &[Vec<f32>],
    indices: &[usize],
) -> Result<Vec<f32>, String> {
    let Some(&first_index) = indices.first() else {
        return Err("Cannot calculate coverage for an empty topic".to_string());
    };
    let dimension = embeddings
        .get(first_index)
        .ok_or_else(|| "Topic member embedding index is out of bounds".to_string())?
        .len();
    if dimension == 0 {
        return Err("Cannot calculate coverage from zero-dimensional embeddings".to_string());
    }
    let mut mean = vec![0.0f32; dimension];
    for &index in indices {
        let embedding = embeddings
            .get(index)
            .ok_or_else(|| "Topic member embedding index is out of bounds".to_string())?;
        if embedding.len() != dimension {
            return Err("Topic member embedding dimensions do not match".to_string());
        }
        if !embedding.iter().all(|value| value.is_finite()) {
            return Err("Topic member embedding contains non-finite values".to_string());
        }
        let norm = embedding
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        if norm > f32::EPSILON {
            for (total, value) in mean.iter_mut().zip(embedding) {
                *total += value / norm;
            }
        }
    }
    let count = indices.len() as f32;
    for value in &mut mean {
        *value /= count;
    }
    Ok(mean)
}

pub(super) fn chunk_cluster_key(chunk_ids: impl IntoIterator<Item = i64>) -> String {
    let mut ids: Vec<i64> = chunk_ids.into_iter().collect();
    ids.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(b"chunk-cluster-v1\n");
    for id in ids {
        hasher.update(id.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub(super) fn chunk_label_inputs(
    topic: &ChunkTopic,
    text_by_id: &std::collections::HashMap<i64, &str>,
) -> Vec<String> {
    let mut remaining: Vec<i64> = topic
        .chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .filter(|id| *id != topic.representative_chunk_id)
        .collect();
    remaining.sort_unstable();
    std::iter::once(topic.representative_chunk_id)
        .chain(remaining)
        .filter_map(|id| text_by_id.get(&id).map(|text| (*text).to_string()))
        .take(wilkes_core::generate::tasks::cluster_label::MAX_MEMBERS)
        .collect()
}

/// Prompt input for one cluster: the representative first, then the remaining
/// members in a deterministic order. Non-deterministic selection would make the
/// cached label depend on iteration order.
pub(super) fn label_inputs(
    cluster: &BookmarkCluster,
    inputs: &std::collections::HashMap<&str, &str>,
) -> Vec<String> {
    let mut ordered: Vec<&String> = cluster
        .bookmark_ids
        .iter()
        .filter(|id| **id != cluster.representative_bookmark_id)
        .collect();
    ordered.sort();

    std::iter::once(&cluster.representative_bookmark_id)
        .chain(ordered)
        .filter_map(|id| inputs.get(id.as_str()).map(|text| text.to_string()))
        .take(wilkes_core::generate::tasks::cluster_label::MAX_MEMBERS)
        .collect()
}
