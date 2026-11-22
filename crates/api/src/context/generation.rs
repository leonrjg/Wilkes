//! Text generation: the generator, completions, steering, and document and
//! search summaries.

use super::*;

/// Outcome of `load_generator`. The three cases are distinct at every call
/// site: "no model attached" for want of configuration is the normal resting
/// state, while "superseded" means a newer load is already doing the work and
/// this one deliberately left the generator alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratorLoad {
    Attached,
    NotConfigured,
    Superseded,
}

impl GeneratorLoad {
    pub fn attached(self) -> bool {
        self == GeneratorLoad::Attached
    }
}

pub(super) struct CompletionOperation {
    pub(super) cancel: Arc<AtomicBool>,
}

impl AppContext {
    pub async fn generation_settings(&self) -> GenerationSettings {
        self.get_settings().await.generation
    }
    /// The single readiness predicate every LLM-dependent affordance is gated
    /// on. Not `settings.enabled` scattered across call sites — that is how a
    /// feature ends up half-gated, with a spinner spinning forever because the
    /// model was never installed.
    pub async fn is_generation_ready(&self) -> bool {
        let settings = self.generation_settings().await;
        settings.enabled && settings.model.is_some() && self.generator.lock().is_some()
    }
    /// Start the authoritative grounded-completion pipeline. Results are
    /// emitted on `completion://{id}` so callers can subscribe before starting
    /// and discard stale ids without a global event race.
    pub async fn request_completion(
        self: Arc<Self>,
        completion_id: String,
        request: CompletionRequest,
    ) -> Result<(), String> {
        if !self.is_generation_ready().await {
            return Err("Generation is not available".to_string());
        }
        if !self.is_semantic_ready() {
            return Err("Semantic index is not available".to_string());
        }
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "Embedding model is not available".to_string())?;
        let index = self.index.lock().clone();
        let generator = self
            .generator
            .lock()
            .clone()
            .ok_or_else(|| "Generation model is not available".to_string())?;
        let (roots, _) = library_roots(&self.get_settings().await);

        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(previous) = self.completion_operations.lock().insert(
            completion_id.clone(),
            CompletionOperation {
                cancel: Arc::clone(&cancel),
            },
        ) {
            previous.cancel.store(true, Ordering::Relaxed);
        }
        let ctx = Arc::clone(&self);
        tokio::task::spawn_blocking(move || {
            let dependencies = CompletionDependencies {
                embedder,
                index,
                generator,
                library_roots: roots,
            };
            let event_name = format!("completion://{completion_id}");
            let result = {
                let mut session = ctx.completion_session.lock();
                run_completion(
                    &completion_id,
                    &request,
                    &dependencies,
                    &mut session,
                    cancel.as_ref(),
                    &mut |event: CompletionEvent| {
                        ctx.events
                            .emit(&event_name, serde_json::to_value(event).unwrap_or_default());
                    },
                )
            };
            if let Err(error) = result {
                if !cancel.load(Ordering::Relaxed) {
                    error!(completion_id, "Grounded completion failed: {error:#}");
                    ctx.events.emit(
                        &event_name,
                        serde_json::to_value(CompletionEvent::Error {
                            message: format!("{error:#}"),
                        })
                        .unwrap_or_default(),
                    );
                }
            }
            let mut operations = ctx.completion_operations.lock();
            if operations
                .get(&completion_id)
                .is_some_and(|operation| Arc::ptr_eq(&operation.cancel, &cancel))
            {
                operations.remove(&completion_id);
            }
        });
        Ok(())
    }
    pub fn cancel_completion(&self, completion_id: &str) {
        if let Some(operation) = self.completion_operations.lock().remove(completion_id) {
            operation.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub(super) fn cancel_all_completions(&self) {
        for (_, operation) in self.completion_operations.lock().drain() {
            operation.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub async fn completion_feedback(
        self: &Arc<Self>,
        completion_id: &str,
        feedback: CompletionFeedback,
    ) -> Result<(), String> {
        let embedder = self
            .embedder
            .lock()
            .clone()
            .ok_or_else(|| "Embedding model is not available".to_string())?;
        let ctx = Arc::clone(self);
        let completion_id = completion_id.to_string();
        let plan = tokio::task::spawn_blocking(move || {
            ctx.completion_session
                .lock()
                .prepare_feedback(&completion_id, feedback)
        })
        .await
        .map_err(|error| format!("Feedback preparation task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (plan, vectors) = tokio::task::spawn_blocking(move || {
            let vectors = plan.embed(embedder.as_ref())?;
            anyhow::Ok((plan, vectors))
        })
        .await
        .map_err(|error| format!("Feedback embedding task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let ctx = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            ctx.completion_session.lock().apply_feedback(plan, vectors)
        })
        .await
        .map_err(|error| format!("Feedback application task failed: {error}"))?
        .map_err(|error| error.to_string())
    }
    pub fn get_session_steering(&self) -> SessionSteering {
        self.completion_session.lock().steering()
    }
    pub fn reset_session_steering(&self) {
        self.completion_session.lock().reset();
    }
    /// Persist an editor buffer. The target must already be a non-PDF document
    /// inside the configured library; saving never becomes a path-creation or
    /// arbitrary-filesystem API.
    pub async fn save_document(&self, path: PathBuf, text: String) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        let settings = self.get_settings().await;
        let (roots, _) = library_roots(&settings);
        let path = std::fs::canonicalize(&path)
            .map_err(|error| format!("Cannot save {}: {error}", path.display()))?;
        Self::ensure_path_in_library(&path, &roots, "Editor document")?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("pdf") {
            return Err("PDF documents are not editable".to_string());
        }
        if !settings.supported_extensions.iter().any(|supported| {
            supported
                .trim_start_matches('.')
                .eq_ignore_ascii_case(extension)
        }) {
            return Err("Document type is not editable".to_string());
        }
        tokio::fs::write(&path, text)
            .await
            .map_err(|error| format!("Failed to save {}: {error}", path.display()))
    }
    pub async fn list_generation_models(&self) -> anyhow::Result<Vec<GeneratorDescriptor>> {
        let settings = self.generation_settings().await;
        generate_dispatch::list_models(settings.engine, &self.model_dir, &settings.ollama_url).await
    }
    pub async fn fetch_generation_model_size(&self, model_id: &str) -> anyhow::Result<u64> {
        let settings = self.generation_settings().await;
        let engine = settings.engine;
        let model_id = model_id.to_string();
        tokio::task::spawn_blocking(move || generate_dispatch::fetch_model_size(engine, &model_id))
            .await?
    }
    pub(super) fn generation_device(settings: &GenerationSettings) -> String {
        settings
            .device
            .clone()
            .unwrap_or_else(|| "auto".to_string())
    }
    /// Download (if needed) and attach the configured generation model.
    ///
    /// Loads are serialised and epoch-stamped: every settings change spawns one,
    /// and a slow load for a model the user has since switched away from must not
    /// win the race and attach itself. The epoch is claimed before queueing, so
    /// the newest claim is the only one allowed to assign the generator.
    pub async fn load_generator(self: &Arc<Self>) -> anyhow::Result<GeneratorLoad> {
        let epoch = self.generator_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let _serialized = self.generator_load_lock.lock().await;
        if self.generator_epoch.load(Ordering::SeqCst) != epoch {
            info!("generation model load superseded before it started");
            return Ok(GeneratorLoad::Superseded);
        }

        // Read after taking the lock: the settings that matter are the current
        // ones, not the ones in force when this load was queued.
        let settings = self.generation_settings().await;
        if !settings.enabled {
            *self.generator.lock() = None;
            return Ok(GeneratorLoad::NotConfigured);
        }
        let Some(model) = settings.model.clone() else {
            *self.generator.lock() = None;
            return Ok(GeneratorLoad::NotConfigured);
        };

        // Generation download progress travels on its own event stream. The
        // `embed-*` events are owned by the semantic index lifecycle: borrowing
        // them here put the UI into "indexing" with no terminal event to leave it.
        let (progress_tx, progress_rx) = mpsc::channel::<EmbedProgress>(64);
        let forward = tokio::spawn(Self::forward_generation_progress(
            Arc::clone(&self.events),
            progress_rx,
        ));
        let attach = generate_dispatch::attach_generator(
            settings.engine,
            model.clone(),
            self.generate_manager.clone(),
            Self::generation_device(&settings),
            &self.model_dir,
            &settings.ollama_url,
            settings.context_tokens,
            progress_tx,
        )
        .await;
        let _ = forward.await;
        let generator = match attach {
            Ok(generator) => generator,
            Err(e) => {
                self.emit_generation_error(format!("{e:#}"));
                return Err(e);
            }
        };

        // Last check before the assignment: a newer load claimed the epoch while
        // this one was downloading, and its model is the one the user wants.
        if self.generator_epoch.load(Ordering::SeqCst) != epoch {
            info!(
                "generation model '{}' finished loading but was superseded; discarding it",
                model.model_id()
            );
            self.events.emit(
                "generation-done",
                serde_json::json!({ "model": model.model_id() }),
            );
            return Ok(GeneratorLoad::Superseded);
        }

        *self.generator.lock() = Some(generator);

        // Generation and embedding share the worker lifecycle default. Do not
        // read a second persisted timeout here: older builds wrote 60 seconds
        // into generation settings, causing a reload every minute even though
        // the common worker residency is five minutes.
        if settings.engine == GenerationEngine::Candle {
            let _ = self
                .generate_manager
                .send(ManagerCommand::SetTimeout(
                    wilkes_core::worker::DEFAULT_IDLE_TIMEOUT_SECS,
                ))
                .await;
        }
        self.events.emit(
            "generation-done",
            serde_json::json!({ "model": model.model_id() }),
        );
        Ok(GeneratorLoad::Attached)
    }
    pub(super) fn emit_generation_error(&self, message: impl Into<String>) {
        let message = message.into();
        error!("Generation model load failed: {message}");
        self.events.emit(
            "generation-error",
            serde_json::json!({ "message": message }),
        );
    }
    pub(super) async fn forward_generation_progress(
        events: Arc<dyn EventEmitter>,
        mut progress_rx: mpsc::Receiver<EmbedProgress>,
    ) {
        while let Some(progress) = progress_rx.recv().await {
            events.emit(
                "generation-progress",
                serde_json::to_value(&progress).unwrap_or_default(),
            );
        }
    }
    /// Detach the generator and reap its process. Called when the feature is
    /// switched off or the model changes.
    pub fn unload_generator(&self) {
        // Supersede an install/probe already in flight. Merely clearing the Arc
        // would let that older load attach itself again after disable returned.
        self.generator_epoch.fetch_add(1, Ordering::SeqCst);
        self.cancel_all_completions();
        *self.generator.lock() = None;
        self.kill_generation_worker();
    }
    /// One lifecycle and event contract for every user-facing token stream.
    /// Task closures own input preparation, prompts, and verification.
    pub(super) async fn run_generation_stream<F>(
        self: Arc<Self>,
        request_id: String,
        task: GenerationTask,
        generate: F,
    ) -> Result<(), String>
    where
        F: FnOnce(
                Arc<Self>,
                Arc<dyn Generator>,
                &mut dyn FnMut(&str) -> std::ops::ControlFlow<()>,
            ) -> anyhow::Result<Generated>
            + Send
            + 'static,
    {
        let result: Result<String, String> = async {
            if request_id.trim().is_empty() || request_id.len() > 128 {
                return Err("Invalid generation request id".to_string());
            }
            if !self.is_generation_ready().await {
                return Err("Generation is not available".to_string());
            }
            let generator = self
                .generator
                .lock()
                .clone()
                .ok_or_else(|| "Generation model unavailable".to_string())?;

            let events = Arc::clone(&self.events);
            let delta_request_id = request_id.clone();
            let ctx = Arc::clone(&self);
            let generated = tokio::task::spawn_blocking(move || {
                generate(ctx, generator, &mut |delta| {
                    events.emit(
                        "generation-stream",
                        serde_json::to_value(GenerationStreamEvent::Delta {
                            request_id: delta_request_id.clone(),
                            task,
                            delta: delta.to_string(),
                        })
                        .unwrap_or_default(),
                    );
                    std::ops::ControlFlow::Continue(())
                })
            })
            .await
            .map_err(|e| format!("Generation task panicked: {e}"))?
            .map_err(|e| format!("{e:#}"))?;
            Ok(generated.text)
        }
        .await;

        match result {
            Ok(text) => {
                self.events.emit(
                    "generation-stream",
                    serde_json::to_value(GenerationStreamEvent::Completed {
                        request_id,
                        task,
                        text,
                    })
                    .unwrap_or_default(),
                );
                Ok(())
            }
            Err(error) => {
                warn!("Generation stream {request_id} failed: {error}");
                self.events.emit(
                    "generation-stream",
                    serde_json::to_value(GenerationStreamEvent::Failed {
                        request_id,
                        task,
                        error: error.clone(),
                    })
                    .unwrap_or_default(),
                );
                Err(error)
            }
        }
    }
    /// Stream a one-sentence explanation of why `related_path` is related to
    /// `anchor_path`.
    pub async fn explain_related_document(
        self: Arc<Self>,
        request_id: String,
        anchor_path: PathBuf,
        related_path: PathBuf,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::RelationExplanation,
            move |ctx, generator, sink| {
                let anchor = ctx
                    .document_summary(&anchor_path)
                    .map_err(anyhow::Error::msg)?;
                let related = ctx
                    .document_summary(&related_path)
                    .map_err(anyhow::Error::msg)?;
                explain_relation(generator.as_ref(), &anchor, &related, sink)
            },
        )
        .await
    }
    /// Stream a concise summary of the document currently shown in the viewer.
    /// Extraction is on demand and independent of the semantic index.
    pub async fn summarize_document(
        self: Arc<Self>,
        request_id: String,
        path: PathBuf,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::DocumentSummary,
            move |ctx, generator, sink| {
                let input = ctx
                    .document_summary_input(&path)
                    .map_err(anyhow::Error::msg)?;
                generate_document_summary(generator.as_ref(), &input, sink)
            },
        )
        .await
    }
    /// Synthesize a cited answer from cleaned, rank-preserving search passages.
    pub async fn summarize_search_results(
        self: Arc<Self>,
        request_id: String,
        input: SearchResultsSummaryInput,
    ) -> Result<(), String> {
        self.run_generation_stream(
            request_id,
            GenerationTask::SearchResultsSummary,
            move |_ctx, generator, sink| {
                generate_search_results_summary(generator.as_ref(), &input, sink)
            },
        )
        .await
    }
    /// Title plus a leading excerpt, both from caches. Returns `Err` when there
    /// is no cached text: no fallback extraction.
    pub(super) fn document_summary(&self, path: &Path) -> Result<DocumentSummary, String> {
        let index = self.index.lock();
        let guard = index.lock().unwrap_or_else(|p| p.into_inner());
        let idx = guard
            .as_ref()
            .ok_or_else(|| "No semantic index found".to_string())?;
        let excerpt = idx
            .cached_document_excerpt(path, MAX_EXCERPT_CHARS)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("No cached text for {}", path.display()))?;

        Ok(DocumentSummary {
            title: self.document_title(path),
            excerpt,
        })
    }
    pub(super) fn document_summary_input(
        &self,
        path: &Path,
    ) -> Result<DocumentSummaryInput, String> {
        // The indexed reading first, and the file only when there is none.
        // Summarizing an indexed document used to re-extract it from disk —
        // the whole document, enriched, per press of the button — when the
        // index already held the text the summary is made of.
        let text = match self.indexed_document_text(path) {
            Some(text) => text,
            None => Self::read_document_text(path)?,
        };
        Ok(DocumentSummaryInput {
            title: self.document_title(path),
            text,
        })
    }
    /// This document's text as the index holds it, or `None` when the index is
    /// absent, does not hold it, or holds a legacy row with nothing stored.
    ///
    /// The one place that asks the index this question. Both readers that can
    /// fall back to the file ask it first, so that a document the index holds
    /// is never read from disk a second time under a second recipe.
    pub(super) fn indexed_document_text(&self, path: &Path) -> Option<String> {
        let index = self.index.lock();
        let guard = index.lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().and_then(|idx| {
            idx.indexed_document_for_path(path)
                .map_err(|error| {
                    info!(path = %path.display(), %error, "indexed text read failed");
                    error
                })
                .ok()
                .flatten()
                .map(|(text, _)| text)
                .filter(|text| !text.trim().is_empty())
        })
    }
    /// Read a document for a summary or a citation label, off the index.
    ///
    /// **Never enriches.** Both callers reach this only when the index cannot
    /// answer — an unindexed file, or a legacy row with no stored text — and
    /// neither is a place the user asked to spend a document's worth of
    /// inference: one is a summary button, the other is a citation label being
    /// filled in. Enriching here meant a recognition worker started behind a
    /// spinner that said nothing about it, for a read whose result is one
    /// paragraph of prose.
    ///
    /// The recipe therefore differs from the index's, and the previous comment
    /// here was right that this is a second answer to what the document says.
    /// The answer to that is above, in the callers: both consult the indexed
    /// reading first and only land here when there is none to consult. Where
    /// the index holds the document, the index is what is read.
    pub(super) fn read_document_text(path: &Path) -> Result<String, String> {
        // A PDF's glyphs, and an HTML file's visible text: neither file's
        // bytes are what a summary or a citation label should read.
        let text = if wilkes_core::extract::html::is_html(path)
            || path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        {
            wilkes_core::extract::native_text_registry()
                .find(path, None)
                .ok_or_else(|| format!("No extractor for {}", path.display()))?
                .extract(path)
                .map_err(|e| format!("Could not extract {}: {e:#}", path.display()))?
                .text
        } else {
            std::fs::read_to_string(path)
                .map_err(|e| format!("Could not read {}: {e}", path.display()))?
        };
        Ok(text)
    }
    /// Prefer the semantic index's current extracted text. If this file has no
    /// usable stored full text (including legacy empty rows), extract it once
    /// off the async runtime so citation labels still work without an index
    /// rebuild.
    pub(super) fn document_title(&self, path: &Path) -> String {
        FileIdentity::for_path(path)
            .zip(self.metadata_cache())
            .and_then(|(identity, cache)| {
                let cache = cache.lock().unwrap_or_else(|p| p.into_inner());
                cache.get_valid(path, identity).ok().flatten()
            })
            .and_then(|metadata| metadata.title.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    }

    // ── Build index ───────────────────────────────────────────────────────────
}
