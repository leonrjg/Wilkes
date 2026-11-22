//! Bookmarks and saved works, and clustering bookmarks into labelled groups.

use super::*;

impl AppContext {
    pub async fn list_bookmarks(&self) -> anyhow::Result<Vec<Bookmark>> {
        let store = self.research_store()?;
        let mut bookmarks = store
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_bookmarks()?;
        let settings = self.get_settings().await;
        self.resolve_bookmark_paths(&mut bookmarks, &settings);
        Ok(bookmarks)
    }
    /// Re-point bookmarks whose stored `path` no longer exists to wherever their
    /// content identity now lives, using the metadata cache as the identity
    /// registry (the same mechanism that re-keys the index and metadata rows on
    /// rename). Resolution is read-only and recomputed each call: nothing is
    /// persisted, so there is no write on this read path.
    ///
    /// Best-effort by design — a bookmark keeps its stored path when it still
    /// exists, has no captured identity, the cache is unavailable, or the
    /// identity is absent/ambiguous in the cache (e.g. the renamed file has not
    /// been re-listed yet). Such a bookmark simply resolves on a later read once
    /// a listing has populated the cache; it never fails to load.
    pub(super) fn resolve_bookmark_paths(&self, bookmarks: &mut [Bookmark], settings: &Settings) {
        self.resolve_moved_paths(
            bookmarks
                .iter_mut()
                .map(|bookmark| (&mut bookmark.path, bookmark.identity)),
            settings,
        );
    }
    /// The re-pointing [`Self::resolve_bookmark_paths`] describes, for any
    /// record that holds a path and the identity captured with it.
    pub(super) fn resolve_moved_paths<'a>(
        &self,
        entries: impl Iterator<Item = (&'a mut PathBuf, Option<FileIdentity>)>,
        settings: &Settings,
    ) {
        let stale: Vec<(&mut PathBuf, FileIdentity)> = entries
            .filter(|(path, _)| !path.exists())
            .filter_map(|(path, identity)| identity.map(|identity| (path, identity)))
            .collect();
        // Avoid opening/locking the cache unless something is actually stale.
        if stale.is_empty() {
            return;
        }
        let (preferred_roots, _) = library_roots(settings);
        let Some(cache) = self.metadata_cache() else {
            return;
        };
        let Ok(guard) = cache.lock() else {
            return;
        };
        for (path, identity) in stale {
            match guard.find_current_path(identity, &preferred_roots) {
                Ok(Some(current)) => {
                    *path = current;
                }
                Ok(None) => {}
                Err(e) => error!("path resolution {}: {e:#}", path.display()),
            }
        }
    }
    pub async fn add_bookmark(&self, bookmark: NewBookmark) -> anyhow::Result<Bookmark> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .add_bookmark(bookmark)
    }
    pub async fn remove_bookmark(&self, id: &str) -> anyhow::Result<()> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_bookmark(id)
    }
    pub async fn update_bookmark_note(
        &self,
        id: &str,
        note: Option<String>,
    ) -> anyhow::Result<Bookmark> {
        let _guard = self.bookmarks_lock.lock().await;
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_bookmark_note(id, note)
    }
    pub async fn list_saved_works(&self) -> anyhow::Result<Vec<SavedWork>> {
        let mut works = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_saved_works()?;
        let settings = self.get_settings().await;
        self.resolve_moved_paths(
            works
                .iter_mut()
                .filter_map(|saved| saved.acquired.as_mut())
                .map(|acquired| (&mut acquired.path, acquired.identity)),
            &settings,
        );
        Ok(works)
    }
    pub fn save_work(&self, new: NewSavedWork) -> anyhow::Result<SavedWork> {
        let saved = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .save_work(new)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"saved_works"}),
        );
        Ok(saved)
    }
    pub fn remove_saved_work(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_saved_work(id)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"saved_works"}),
        );
        Ok(())
    }
    pub fn update_saved_work_note(
        &self,
        id: &str,
        note: Option<String>,
    ) -> anyhow::Result<SavedWork> {
        let saved = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_saved_work_note(id, note)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"saved_works"}),
        );
        Ok(saved)
    }
    /// The one place a saved work is marked as in the library, whoever added
    /// it: the pane once its import has finished, or the MCP download.
    pub fn mark_saved_work_acquired(
        &self,
        provider: &str,
        work: &LiteratureSearchResult,
        path: PathBuf,
    ) -> anyhow::Result<Option<SavedWork>> {
        let saved = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .mark_saved_work_acquired(provider, work, path)?;
        if saved.is_some() {
            self.events.emit(
                "research-state-updated",
                serde_json::json!({"kind":"saved_works"}),
            );
        }
        Ok(saved)
    }
    pub async fn cluster_bookmarks(
        self: Arc<Self>,
        query: BookmarkClustersQuery,
    ) -> Result<BookmarkClustersResult, String> {
        self.ensure_no_active_embed_task(
            "Semantic index is currently being built. Please wait before grouping bookmarks.",
        )?;

        let granularity = query.granularity;
        let mut requested_ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in query.bookmark_ids {
            if seen.insert(id.clone()) {
                requested_ids.push(id);
            }
        }
        if requested_ids.is_empty() {
            return Ok(BookmarkClustersResult::default());
        }

        let bookmarks = self.list_bookmarks().await.map_err(|e| e.to_string())?;
        let mut by_id: std::collections::HashMap<String, Bookmark> = bookmarks
            .into_iter()
            .map(|bookmark| (bookmark.id.clone(), bookmark))
            .collect();
        let mut prepared = Vec::new();
        let mut inherently_unclustered = Vec::new();
        for id in &requested_ids {
            let bookmark = by_id
                .remove(id)
                .ok_or_else(|| format!("Bookmark not found: {id}"))?;
            let input = bookmark_embedding_input(&bookmark);
            if input.trim().is_empty() {
                inherently_unclustered.push(id.clone());
                continue;
            }
            let input_hash = format!("{:x}", Sha256::digest(input.as_bytes()));
            prepared.push((id.clone(), input, input_hash));
        }

        if prepared.len() < 3 {
            return Ok(BookmarkClustersResult {
                clusters: Vec::new(),
                unclustered_bookmark_ids: requested_ids,
            });
        }

        let embedder = self.embedder.lock().clone().ok_or_else(|| {
            "Semantic model unavailable. Build or restore the semantic index first.".to_string()
        })?;
        let engine = embedder.engine().as_str().to_string();
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();

        let cached = self
            .research_store()
            .map_err(|e| e.to_string())?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cached_bookmark_embeddings(
                &engine,
                &model_id,
                dimension,
                BOOKMARK_EMBEDDING_RECIPE_VERSION,
            )
            .map_err(|e| e.to_string())?;
        let cached_by_id: std::collections::HashMap<_, _> = cached
            .into_iter()
            .map(|cached| (cached.bookmark_id.clone(), cached))
            .collect();

        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; prepared.len()];
        let mut misses = Vec::new();
        for (index, (id, input, input_hash)) in prepared.iter().enumerate() {
            match cached_by_id.get(id) {
                Some(cached) if cached.input_hash == *input_hash => {
                    vectors[index] = Some(cached.embedding.clone());
                }
                _ => misses.push((index, id.clone(), input.clone(), input_hash.clone())),
            }
        }

        if !misses.is_empty() {
            let texts: Vec<String> = misses
                .iter()
                .map(|(_, _, input, _)| input.clone())
                .collect();
            let embedder_for_task = Arc::clone(&embedder);
            let embeddings = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                embedder_for_task.embed_passages(&refs)
            })
            .await
            .map_err(|e| format!("Bookmark embedding task panicked: {e}"))?
            .map_err(|e| format!("Could not embed bookmarks: {e:#}"))?;
            if embeddings.len() != misses.len() {
                return Err(format!(
                    "Embedder returned {} bookmark vectors for {} inputs",
                    embeddings.len(),
                    misses.len()
                ));
            }

            let store = self.research_store().map_err(|e| e.to_string())?;
            let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
            for ((index, id, _, input_hash), embedding) in misses.into_iter().zip(embeddings) {
                if embedding.len() != dimension {
                    return Err(format!(
                        "Bookmark embedding dimension mismatch. Expected {dimension}, received {}",
                        embedding.len()
                    ));
                }
                if embedding.iter().any(|value| !value.is_finite()) {
                    return Err("Embedder returned a non-finite bookmark vector".to_string());
                }
                let cached = CachedBookmarkEmbedding {
                    bookmark_id: id,
                    input_hash,
                    embedding: embedding.clone(),
                };
                if let Err(error) = store.upsert_bookmark_embedding(
                    &cached,
                    &engine,
                    &model_id,
                    dimension,
                    BOOKMARK_EMBEDDING_RECIPE_VERSION,
                ) {
                    error!("Could not cache bookmark embedding: {error:#}");
                }
                vectors[index] = Some(embedding);
            }
        }

        let vectors: Vec<Vec<f32>> = vectors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "A bookmark embedding was not produced".to_string())?;
        let clustered = tokio::task::spawn_blocking(move || {
            wilkes_core::embed::cluster::cluster_embeddings(&vectors, granularity)
        })
        .await
        .map_err(|e| format!("Bookmark clustering task panicked: {e}"))?
        .map_err(|e| e.to_string())?;

        let mut clusters: Vec<BookmarkCluster> = clustered
            .clusters
            .into_iter()
            .map(|cluster| {
                let members: Vec<(String, String)> = cluster
                    .item_indices
                    .iter()
                    .map(|index| (prepared[*index].0.clone(), prepared[*index].2.clone()))
                    .collect();
                BookmarkCluster {
                    cluster_key: cluster_key(&members),
                    bookmark_ids: members.iter().map(|(id, _)| id.clone()).collect(),
                    representative_bookmark_id: prepared[cluster.representative_index].0.clone(),
                    cohesion: cluster.cohesion,
                    label: None,
                }
            })
            .collect();

        // Labels are asynchronous: decode is ~370ms each, so labelling inline
        // would add seconds to a call that is otherwise pure compute.
        self.attach_cluster_labels(&mut clusters, &prepared).await;

        let clustered_unassigned = clustered
            .unclustered_indices
            .into_iter()
            .map(|index| prepared[index].0.clone());
        inherently_unclustered.extend(clustered_unassigned);
        let unclustered_set: std::collections::HashSet<_> =
            inherently_unclustered.into_iter().collect();
        let unclustered_bookmark_ids = requested_ids
            .into_iter()
            .filter(|id| unclustered_set.contains(id))
            .collect();

        Ok(BookmarkClustersResult {
            clusters,
            unclustered_bookmark_ids,
        })
    }
    /// Fill in cached labels, then kick off a background run for the misses.
    /// Returns immediately; late labels arrive as `bookmark-cluster-labelled`
    /// events, patched by `cluster_key`.
    pub(super) async fn attach_cluster_labels(
        self: &Arc<Self>,
        clusters: &mut [BookmarkCluster],
        prepared: &[(String, String, String)],
    ) {
        // A newer run supersedes the previous one outright: its results are for
        // a partition that is no longer displayed.
        if let Some(previous) = self.cluster_label_task.lock().take() {
            previous.abort();
        }
        if clusters.is_empty() {
            return;
        }

        let Some(generator) = self.generator.lock().clone() else {
            return;
        };
        let settings = self.generation_settings().await;
        if !settings.enabled {
            return;
        }
        let model_id = generator.model_id().to_string();

        let keys: Vec<String> = clusters.iter().map(|c| c.cluster_key.clone()).collect();
        let cached = match self.research_store() {
            Ok(store) => store
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .cached_cluster_labels(&keys, &model_id, BOOKMARK_CLUSTER_LABEL_RECIPE_VERSION),
            Err(e) => Err(e),
        };
        let cached = match cached {
            Ok(cached) => cached,
            Err(e) => {
                error!("Could not read cached cluster labels: {e:#}");
                return;
            }
        };

        let inputs: std::collections::HashMap<&str, &str> = prepared
            .iter()
            .map(|(id, input, _)| (id.as_str(), input.as_str()))
            .collect();

        let mut pending: Vec<(String, Vec<String>)> = Vec::new();
        for cluster in clusters.iter_mut() {
            let members = label_inputs(cluster, &inputs);
            if let Some(label) = cached.get(&cluster.cluster_key) {
                let refs: Vec<&str> = members.iter().map(String::as_str).collect();
                if validate_cluster_label(label, &refs).is_ok() {
                    cluster.label = Some(label.clone());
                    continue;
                }
                warn!(
                    "Ignoring invalid cached cluster label for {}",
                    cluster.cluster_key
                );
            }
            pending.push((cluster.cluster_key.clone(), members));
        }

        if pending.is_empty() {
            return;
        }
        if clusters.len() > MAX_LABELLED_CLUSTERS {
            info!(
                "Skipping cluster labelling: {} clusters exceeds the {MAX_LABELLED_CLUSTERS} cap",
                clusters.len()
            );
            return;
        }

        let ctx = Arc::clone(self);
        let task = tokio::spawn(async move {
            for (key, members) in pending {
                let generator = Arc::clone(&generator);
                let members_for_task = members.clone();
                let generated = tokio::task::spawn_blocking(move || {
                    let refs: Vec<&str> = members_for_task.iter().map(String::as_str).collect();
                    cluster_label(generator.as_ref(), &refs)
                })
                .await;

                let label = match generated {
                    Ok(Ok(label)) => label,
                    // A cluster that would not label is that cluster's
                    // failure, and the next one is worth attempting. A gone
                    // worker is not: the next iteration would *start* one,
                    // which is how a cancel comes to need one kill per
                    // cluster. See the loop invariant in AGENTS.md.
                    Ok(Err(e)) => {
                        if wilkes_core::worker::fault::is_worker_gone(&e) {
                            info!("Stopping cluster labelling: the generator is gone ({e:#})");
                            break;
                        }
                        warn!("Could not label cluster {key}: {e:#}");
                        continue;
                    }
                    Err(e) => {
                        warn!("Cluster label task failed for {key}: {e}");
                        continue;
                    }
                };

                if let Ok(store) = ctx.research_store() {
                    if let Err(e) = store
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .upsert_cluster_label(
                            &key,
                            &label,
                            &model_id,
                            BOOKMARK_CLUSTER_LABEL_RECIPE_VERSION,
                        )
                    {
                        error!("Could not cache cluster label: {e:#}");
                    }
                }

                ctx.events.emit(
                    "bookmark-cluster-labelled",
                    serde_json::json!({ "cluster_key": key, "label": label }),
                );
            }
        });
        *self.cluster_label_task.lock() = Some(task);
    }
}

pub(super) fn bookmark_embedding_input(bookmark: &Bookmark) -> String {
    let quote = bookmark.quote.trim();
    let note = bookmark.note.as_deref().map(str::trim).unwrap_or_default();
    match (quote.is_empty(), note.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("Selected passage:\n{quote}"),
        (true, false) => format!("Research note:\n{note}"),
        (false, false) => {
            format!("Selected passage:\n{quote}\n\nResearch note:\n{note}")
        }
    }
}
