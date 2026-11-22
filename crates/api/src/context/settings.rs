//! Applying settings changes, and activating or detaching the search runtime
//! they govern.

use super::*;

impl AppContext {
    pub(super) async fn validate_index_root(
        &self,
        root: &Path,
        settings: &Settings,
        missing_dir_message: &str,
    ) -> Result<(), String> {
        let root_display = root.display().to_string();

        if root_display.trim().is_empty() {
            return Err(missing_dir_message.to_string());
        }
        if !root.exists() {
            return Err(format!("Index root not found: {}", root.display()));
        }
        if !root.is_dir() {
            return Err(format!("Index root is not a directory: {}", root.display()));
        }

        let files = crate::commands::files::list_files(
            root.to_path_buf(),
            settings.supported_extensions.clone(),
            settings.max_file_size,
        )
        .await
        .map_err(|err| format!("Failed to scan index root: {err}"))?;

        if files.files.is_empty() {
            return Err(format!(
                "No supported files found in selected directory: {}",
                root.display()
            ));
        }

        Ok(())
    }

    // ── Settings ──────────────────────────────────────────────────────────────
    pub async fn update_semantic_settings<F>(&self, f: F)
    where
        F: FnOnce(SemanticSettings) -> SemanticSettings,
    {
        let _lock = self.settings_lock.lock().await;
        let current = match get_scoped_settings(&self.settings_path, &self.workspace_path).await {
            Ok(s) => s,
            Err(e) => {
                error!("update_semantic_settings: read: {e:#}");
                return;
            }
        };
        let semantic = f(current.semantic);
        if let Err(e) = update_scoped_settings(
            &self.settings_path,
            &self.workspace_path,
            serde_json::json!({ "semantic": semantic }),
        )
        .await
        {
            error!("update_semantic_settings: write: {e:#}");
        }
    }
    pub(super) fn on_generation_settings_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        let before = &before.generation;
        let after = &after.generation;
        let attachment_changed = before.enabled != after.enabled
            || before.engine != after.engine
            || before.model != after.model
            || (after.engine == GenerationEngine::Candle && before.device != after.device)
            || (after.engine == GenerationEngine::Ollama
                && (before.ollama_url != after.ollama_url
                    || before.context_tokens != after.context_tokens));
        if !attachment_changed {
            return;
        }
        let ctx = Arc::clone(self);
        let enabled = after.enabled;
        let identity_changed = before.engine != after.engine
            || before.model != after.model
            || before.device != after.device
            || before.ollama_url != after.ollama_url;
        if !enabled {
            info!("generation disabled; detaching the generator");
            self.unload_generator();
            return;
        }
        if identity_changed {
            // Detach synchronously with the settings transition. Until the new
            // backend is attached, the shared readiness predicate must not
            // accidentally bless the generator selected by the old settings.
            self.unload_generator();
        }
        tokio::spawn(async move {
            match ctx.load_generator().await {
                Ok(GeneratorLoad::Attached) => info!("generation model attached"),
                Ok(GeneratorLoad::NotConfigured) => {
                    info!("generation enabled but no model selected")
                }
                Ok(GeneratorLoad::Superseded) => {
                    info!("generation model load superseded by a newer settings change")
                }
                Err(e) => error!("Could not attach the generation model: {e:#}"),
            }
        });
    }
    /// Admit a directory through the same persisted roots search validates.
    /// The read/append/write is serialized with settings edits, so concurrent
    /// uploads cannot overwrite another request's root registration.
    pub async fn register_library_root(self: &Arc<Self>, root: &Path) -> anyhow::Result<()> {
        self.ensure_writable()?;
        let root = root.canonicalize()?;
        anyhow::ensure!(root.is_dir(), "Library root is not a directory");
        let (before, updated) = {
            let _lock = self.settings_lock.lock().await;
            let before = get_scoped_settings(&self.settings_path, &self.workspace_path).await?;
            let (roots, _) = library_roots(&before);
            if roots.iter().any(|registered| root.starts_with(registered)) {
                return Ok(());
            }
            let mut recent_dirs = before.recent_dirs.clone();
            recent_dirs.push(root);
            let updated = update_scoped_settings(
                &self.settings_path,
                &self.workspace_path,
                serde_json::json!({ "recent_dirs": recent_dirs }),
            )
            .await?;
            (before, updated)
        };
        self.on_directory_setting_maybe_changed(&before, &updated);
        self.on_library_roots_maybe_changed(&before, &updated);
        Ok(())
    }
    pub async fn update_settings(
        self: &Arc<Self>,
        patch: serde_json::Value,
    ) -> anyhow::Result<wilkes_core::types::Settings> {
        let (before, updated) = {
            let _lock = self.settings_lock.lock().await;
            let before = get_scoped_settings(&self.settings_path, &self.workspace_path)
                .await
                .unwrap_or_default();
            // Where the host fixed the programs a manifest may run, no client
            // changes the stored list. One sent back as it was is no change.
            if let Some(programs) = patch.pointer("/integrations/allowed_programs") {
                let allowlist = wilkes_core::integrations::allowed_programs(&before.integrations);
                anyhow::ensure!(
                    allowlist.editable
                        || *programs == serde_json::json!(before.integrations.allowed_programs),
                    "The programs integrations may run are fixed by this server's WILKES_ALLOWED_PROGRAMS and cannot be changed here"
                );
            }
            let updated =
                update_scoped_settings(&self.settings_path, &self.workspace_path, patch).await?;
            (before, updated)
        };
        self.on_directory_setting_maybe_changed(&before, &updated);
        self.on_library_roots_maybe_changed(&before, &updated);
        self.on_zotero_settings_maybe_changed(&before, &updated);
        self.on_literature_provider_settings_maybe_changed(&before, &updated);
        self.on_search_runtime_settings_maybe_changed(&before, &updated);
        if before.remote_embedding != updated.remote_embedding {
            self.embedding_location_changed();
        }
        self.on_generation_settings_maybe_changed(&before, &updated);
        self.on_image_analysis_settings_maybe_changed(&before, &updated);
        Ok(updated)
    }
    /// Reconcile the search runtime after a settings edit.
    ///
    /// These two toggles own the embedder, not index residency. The reading
    /// surfaces read the retained extraction out of the same index and are
    /// consumers of it whatever search prefers, so turning search off releases
    /// the embedding model and nothing else; only deleting the index releases
    /// the index. Loads run off the settings write path, while the model made
    /// unnecessary by the new state detaches synchronously.
    pub(super) fn on_search_runtime_settings_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if before.search_prefer_semantic == after.search_prefer_semantic
            && before.grep_use_index == after.grep_use_index
        {
            return;
        }

        if !after.search_prefer_semantic {
            // Semantic search is off. Its model must not remain resident; the
            // index stays open for every other reader of the extraction.
            self.detach_semantic_embedder();
        }

        let semantic_just_enabled = after.search_prefer_semantic && !before.search_prefer_semantic;
        if semantic_just_enabled {
            // Reload can install/probe the embedder or open the index, so do it
            // off the settings write path.
            let ctx = Arc::clone(self);
            tokio::spawn(async move {
                ctx.activate_required_search_runtime_from_disk().await;
                // A newly restored index may hold renditions to copy.
                ctx.request_text_index_reconcile();
            });
        }

        // The document text index exists for exact search, so this setting
        // owns its upkeep.
        if after.grep_use_index && !before.grep_use_index {
            self.request_text_index_reconcile();
        } else if !after.grep_use_index && before.grep_use_index {
            self.cancel_text_index_upkeep();
        }
    }
    /// Release the semantic-only model state while preserving the index, which
    /// the reading surfaces and index-backed exact search still read.
    pub(super) fn detach_semantic_embedder(&self) {
        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = None;
    }
    /// Release the open index. The only reason to is that the index is going
    /// away: no settings toggle owns its residency, because the reading
    /// surfaces read it whenever one exists on disk.
    pub(super) fn detach_search_index(&self) {
        self.invalidate_topic_tree_cache();
        *self.index.lock() = Arc::new(Mutex::new(None));
    }
    pub(super) fn is_index_loaded(&self) -> bool {
        self.index
            .lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
    /// Reload the embedder, index, and watcher from the on-disk DB when a usable
    /// one exists. No-op if semantic is already live or nothing is built yet.
    pub(super) async fn activate_semantic_from_disk(self: &Arc<Self>) {
        if self.is_semantic_ready() {
            return;
        }
        let settings = self.get_settings().await;
        if !settings.search_prefer_semantic {
            return;
        }
        if let Some(loaded) = self.load_restore_state(settings).await {
            if !self.get_settings().await.search_prefer_semantic {
                return;
            }
            self.finish_restore_state(&loaded.plan, loaded.embedder, loaded.index)
                .await;
        }
    }
    /// Activate the runtime the latest settings require. The embedder is what
    /// the search settings decide; the index is opened either way, after
    /// semantic restoration so it can still succeed when the embedding model
    /// is unavailable.
    pub(super) async fn activate_required_search_runtime_from_disk(self: &Arc<Self>) {
        let settings = self.get_settings().await;
        if settings.search_prefer_semantic {
            self.activate_semantic_from_disk().await;
        }
        self.activate_index_from_disk().await;
    }
    /// Open the existing index without an embedder.
    ///
    /// Whether to open it is not a search question. The reading surfaces read
    /// the retained extraction out of this same index — a PDF preview's
    /// superseded areas, the reading regions behind them, figures — and they
    /// are consumers of it whatever `search_prefer_semantic` says; the
    /// document text index copies its renditions, and is one more consumer,
    /// not the owner. So the only gate here is whether an index is open
    /// already, and this is the single opener for the no-embedder case.
    ///
    /// This intentionally does not install, probe, or retain an embedding
    /// model. Files changed while no embedder is resident are rejected by
    /// identity checks and extracted live.
    pub(super) async fn activate_index_from_disk(self: &Arc<Self>) {
        if self.is_index_loaded() {
            return;
        }
        let settings = self.get_settings().await;
        let Some((_plan, index)) = self.load_restore_index_only(settings).await else {
            return;
        };
        // Semantic restore or a concurrent build may have installed one while
        // this open was in flight.
        if self.is_index_loaded() {
            return;
        }
        self.restore_store_index_only(index);
        info!("restore_state: index restored without embedder");
    }
    /// React to a change in Zotero configuration by keeping the metadata cache
    /// coherent with the current integration state: drop now-stale Zotero rows,
    /// and, if Zotero is (still/now) enabled, re-resolve file-based rows into
    /// authoritative Zotero rows in the background.
    pub(super) fn on_zotero_settings_maybe_changed(&self, before: &Settings, after: &Settings) {
        let z_before = &before.integrations.zotero;
        let z_after = &after.integrations.zotero;
        let relevant_changed =
            z_before.enabled != z_after.enabled || z_before.base_url != z_after.base_url;
        if !relevant_changed {
            return;
        }

        if let Some(cache) = self.metadata_cache() {
            if let Ok(guard) = cache.lock() {
                if let Err(e) = guard.invalidate_zotero() {
                    error!("metadata cache invalidate_zotero: {e:#}");
                }
            }
        }

        if z_after.enabled {
            self.spawn_zotero_backfill(after.clone());
        }
    }
    /// Keep the metadata cache coherent with the literature providers the user
    /// has configured. For every provider whose configuration changed — enabled
    /// or disabled, a different secret or base URL, an edited manifest, or gone
    /// altogether — what it recorded about files is dropped, and a provider
    /// that is now enabled and can look a work up is asked again.
    ///
    /// One rule for every provider. It used to be two near-identical methods,
    /// one per compiled-in provider, and a provider the user described had none
    /// at all.
    pub(super) fn on_literature_provider_settings_maybe_changed(
        &self,
        before: &Settings,
        after: &Settings,
    ) {
        let before_state = IntegrationRegistry::fingerprints(&before.integrations);
        let after_state = IntegrationRegistry::fingerprints(&after.integrations);
        let changed: Vec<&String> = before_state
            .keys()
            .chain(after_state.keys())
            .filter(|id| before_state.get(*id) != after_state.get(*id))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if changed.is_empty() {
            return;
        }

        if let Some(cache) = self.metadata_cache() {
            if let Ok(guard) = cache.lock() {
                for id in &changed {
                    if let Err(e) = guard.invalidate_provider(id) {
                        error!("metadata cache invalidate_provider {id}: {e:#}");
                    }
                }
            }
        }

        let registry =
            IntegrationRegistry::from_settings(&after.integrations, Some(&self.catalogue_dir));
        let backfill = changed.iter().any(|id| {
            registry.literature(id).is_some_and(|entry| {
                entry.enabled && {
                    let capabilities = entry.source().capabilities();
                    capabilities.lookup || capabilities.references
                }
            })
        });
        if backfill {
            self.spawn_provider_backfill(after.clone());
        }
    }
}
