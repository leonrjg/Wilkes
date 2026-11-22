//! Opening the managed index and the documents admitted to it.

use crate::context::*;

impl AppContext {
    /// Whether this workspace needs a model resident, or only its index.
    ///
    /// The canonical managed corpus owns retained sources, extraction,
    /// chunking and stable passage identity, and no coordinate system. It has
    /// nothing to embed, so it has no reason to install, load and hold a
    /// model — and holding one meant every managed import spun up the embedder
    /// the corpus was *created* under, which is frozen at creation and
    /// reachable from no setting. A projection is the opposite: it is nothing
    /// but a space, so its model is the whole of it.
    pub async fn ensure_managed_runtime(self: &Arc<Self>) -> Result<String, ConsumerError> {
        self.ensure_managed_runtime_with(ManagedRuntime::WithEmbedder)
            .await
    }
    /// An index and no model: for the canonical corpus, which stores passages
    /// and never coordinates.
    ///
    /// Its index is opened as it stands rather than against the configured
    /// embedder. The space such an index was created under is vestigial — it
    /// holds no vectors for that space or any other — so validating it would
    /// refuse to open a perfectly good pile of passages because a setting
    /// elsewhere had changed.
    pub async fn ensure_managed_index(self: &Arc<Self>) -> Result<String, ConsumerError> {
        self.ensure_managed_runtime_with(ManagedRuntime::IndexOnly)
            .await
    }
    /// The model comes from [`Self::load_embedder`] and the loaded state is
    /// installed by `restore_store_*`, exactly as a user workspace's is. What
    /// is the managed corpus's own is only the index: it is created on first
    /// use rather than by a build, because nothing ever builds one, and its one
    /// root is its retained sources.
    pub(in crate::context) async fn ensure_managed_runtime_with(
        self: &Arc<Self>,
        runtime: ManagedRuntime,
    ) -> Result<String, ConsumerError> {
        let _runtime_guard = self.managed_runtime_lock.lock().await;
        let settings = self.settings().await;
        let selected = settings.semantic.selected.clone();
        if matches!(runtime, ManagedRuntime::IndexOnly) {
            let identity = wilkes_core::embed::identity::EmbeddingSpaceIdentity::for_runtime(
                selected.engine,
                selected.model.model_id(),
                selected.dimension,
            );
            if self.is_index_loaded() {
                return Ok(identity.id().0);
            }
            let index = self
                .open_or_create_managed_index(identity.clone(), |data_dir| {
                    SemanticIndex::open_for_maintenance(data_dir)
                })
                .await?;
            self.restore_store_index_only(index);
            return Ok(identity.id().0);
        }
        if let Some(embedder) = self.embedder.lock().clone() {
            let identity = embedder.embedding_space_identity();
            let index_arc = self.index.lock().clone();
            let guard = index_arc
                .lock()
                .map_err(|_| "Semantic index lock was poisoned".to_string())?;
            if guard
                .as_ref()
                .is_some_and(|index| index.validate_embedding_space(&identity).is_ok())
            {
                return Ok(identity.id().0);
            }
        }
        let installer = self.embedder_installer(
            &EmbeddingLocation::from_settings(&settings, selected.engine),
            selected.engine,
            selected.model.clone(),
        )?;
        let embedder = self
            .load_embedder(installer)
            .await
            .map_err(|error| format!("Could not load managed embedder: {error:#}"))?
            .ok_or_else(|| {
                "Could not load managed embedder: its model files are absent after install"
                    .to_string()
            })?;
        if embedder.dimension() != selected.dimension {
            return Err(ConsumerError::new(
                ConsumerErrorCode::ManagedWorkspaceConfigurationMismatch,
                format!(
                    "configured dimension {}, runtime dimension {}",
                    selected.dimension,
                    embedder.dimension()
                ),
            ));
        }
        let expected_identity = embedder.embedding_space_identity();
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();
        let index = self
            .open_or_create_managed_index(expected_identity.clone(), {
                let expected = expected_identity.clone();
                move |data_dir| {
                    let index = SemanticIndex::open(data_dir, &model_id, dimension)?;
                    index.validate_embedding_space(&expected)?;
                    Ok(index)
                }
            })
            .await?;
        self.restore_store_loaded_state(embedder, index);
        Ok(expected_identity.id().0)
    }
    /// This managed workspace's index, opened with `open` when it exists and
    /// created under `identity` when it does not, with its retained sources
    /// as its active root. `open` is where an existing index's space is
    /// checked, for the workspaces whose space means something.
    ///
    /// The canonical corpus opens its index for maintenance and checks no
    /// space, because the space it was created under is vestigial: it holds no
    /// vectors for that space or any other, and refusing a good pile of
    /// passages because a setting elsewhere changed would help nobody.
    pub(in crate::context) async fn open_or_create_managed_index(
        &self,
        identity: wilkes_core::embed::identity::EmbeddingSpaceIdentity,
        open: impl FnOnce(&Path) -> anyhow::Result<SemanticIndex> + Send + 'static,
    ) -> Result<SemanticIndex, ConsumerError> {
        let data_dir = self.data_dir.clone();
        tokio::task::spawn_blocking(move || {
            let root = data_dir.join("managed_sources");
            std::fs::create_dir_all(&root)?;
            let mut index = if data_dir.join("semantic_index.db").exists() {
                open(&data_dir)?
            } else {
                SemanticIndex::create_exact(&data_dir, &identity, Some(&root))?
            };
            index.activate_root(&root)?;
            Ok::<_, anyhow::Error>(index)
        })
        .await
        .map_err(|error| format!("Managed index task panicked: {error}"))?
        .map_err(|error| ConsumerError::untyped(format!("Could not open managed index: {error:#}")))
    }
    pub(in crate::context) fn sanitize_managed_source_name(path: &Path) -> String {
        let original = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source");
        let sanitized: String = original
            .chars()
            .map(|character| {
                if character.is_alphanumeric() || matches!(character, '.' | '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        if sanitized.is_empty() {
            "source".to_string()
        } else {
            sanitized
        }
    }
    /// The retained snapshots this managed corpus has admitted, each with the
    /// rendition it was admitted as — what a projection must hold to be level
    /// with it.
    pub fn managed_admitted_sources(&self) -> Result<Vec<(PathBuf, String)>, ConsumerError> {
        let index_arc = self.index.lock().clone();
        let guard = index_arc
            .lock()
            .map_err(|_| "Semantic index lock was poisoned")?;
        guard
            .as_ref()
            .ok_or_else(|| "Managed index unavailable".to_string())?
            .managed_admitted_renditions()
            .map_err(|error| ConsumerError::from_anyhow(&error))
    }
    /// Imports into this corpus that are waiting for, or holding, its import
    /// lock.
    pub fn managed_pending_imports(&self) -> u64 {
        self.managed_pending_imports.load(Ordering::Acquire)
    }
}
