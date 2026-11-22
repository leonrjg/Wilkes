//! Restoring the semantic runtime from what is on disk at startup.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct RestoreStatePlan {
    pub(super) db_status: IndexStatus,
    pub(super) selected: SelectedEmbedder,
    pub(super) location: EmbeddingLocation,
}

pub(super) struct RestoreLoadedState {
    pub(super) plan: RestoreStatePlan,
    pub(super) embedder: Arc<dyn Embedder>,
    pub(super) index: SemanticIndex,
}

pub(super) enum RestoreStatePreparation {
    Ready(RestoreStatePlan),
    ResetStaleSelection {
        db_status: IndexStatus,
        selected: SelectedEmbedder,
    },
}

impl AppContext {
    pub(super) fn restore_state_needs_reset(
        settings: &Settings,
        db_status: Option<&IndexStatus>,
    ) -> bool {
        match db_status {
            None => settings.semantic.enabled || settings.semantic.index_path.is_some(),
            Some(db_status) => {
                let selected = &settings.semantic.selected;
                db_status.engine != selected.engine
                    || db_status.model_id != selected.model.model_id()
            }
        }
    }
    pub(super) fn restore_state_indexing_config(settings: &Settings) -> IndexingConfig {
        IndexingConfig {
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
        }
    }
    pub(super) fn restore_state_enabled_settings(
        current: SemanticSettings,
        db_path: PathBuf,
        selected: SelectedEmbedder,
        dim: usize,
    ) -> SemanticSettings {
        SemanticSettings {
            enabled: true,
            index_path: Some(db_path),
            selected: SelectedEmbedder {
                dimension: dim,
                ..selected
            },
            ..current
        }
    }
    pub(super) fn open_semantic_index_with<F>(
        data_dir: &PathBuf,
        model_id: &str,
        expected_dim: usize,
        open: F,
    ) -> anyhow::Result<SemanticIndex>
    where
        F: FnOnce(&PathBuf, &str, usize) -> anyhow::Result<SemanticIndex>,
    {
        open(data_dir, model_id, expected_dim)
    }
    pub(super) fn prepare_restore_state_plan(
        settings: Settings,
        db_status: IndexStatus,
    ) -> RestoreStatePreparation {
        let selected = settings.semantic.selected.clone();
        if Self::restore_state_needs_reset(&settings, Some(&db_status)) {
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            }
        } else {
            RestoreStatePreparation::Ready(RestoreStatePlan {
                location: EmbeddingLocation::from_settings(&settings, selected.engine),
                db_status,
                selected,
            })
        }
    }
    pub(super) async fn clear_restore_state_settings(&self) {
        self.update_semantic_settings(|s| SemanticSettings {
            enabled: false,
            index_path: None,
            ..s
        })
        .await;
    }
    pub(super) async fn load_restore_db_status(&self, settings: &Settings) -> Option<IndexStatus> {
        match tokio::task::spawn_blocking({
            let d = self.data_dir.clone();
            move || SemanticIndex::read_status_from_path(&d)
        })
        .await
        {
            Ok(Ok(status)) => Some(status),
            Ok(Err(err)) => {
                info!("restore_state: no index DB ({err:#}), nothing to restore");
                if Self::restore_state_needs_reset(settings, None) {
                    self.clear_restore_state_settings().await;
                }
                None
            }
            Err(err) => {
                error!("restore_state: spawn_blocking panicked: {err}");
                None
            }
        }
    }
    pub(super) async fn load_restore_plan(&self, settings: Settings) -> Option<RestoreStatePlan> {
        let db_status = self.load_restore_db_status(&settings).await?;
        match Self::prepare_restore_state_plan(settings, db_status) {
            RestoreStatePreparation::Ready(plan) => Some(plan),
            RestoreStatePreparation::ResetStaleSelection {
                db_status,
                selected,
            } => {
                info!(
                    "restore_state: index selection '{:?}/{}' != settings selection '{:?}/{}', clearing stale index reference",
                    db_status.engine, db_status.model_id, selected.engine, selected.model.model_id()
                );
                self.clear_restore_state_settings().await;
                None
            }
        }
    }
    pub(super) async fn load_restore_state(
        self: &Arc<Self>,
        settings: Settings,
    ) -> Option<RestoreLoadedState> {
        let plan = self.load_restore_plan(settings).await?;

        let embedder = self
            .restore_embedder(&plan.selected, &plan.location)
            .await?;
        let index = self
            .restore_index(&plan.selected, embedder.dimension())
            .await?;
        if let Err(error) = index.validate_embedding_space(&embedder.embedding_space_identity()) {
            error!("restore_state: incompatible embedding space: {error:#}");
            return None;
        }

        Some(RestoreLoadedState {
            plan,
            embedder,
            index,
        })
    }
    pub(super) async fn load_restore_index_only(
        &self,
        settings: Settings,
    ) -> Option<(RestoreStatePlan, SemanticIndex)> {
        let plan = self.load_restore_plan(settings).await?;
        let index = self
            .restore_index(&plan.selected, plan.db_status.dimension)
            .await?;
        Some((plan, index))
    }
    pub(super) async fn restore_embedder(
        &self,
        selected: &SelectedEmbedder,
        location: &EmbeddingLocation,
    ) -> Option<Arc<dyn Embedder>> {
        let installer =
            match self.embedder_installer(location, selected.engine, selected.model.clone()) {
                Ok(installer) => installer,
                Err(err) => {
                    error!("restore_state: {err}");
                    return None;
                }
            };
        self.restore_embedder_with(installer).await
    }
    pub(super) async fn restore_embedder_with(
        &self,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Option<Arc<dyn Embedder>> {
        match self.load_embedder(installer).await {
            Ok(Some(embedder)) => Some(embedder),
            Ok(None) => {
                info!("restore_state: model files absent, skipping");
                None
            }
            Err(err) => {
                error!("restore_state: {err:#}");
                None
            }
        }
    }
    /// Load an embedder: install (a no-op once the model is cached, and the
    /// probe that settles a model's dimension), confirm its files are there,
    /// and build it on the blocking pool. `None` when the files are absent.
    ///
    /// The one way a model becomes resident, whatever asked for it — a
    /// workspace restoring its runtime, a finished download, or a managed
    /// projection about to embed. Each of those used to carry its own copy of
    /// these three steps, and they had drifted: one built the embedder on the
    /// async runtime's own thread, and one never checked that the install had
    /// left anything to build from.
    pub(super) async fn load_embedder(
        &self,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> anyhow::Result<Option<Arc<dyn Embedder>>> {
        let (probe_tx, _) = tokio::sync::mpsc::channel(1);
        installer
            .install(&self.model_dir, probe_tx)
            .await
            .map_err(|err| anyhow::anyhow!("Failed to probe model dimensions: {err:#}"))?;
        if !installer.is_available(&self.model_dir) {
            return Ok(None);
        }
        let model_dir = self.model_dir.clone();
        let embedder = tokio::task::spawn_blocking(move || installer.build(&model_dir))
            .await
            .map_err(|err| anyhow::anyhow!("building the embedder panicked: {err}"))?
            .map_err(|err| anyhow::anyhow!("building the embedder failed: {err:#}"))?;
        Ok(Some(embedder))
    }
    pub(super) async fn restore_index(
        &self,
        selected: &SelectedEmbedder,
        expected_dim: usize,
    ) -> Option<SemanticIndex> {
        let data_dir = self.data_dir.clone();
        let model_id = selected.model.model_id().to_string();
        match tokio::task::spawn_blocking(move || {
            Self::open_semantic_index_with(&data_dir, &model_id, expected_dim, |dir, model, dim| {
                SemanticIndex::open(dir, model, dim)
            })
        })
        .await
        {
            Ok(Ok(index)) => Some(index),
            Ok(Err(err)) => {
                error!("restore_state: open index: {err:#}");
                None
            }
            Err(err) => {
                error!("restore_state: open index panicked: {err}");
                None
            }
        }
    }
    pub(super) fn restore_store_loaded_state(
        &self,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) -> Arc<Mutex<Option<SemanticIndex>>> {
        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = Some(Arc::clone(&embedder));
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);
        self.spawn_full_text_backfill();
        index_arc
    }
    pub(super) fn restore_store_index_only(
        &self,
        index: SemanticIndex,
    ) -> Arc<Mutex<Option<SemanticIndex>>> {
        self.invalidate_topic_tree_cache();
        let index_arc = Arc::new(Mutex::new(Some(index)));
        *self.index.lock() = Arc::clone(&index_arc);
        self.spawn_full_text_backfill();
        index_arc
    }
    pub(super) async fn finish_restore_state(
        &self,
        plan: &RestoreStatePlan,
        embedder: Arc<dyn Embedder>,
        index: SemanticIndex,
    ) {
        self.restore_store_loaded_state(Arc::clone(&embedder), index);

        let db_path = self.data_dir.join("semantic_index.db");
        let dim = plan.db_status.dimension;
        self.update_semantic_settings(|s| {
            Self::restore_state_enabled_settings(s, db_path.clone(), plan.selected.clone(), dim)
        })
        .await;

        info!("restore_state: embedder and index restored");
    }
    /// Restore the runtime and restart the filesystem watcher. Run once after
    /// `new`.
    ///
    /// An index that exists on disk is opened whenever anything in the process
    /// reads from it, and the reading surfaces — PDF preview's superseded
    /// areas, reading regions, figures — are among those readers. So the search
    /// settings decide whether an *embedder* loads, never whether the reading
    /// store is opened: semantic search loads the embedder plus the index,
    /// everything else loads the index alone.
    pub async fn restore_state(self: Arc<Self>) {
        let settings = match get_scoped_settings(&self.settings_path, &self.workspace_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("restore_state: read settings: {e:#}");
                return;
            }
        };
        if let Some(root) = settings.last_directory.clone() {
            self.start_directory_watcher(root);
        }
        // Independent of the semantic toggle below: generation has its own
        // enable flag and must restore whether or not semantic search is on.
        if settings.generation.enabled && settings.generation.model.is_some() {
            let ctx = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = ctx.load_generator().await {
                    error!("restore_state: could not attach the generation model: {e:#}");
                }
            });
        }
        // Likewise independent: image analysis has its own enable flag, and
        // every extraction path — not only indexing — reads the analyzer.
        if settings.image_analysis.enabled {
            let ctx = Arc::clone(&self);
            tokio::spawn(async move {
                if let Err(e) = ctx.load_image_analyzer().await {
                    error!("restore_state: could not attach the image analyzer: {e:#}");
                }
            });
        }
        // No early return on the search toggles: they decide whether an
        // embedder is needed, not whether the reading store is opened. An index
        // on disk is opened for the reading surfaces regardless.
        self.activate_required_search_runtime_from_disk().await;
        // After the index, so renditions it holds are copied, not read again.
        // Also what catches the documents that changed while Wilkes was closed.
        self.request_text_index_reconcile();
        // Refreshes every feed now if the last refresh is older than the
        // interval, then whenever the interval has passed.
        self.start_feed_timer();
    }
}
