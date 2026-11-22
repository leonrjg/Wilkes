//! Embedding text with the configured model, and the embedder's location
//! and capabilities.

use super::*;

/// Where a model runs, read once from the settings and carried by every plan
/// that will build an embedder, so that a plan and the installer it ends in
/// cannot disagree about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EmbeddingLocation {
    /// This machine's worker, on `device`, with `batch_size` texts per forward
    /// pass. The two are read together: a build that ran under one is not the
    /// same build under another, and neither has a safe guess at a call site.
    Local { device: String, batch_size: usize },
    /// Another Wilkes instance, reached as these settings say. Device and
    /// batch size are the host's own.
    Remote(RemoteEmbeddingSettings),
}

impl EmbeddingLocation {
    pub(super) fn from_settings(settings: &Settings, engine: EmbeddingEngine) -> Self {
        if settings.remote_embedding.enabled {
            Self::Remote(settings.remote_embedding.clone())
        } else {
            Self::local_from_settings(settings, engine)
        }
    }

    /// This machine, whatever the settings say about a remote host: what
    /// serves a request another instance sent here, and what manages the
    /// model files on this disk.
    pub(super) fn local_from_settings(settings: &Settings, engine: EmbeddingEngine) -> Self {
        Self::Local {
            device: settings.semantic.device_for(engine).to_string(),
            batch_size: settings.semantic.embed_batch_size,
        }
    }
}

/// Which side of a retrieval pair a text is.
///
/// An asymmetric model — E5, BGE, arctic-embed — is trained with a prefix on
/// one side or both, and applying the wrong one is not a small loss: measured
/// on a 6,600-record corpus, the same model placed the right answer at rank 52
/// with the query prefix and rank 1792 without it, while every similarity rose
/// (Underdog, ACQUISITION §12i). The role is therefore never a request field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EmbedRole {
    Query,
    Passage,
}

/// The model named by a host-side embedding request, as loaded.
pub(super) struct ModelEmbedder {
    pub(super) address: EmbedderAddress,
    pub(super) location: EmbeddingLocation,
    pub(super) embedder: Arc<dyn Embedder>,
}

/// Where the bearer token for the remote embedding host is kept, under the
/// installation's data directory. Beside the settings rather than in them,
/// because the settings are served to any client of the HTTP API.
pub const REMOTE_EMBEDDING_TOKEN_FILENAME: &str = "remote-embedding-token";

