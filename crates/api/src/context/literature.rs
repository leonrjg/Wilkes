//! Literature providers, custom integrations and composed searches.

use super::*;

impl AppContext {
    /// Whether one literature provider is reachable and usable now, shipped or
    /// pasted in. Answers whether or not it is enabled, so a settings form can
    /// check it before switching it on.
    pub async fn literature_provider_status(
        &self,
        provider: String,
    ) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        let s = self.get_settings().await;
        crate::commands::integrations::literature::status(
            &s.integrations,
            Some(&self.catalogue_dir),
            &provider,
        )
        .await
    }
    /// One provider's record of a work, from the metadata cache when it holds
    /// one and from the provider when it does not.
    pub async fn literature_provider_lookup(
        &self,
        provider: String,
        doi: String,
    ) -> anyhow::Result<wilkes_core::types::ProviderWork> {
        let s = self.get_settings().await;
        crate::commands::integrations::literature::lookup(
            &s.integrations,
            Some(&self.catalogue_dir),
            self.metadata_cache(),
            &provider,
            &doi,
        )
        .await
    }
    /// What a draft manifest declares. Nothing is saved or sent; the saved
    /// integrations are read only because a draft that extends another
    /// provider is described together with what it takes from it.
    pub async fn custom_integration_summary(
        &self,
        manifest: String,
    ) -> crate::commands::integrations::custom::ManifestSummary {
        let settings = self.get_settings().await;
        crate::commands::integrations::custom::custom_integration_summary(
            manifest,
            &settings.integrations,
            &wilkes_core::integrations::allowed_programs(&settings.integrations),
        )
    }
    /// Store a manifest an agent wrote, as [`with_saved_manifest`] describes.
    /// The interface has no screen that started the edit, so it is told.
    ///
    /// [`with_saved_manifest`]: crate::commands::integrations::custom::with_saved_manifest
    pub async fn save_custom_integration_manifest(
        self: &Arc<Self>,
        manifest: String,
        enabled: Option<bool>,
    ) -> anyhow::Result<crate::commands::integrations::custom::SavedManifest> {
        let integrations = self.get_settings().await.integrations;
        let (next, saved) = crate::commands::integrations::custom::with_saved_manifest(
            &integrations,
            manifest,
            enabled,
            &wilkes_core::integrations::allowed_programs(&integrations),
        )?;
        self.update_settings(serde_json::json!({ "integrations": next }))
            .await?;
        self.events
            .emit("integrations-updated", serde_json::json!({ "kind": "mcp" }));
        Ok(saved)
    }
    /// Self-contained instructions for asking a model to translate service
    /// documentation or code into the manifest format this build accepts.
    pub fn custom_integration_authoring_prompt(&self) -> String {
        crate::commands::integrations::custom::custom_integration_authoring_prompt()
    }
    pub async fn custom_integration_probe(
        &self,
        manifest: String,
        secrets: std::collections::HashMap<String, String>,
        query: String,
    ) -> anyhow::Result<wilkes_core::integrations::custom::ProbeReport> {
        let settings = self.get_settings().await;
        crate::commands::integrations::custom::custom_integration_probe(
            manifest,
            secrets,
            query,
            &settings.integrations,
        )
        .await
    }
    /// Every literature provider this installation knows, each saying whether
    /// it is switched on. What a caller offers as a provider filter before it
    /// has asked anything.
    pub async fn literature_providers(
        &self,
    ) -> Vec<crate::commands::integrations::literature::LiteratureProviderInfo> {
        let s = self.get_settings().await;
        crate::commands::integrations::literature::providers(
            &s.integrations,
            Some(&self.catalogue_dir),
        )
    }
    /// One query against every enabled literature provider, or against the
    /// named subset of them, answered per provider. See
    /// [`crate::commands::integrations::literature`].
    pub async fn literature_search(
        &self,
        query: String,
        limit: Option<usize>,
        providers: Option<Vec<String>>,
        page: Option<usize>,
    ) -> Result<
        crate::commands::integrations::literature::LiteratureSearchResponse,
        crate::commands::integrations::literature::LiteratureRequestError,
    > {
        let s = self.get_settings().await;
        crate::commands::integrations::literature::search_all(
            &s.integrations,
            Some(&self.catalogue_dir),
            query,
            limit,
            providers,
            page,
        )
        .await
    }
    /// One provider's listing by filter and sort. What may be asked is what the
    /// provider's manifest declares; see
    /// [`crate::commands::integrations::literature::browse`].
    pub async fn literature_browse(
        &self,
        provider: String,
        request: wilkes_core::types::BrowseRequest,
    ) -> Result<
        crate::commands::integrations::literature::LiteratureProviderAnswer,
        crate::commands::integrations::literature::LiteratureRequestError,
    > {
        let settings = self.get_settings().await;
        crate::commands::integrations::literature::browse(
            &settings.integrations,
            Some(&self.catalogue_dir),
            provider,
            request,
        )
        .await
    }
    /// What a provider suggests for one of its filters as the user types.
    pub async fn literature_filter_suggest(
        &self,
        provider: String,
        filter: String,
        text: String,
    ) -> Result<
        Vec<wilkes_core::types::FilterSuggestion>,
        crate::commands::integrations::literature::LiteratureRequestError,
    > {
        let settings = self.get_settings().await;
        crate::commands::integrations::literature::suggest(
            &settings.integrations,
            Some(&self.catalogue_dir),
            provider,
            filter,
            text,
        )
        .await
    }
    /// Turns a request written in ordinary words into filters on each named
    /// provider, using the generation model. See
    /// [`crate::commands::integrations::literature::compose`].
    ///
    /// `request_id` is the caller's, so it can stop this with
    /// [`Self::literature_compose_cancel`]; a second request under an id still
    /// in flight stops the first.
    pub async fn literature_compose(
        &self,
        request_id: String,
        text: String,
        providers: Vec<String>,
    ) -> Result<
        crate::commands::integrations::literature::LiteratureComposition,
        crate::commands::integrations::literature::LiteratureRequestError,
    > {
        use crate::commands::integrations::literature::LiteratureRequestError;
        if request_id.trim().is_empty() || request_id.len() > 128 {
            return Err(LiteratureRequestError(
                "Invalid composition request id".into(),
            ));
        }
        if !self.is_generation_ready().await {
            return Err(LiteratureRequestError("Generation is not available".into()));
        }
        let generator = self
            .generator
            .lock()
            .clone()
            .ok_or_else(|| LiteratureRequestError("Generation model unavailable".into()))?;
        let settings = self.get_settings().await;

        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self
            .compose_operations
            .lock()
            .insert(request_id.clone(), Arc::clone(&cancel))
        {
            previous.store(true, Ordering::Relaxed);
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let composed = crate::commands::integrations::literature::compose(
            &settings.integrations,
            Some(&self.catalogue_dir),
            generator,
            text,
            providers,
            &today,
            Arc::clone(&cancel),
        )
        .await;
        // Only this request's own entry: a later one under the same id has
        // already replaced it and is not this one's to remove.
        let mut operations = self.compose_operations.lock();
        if operations
            .get(&request_id)
            .is_some_and(|current| Arc::ptr_eq(current, &cancel))
        {
            operations.remove(&request_id);
        }
        composed
    }
    /// Stops a composition in flight. The decode ends at its next token, and
    /// the request it belongs to answers that it was cancelled.
    pub fn literature_compose_cancel(&self, request_id: &str) {
        if let Some(cancel) = self.compose_operations.lock().remove(request_id) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}
