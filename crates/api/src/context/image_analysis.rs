//! Image analysis: the analyzer, recognizers and the layout detector.

use super::*;

impl AppContext {
    /// Attach, detach or reload the generator when the generation settings
    /// change. Both the on and off transitions matter: leaving a generator
    /// attached after the feature is switched off would keep a multi-gigabyte
    /// process alive behind a toggle the user turned off.
    /// Reconcile the process-wide image analyzer after a settings edit.
    ///
    /// The Ollama endpoint is in here because the describer speaks to it:
    /// moving the server moves the describer, even though nothing under
    /// `image_analysis` changed.
    pub(super) fn on_image_analysis_settings_maybe_changed(
        self: &Arc<Self>,
        before: &Settings,
        after: &Settings,
    ) {
        if before.image_analysis == after.image_analysis
            && before.generation.ollama_url == after.generation.ollama_url
        {
            return;
        }
        if !after.image_analysis.enabled {
            // Synchronously, with the settings transition: extraction reads the
            // analyzer on every call, and a disabled feature must not keep
            // enriching while a detach is queued.
            info!("image analysis disabled; detaching the analyzer");
            wilkes_core::extract::image::configure(None);
            return;
        }
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = ctx.load_image_analyzer().await {
                error!("could not attach the image analyzer: {error:#}");
            }
        });
    }
    /// Build the analyzer the settings describe and install it for the whole
    /// process.
    ///
    /// No longer expensive and no longer off the executor: the weights live in
    /// the recognition worker, and what is assembled here is the address of
    /// one plus the annotation cache. Still serialized against itself, because
    /// two edits in flight could otherwise install the older one's analyzer.
    /// A failure detaches rather than leaving the previous analyzer in place:
    /// the settings no longer describe it, and silently enriching under the
    /// old recipe is the one outcome that would put two answers into one index.
    pub async fn load_image_analyzer(self: &Arc<Self>) -> anyhow::Result<bool> {
        let _serialized = self.image_analyzer_load_lock.lock().await;
        let settings = self.get_settings().await;
        let built = wilkes_core::extract::image::build_analyzer(
            self.recognize_manager.clone(),
            &self.model_dir,
            &self.shared_data_dir,
            &settings.image_analysis,
            &settings.generation.ollama_url,
        );
        match built {
            Ok(analyzer) => {
                let attached = analyzer.is_some();
                wilkes_core::extract::image::configure(analyzer);
                Ok(attached)
            }
            Err(error) => {
                wilkes_core::extract::image::configure(None);
                self.events.emit(
                    "image-analysis-error",
                    serde_json::json!({ "error": format!("{error:#}") }),
                );
                Err(error)
            }
        }
    }
    /// The recognizer this Wilkes is configured to read with.
    ///
    /// One place answers it, because the install, the inventory and the
    /// analyzer must all be talking about the same recognizer — three call
    /// sites each deriving it from the settings is three chances to disagree.
    pub(super) async fn configured_recognizer(
        &self,
    ) -> (
        wilkes_core::extract::image::dispatch::RecognitionEngine,
        String,
    ) {
        let settings = self.settings().await;
        let engine = settings.image_analysis.engine;
        let model_id = settings
            .image_analysis
            .model
            .clone()
            .unwrap_or_else(|| engine.default_model().to_string());
        (engine, model_id)
    }
    /// Every recognizer this build can read with, described: what it costs,
    /// what kinds of region it produces, whether its weights are here — plus
    /// the engines the build compiled in, so a picker can tell an engine that
    /// is absent from one that merely has nothing selected.
    pub fn image_recognizer_catalogue(
        &self,
    ) -> wilkes_core::extract::image::dispatch::RecognizerCatalogue {
        wilkes_core::extract::image::recognizer_catalogue(&self.model_dir)
    }
    /// What a recognizer is, where it came from, and under what terms.
    /// Answers before the download, which is the only time it is of any use to
    /// whoever has to decide about it.
    ///
    /// Named by the caller rather than read out of the settings, because the
    /// recognizer whose licence and footprint have to be disclosed is the one
    /// being *considered*, and a picker's answer to that is not yet in the
    /// settings.
    pub fn image_recognizer_inventory(
        &self,
        engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
        model_id: &str,
    ) -> anyhow::Result<wilkes_core::types::RecognizerInventory> {
        wilkes_core::extract::image::recognizer_inventory(engine, model_id)
    }
    /// Download and verify the layout detector, then reconcile the analyzer.
    ///
    /// Its own command rather than a case of the recognizer install: there is
    /// one detector, it is not chosen from a catalogue, and it is required
    /// whenever image analysis is on — a document read without it contains
    /// none of the formulas and tables its pages typeset. Reconciled
    /// afterwards without a condition, because unlike a recognizer there is no
    /// "the settings name a different one": installing it always changes
    /// whether the analyzer can attach.
    ///
    /// Shares the recognizer's event stream, so a UI that already listens for
    /// image-analysis progress needs nothing new to show this.
    #[cfg(feature = "recognize-onnx")]
    pub async fn install_layout_detector(self: &Arc<Self>) -> anyhow::Result<()> {
        let (progress_tx, mut progress_rx) = mpsc::channel::<EmbedProgress>(64);
        let events = Arc::clone(&self.events);
        let forward = tokio::spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                events.emit("image-analysis-progress", serde_json::json!(progress));
            }
        });
        let model_dir = self.model_dir.clone();
        let installed = tokio::task::spawn_blocking(move || {
            wilkes_core::extract::image::install_layout_detector(&model_dir, Some(progress_tx))
        })
        .await?;
        let _ = forward.await;
        if let Err(error) = installed {
            self.events.emit(
                "image-analysis-error",
                serde_json::json!({ "error": format!("{error:#}") }),
            );
            return Err(error);
        }
        // Only when the feature is on. Installing the detector for a runtime
        // that is not enriching is a download, and attaching on the strength
        // of it would turn a download into a configuration change.
        if self.get_settings().await.image_analysis.enabled {
            self.load_image_analyzer().await?;
        }
        self.events
            .emit("image-analysis-done", serde_json::json!({}));
        Ok(())
    }
    /// Download and verify the named recognizer, then attach the analyzer if
    /// the settings already ask for that one.
    ///
    /// The subject is named by the caller. Reading it out of the settings
    /// would force a picker to commit a choice before the weights exist, and
    /// committing to an uninstalled recognizer is precisely the state
    /// `build_analyzer` refuses — so the order is install, then commit, and
    /// the settings never name a recognizer that is not on disk.
    ///
    /// Progress travels on its own event stream for the reason the generation
    /// one does: borrowing `embed-*` would put the UI into "indexing" with no
    /// terminal event to leave it.
    pub async fn install_image_recognizer(
        self: &Arc<Self>,
        engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
        model_id: String,
    ) -> anyhow::Result<()> {
        let (progress_tx, mut progress_rx) = mpsc::channel::<EmbedProgress>(64);
        let events = Arc::clone(&self.events);
        let forward = tokio::spawn(async move {
            while let Some(progress) = progress_rx.recv().await {
                events.emit("image-analysis-progress", serde_json::json!(progress));
            }
        });
        let model_dir = self.model_dir.clone();
        let subject = (engine, model_id.clone());
        let installed = tokio::task::spawn_blocking(move || {
            wilkes_core::extract::image::install_recognizer(
                engine,
                &model_id,
                &model_dir,
                Some(progress_tx),
            )
        })
        .await?;
        let _ = forward.await;
        if let Err(error) = installed {
            self.events.emit(
                "image-analysis-error",
                serde_json::json!({ "error": format!("{error:#}") }),
            );
            return Err(error);
        }
        // Only the configured recognizer is attachable. Installing one the
        // settings do not name is a download, not a reconfiguration, and
        // reconciling here would either re-attach what is already attached or
        // fail over a recognizer nobody asked to run.
        if self.configured_recognizer().await == subject {
            self.load_image_analyzer().await?;
        }
        self.events
            .emit("image-analysis-done", serde_json::json!({}));
        Ok(())
    }
}