impl AppContext {
    /// Embed arbitrary short strings with the same model the semantic index
    /// uses. Consumers (the Underdog sidecar client) pin the returned model
    /// id and dimension and treat any later mismatch as a hard error, so the
    /// response always names both.
    /// Embed arbitrary strings with the model this workspace's index uses.
    ///
    /// `embedding_space_id` is passed in rather than read back out of the
    /// index: the resolver already established which space this request is
    /// about, and asking the index again would be a second answer that could
    /// disagree with the pin the caller was checked against.
    pub async fn embed_texts(
        &self,
        texts: Vec<String>,
        embedding_space_id: Option<String>,
    ) -> Result<EmbeddedTexts, String> {
        let mut embedded = self.embed_texts_in_role(texts, EmbedRole::Passage).await?;
        embedded.embedding_space_id = embedding_space_id;
        Ok(embedded)
    }
    /// Embed strings in the **query** role, with the model the addressed
    /// index uses, for a caller that ranks its own stored passage vectors.
    ///
    /// The vectors are not for storage. An asymmetric model puts a query and
    /// a passage of the same words in different places on purpose, so a
    /// query vector stored beside passage vectors is a passage that ranks
    /// wrongly against everything. The role is this method's, never a field
    /// the caller sets: see [`Self::embed_texts_in_role`].
    ///
    /// A blank text is refused rather than embedded, as it is for a search
    /// probe: it names nothing to look for.
    pub async fn embed_query_texts(
        &self,
        texts: Vec<String>,
        embedding_space_id: Option<String>,
    ) -> Result<EmbeddedTexts, String> {
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err("A query text is empty and names nothing to embed".to_string());
        }
        let mut embedded = self.embed_texts_in_role(texts, EmbedRole::Query).await?;
        embedded.embedding_space_id = embedding_space_id;
        Ok(embedded)
    }
    /// The two roles an asymmetric model distinguishes. Which one applies is
    /// decided by the endpoint a caller reached, never by a field it sets: a
    /// flag that can be set correctly can be set wrongly, and the caller is the
    /// party least able to know what the model was trained to expect.
    pub(super) async fn embed_texts_in_role(
        &self,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<EmbeddedTexts, String> {
        let embedder = self.embedder.lock().clone().ok_or_else(|| {
            "Semantic model unavailable. Build or restore the semantic index first.".to_string()
        })?;
        Self::embed_with(embedder, texts, role).await
    }
    /// Embed `texts` in `role` with `embedder`, and check that what comes back
    /// is one finite vector of the embedder's width per text.
    pub(super) async fn embed_with(
        embedder: Arc<dyn Embedder>,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<EmbeddedTexts, String> {
        if texts.is_empty() {
            return Err("No texts provided".to_string());
        }
        let identity = embedder.embedding_space_identity();
        let engine = embedder.engine().as_str().to_string();
        let model_id = embedder.model_id().to_string();
        let dimension = embedder.dimension();

        let expected = texts.len();
        let embedder_for_task = Arc::clone(&embedder);
        let vectors = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            match role {
                EmbedRole::Passage => embedder_for_task.embed_passages(&refs),
                EmbedRole::Query => embedder_for_task.embed_query(&refs),
            }
        })
        .await
        .map_err(|e| format!("Text embedding task panicked: {e}"))?
        .map_err(|e| format!("Could not embed texts: {e:#}"))?;

        if vectors.len() != expected {
            return Err(format!(
                "Embedder returned {} vectors for {expected} inputs",
                vectors.len()
            ));
        }
        for vector in &vectors {
            if vector.len() != dimension {
                return Err(format!(
                    "Text embedding dimension mismatch. Expected {dimension}, received {}",
                    vector.len()
                ));
            }
            if vector.iter().any(|value| !value.is_finite()) {
                return Err("Embedder returned a non-finite text vector".to_string());
            }
        }
        Ok(EmbeddedTexts {
            embedding_space_id: None,
            engine,
            model_id,
            dimension,
            vectors,
            embedding_space_identity: Some(identity),
        })
    }
    /// Embed `texts` in the passage role with the model `address` names, on
    /// this machine, for another instance that embeds here. See
    /// [`Self::embed_texts_with_model`].
    pub async fn embed_model_texts(
        &self,
        address: EmbedderAddress,
        texts: Vec<String>,
    ) -> Result<EmbeddedTexts, ConsumerError> {
        self.embed_texts_with_model(address, texts, EmbedRole::Passage)
            .await
    }
    /// The query-role counterpart of [`Self::embed_model_texts`]. A blank text
    /// is refused, as it is for every query.
    pub async fn embed_model_query_texts(
        &self,
        address: EmbedderAddress,
        texts: Vec<String>,
    ) -> Result<EmbeddedTexts, ConsumerError> {
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err("A query text is empty and names nothing to embed".into());
        }
        self.embed_texts_with_model(address, texts, EmbedRole::Query)
            .await
    }
    /// Embed with a model named by the caller rather than by an index.
    ///
    /// This is what lets another Wilkes use this one's compute: it names the
    /// model its own index is in, and the vectors come back with the identity
    /// of that space. Nothing is written here — no corpus, no index — and
    /// nothing is downloaded: a model this machine does not have is refused
    /// with `MODEL_NOT_INSTALLED`, and [`Self::install_model`] fetches it with
    /// progress.
    ///
    /// Always this machine's own worker, whatever this instance's own
    /// embedding location is: a host that passed requests on would hand out
    /// vectors from a machine nobody chose.
    pub(super) async fn embed_texts_with_model(
        &self,
        address: EmbedderAddress,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<EmbeddedTexts, ConsumerError> {
        if texts.is_empty() {
            return Err("No texts provided".into());
        }
        let embedder = self.model_embedder(&address).await?;
        let mut embedded = Self::embed_with(embedder.clone(), texts, role).await?;
        embedded.embedding_space_id = Some(embedder.embedding_space_identity().id().0);
        Ok(embedded)
    }
    /// The embedder for `address`, loaded on this context's worker.
    pub(super) async fn model_embedder(
        &self,
        address: &EmbedderAddress,
    ) -> Result<Arc<dyn Embedder>, ConsumerError> {
        let settings = self.settings().await;
        let location = EmbeddingLocation::local_from_settings(&settings, address.engine);
        let mut held = self.model_embedder.lock().await;
        if let Some(loaded) = held
            .as_ref()
            .filter(|loaded| loaded.address == *address && loaded.location == location)
        {
            return Ok(Arc::clone(&loaded.embedder));
        }

        // Whether the model is here is the catalogue's answer, not the
        // installer's: an installer may count a built-in model as available
        // before a byte of it is on disk, and would then download it inside
        // this request, with no progress and no way to cancel it.
        let installed = self
            .local_embedder_capabilities(&settings)
            .models
            .iter()
            .any(|model| {
                model.engine == address.engine
                    && model.model_id == address.model.model_id()
                    && model.locally_available
            });
        if !installed {
            return Err(ConsumerError::new(
                ConsumerErrorCode::ModelNotInstalled,
                format!(
                    "{} model {} is not installed on this host",
                    address.engine.as_str(),
                    address.model.model_id()
                ),
            ));
        }
        let installer =
            self.embedder_installer(&location, address.engine, address.model.clone())?;
        let embedder = self
            .load_embedder(installer)
            .await
            .map_err(|error| format!("Could not load {}: {error:#}", address.model.model_id()))?
            .ok_or_else(|| {
                ConsumerError::new(
                    ConsumerErrorCode::ModelNotInstalled,
                    format!(
                        "{} model {} has no files on this host",
                        address.engine.as_str(),
                        address.model.model_id()
                    ),
                )
            })?;
        *held = Some(ModelEmbedder {
            address: address.clone(),
            location,
            embedder: Arc::clone(&embedder),
        });
        Ok(embedder)
    }
    /// Download the model `address` names into this machine's model cache,
    /// for another instance that embeds here, reporting each step on
    /// `events` and ending with `Done` or `Error`.
    ///
    /// Download only: unlike the download this instance's own interface
    /// starts, it does not make the model this workspace's embedder, and it
    /// does not take the workspace's embedding slot, so it neither waits for
    /// nor blocks a build here.
    pub async fn install_model(
        &self,
        address: EmbedderAddress,
        events: mpsc::Sender<InstallEvent>,
    ) {
        let settings = self.settings().await;
        let location = EmbeddingLocation::local_from_settings(&settings, address.engine);
        let outcome = match self.embedder_installer(&location, address.engine, address.model) {
            Ok(installer) => {
                let (progress_tx, mut progress_rx) = mpsc::channel::<EmbedProgress>(64);
                let forward_events = events.clone();
                let forward = tokio::spawn(async move {
                    while let Some(progress) = progress_rx.recv().await {
                        if forward_events
                            .send(InstallEvent::Progress(progress))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                let result = installer.install(&self.model_dir, progress_tx).await;
                let _ = forward.await;
                result.map_err(|error| format!("{error:#}"))
            }
            Err(error) => Err(error),
        };
        let last = match outcome {
            Ok(()) => InstallEvent::Done,
            Err(error) => {
                error!("install_model: {error}");
                InstallEvent::Error(error)
            }
        };
        let _ = events.send(last).await;
    }
    /// The installer for a model at `location`.
    ///
    /// The one place an embedder installer is chosen: a build, a download, a
    /// restore and a managed corpus all come here, so "where does this
    /// installation embed" has a single answer.
    pub(super) fn embedder_installer(
        &self,
        location: &EmbeddingLocation,
        engine: EmbeddingEngine,
        model: wilkes_core::types::EmbedderModel,
    ) -> Result<Arc<dyn EmbedderInstaller>, String> {
        match location {
            EmbeddingLocation::Local { device, batch_size } => Ok(dispatch::get_installer(
                engine,
                model,
                self.worker_manager.clone(),
                device.clone(),
                *batch_size,
            )),
            EmbeddingLocation::Remote(remote) => Ok(Arc::new(RemoteInstaller::new(
                Arc::new(self.remote_host(remote)?),
                Arc::clone(&self.remote_seal),
                engine,
                model,
            ))),
        }
    }
    pub(super) fn remote_host(
        &self,
        remote: &RemoteEmbeddingSettings,
    ) -> Result<RemoteHost, String> {
        let token =
            wilkes_core::token_file::read_if_present(&self.remote_embedding_token_path())
                .map_err(|error| format!("Could not read the embedding host's token: {error:#}"))?;
        RemoteHost::new(&remote.address, token).map_err(|error| format!("{error:#}"))
    }
    /// Where the bearer token for the remote embedding host is stored.
    pub fn remote_embedding_token_path(&self) -> PathBuf {
        self.shared_data_dir.join(REMOTE_EMBEDDING_TOKEN_FILENAME)
    }
    /// What this installation can embed with, from wherever it embeds: the
    /// remote host's models when embedding runs there, this machine's
    /// otherwise. The model picker reads this.
    pub async fn embedder_capabilities(
        &self,
    ) -> Result<wilkes_core::types::EmbedderCapabilityManifest, String> {
        let settings = self.settings().await;
        if settings.remote_embedding.enabled {
            return self
                .remote_host(&settings.remote_embedding)?
                .capabilities()
                .await
                .map_err(|error| format!("{error:#}"));
        }
        Ok(self.local_embedder_capabilities(&settings))
    }
    /// What this machine can embed with, whatever this instance's own
    /// embedding location: the answer a host gives another instance.
    pub fn local_embedder_capabilities(
        &self,
        settings: &Settings,
    ) -> wilkes_core::types::EmbedderCapabilityManifest {
        dispatch::model_capabilities(
            &self.model_dir,
            &self.sbert_cache(),
            &settings.semantic.custom_models,
        )
    }
    /// Drop the embedder after the place embeddings run has changed, and load
    /// it again from there when search wants one. An index build already
    /// under way finishes where it started.
    pub fn embedding_location_changed(self: &Arc<Self>) {
        self.detach_semantic_embedder();
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            ctx.activate_required_search_runtime_from_disk().await;
        });
    }
    /// Attach the immutable managed workspace configuration to a concrete
    /// embedder and index. This is idempotent and refuses any exact-space
    /// mismatch; it never rewrites the managed manifest.
    /// Whether a model is resident for this workspace.
    ///
    /// The canonical corpus answers `false` for its whole life, and that is
    /// the point of [`Self::ensure_managed_index`].
    pub fn has_embedder(&self) -> bool {
        self.embedder.lock().is_some()
    }
}

/// The texts a projection import still has to embed: one `(hash, text)` per
/// distinct text hash that `vectors` holds nothing for, in the order each hash
/// first appears among the chunks. A text repeated across chunks is embedded
/// once, and every chunk carrying it is filled from that one vector.
pub(super) fn texts_to_embed<'a>(
    texts: impl Iterator<Item = &'a str>,
    text_hashes: &[String],
    vectors: &HashMap<String, Vec<f32>>,
) -> Vec<(String, String)> {
    let mut queued: HashSet<&str> = HashSet::new();
    texts
        .zip(text_hashes)
        .filter(|(_, hash)| !vectors.contains_key(*hash) && queued.insert(hash.as_str()))
        .map(|(text, hash)| (hash.clone(), text.to_string()))
        .collect()
}

/// Gives every chunk the vector held for its text hash. `vectors` must hold
/// one for every hash by now — adopted or freshly embedded — so a hash with
/// none is a broken import, refused rather than written short.
pub(super) fn fill_chunk_vectors<T>(
    chunks: &mut [(T, Vec<f32>)],
    text_hashes: &[String],
    vectors: &HashMap<String, Vec<f32>>,
) -> Result<(), ConsumerError> {
    for ((_, vector), hash) in chunks.iter_mut().zip(text_hashes) {
        *vector = vectors.get(hash).cloned().ok_or_else(|| {
            ConsumerError::new(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "no vector was adopted or computed for a canonical chunk's text",
            )
        })?;
    }
    Ok(())
}
