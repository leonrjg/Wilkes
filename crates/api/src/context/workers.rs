//! Worker processes: status, timeouts, removal, shutdown and wind-down.

use super::*;

/// How long a worker gets to stop before files are removed from under it. A
/// stop between requests is answered within the roof knock's three seconds.
pub(super) const WORKER_REMOVAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// How long *exiting* waits for a stopped build to finish writing.
///
/// Not a cancellation budget and not an exception to one: by the time this is
/// reached every model process is dead and no document is being read. What is
/// left is a SQLite merge that publishes what the build finished, and a quit
/// that refused to wait for it would throw away the hours it is saving. The
/// user's cancel never waits here — only the last moments of the process do.
pub(super) const WINDDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

impl AppContext {
    /// Whether [`Self::shutdown`] has run. For callers that own several
    /// contexts and have to assert they released all of them.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }
    pub async fn shutdown(self: &Arc<Self>) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stop_directory_watcher();
        self.cancel_text_index_upkeep();
        self.shut_down_feeds();
        self.cancel_all_completions();
        self.cancel_embed().await;
        self.await_winddown().await;
        if let Some(task) = self.cluster_label_task.lock().take() {
            task.abort();
        }
        for (_, operation) in self.topic_operations.lock().drain() {
            operation.cancel_flag.store(true, Ordering::Relaxed);
            operation.cancel.cancel();
            if let Some(task) = operation.label_task {
                task.abort();
            }
        }
        self.kill_all_workers();
    }
    /// Status of every worker. Two processes can die independently, so a single
    /// status would misreport a dead generation worker as healthy.
    pub fn get_worker_statuses(&self) -> Vec<WorkerStatus> {
        self.managers()
            .into_iter()
            .map(|manager| manager.status())
            .collect()
    }
    /// The embedding worker's status. Kept for callers that only care about
    /// indexing; new callers should prefer `get_worker_statuses`.
    pub fn get_worker_status(&self) -> WorkerStatus {
        self.worker_manager.status()
    }
    /// Every worker this context owns, and the only list of them.
    ///
    /// Written once because it was written three times: `status`, `shutdown`
    /// and cancellation each named the managers by hand, so adding the
    /// recognition worker meant remembering it in three places and it was
    /// remembered in two. Cancelling a build then killed the embedder — which
    /// the build was not waiting on — and left the recognizer running.
    pub(super) fn managers(&self) -> [&WorkerManager; 3] {
        [
            &self.worker_manager,
            &self.generate_manager,
            &self.recognize_manager,
        ]
    }
    pub fn kill_worker(&self) {
        self.worker_manager.request_shutdown();
    }
    /// Where the Python worker downloads SBERT models to.
    pub fn sbert_cache(&self) -> PathBuf {
        self.worker_manager.paths().hub_cache()
    }
    /// Every built Python environment, with what uses it and what it occupies.
    pub async fn python_environments(&self) -> anyhow::Result<PythonEnvInventory> {
        let paths = self.worker_manager.paths().clone();
        tokio::task::spawn_blocking(move || python_env_inventory(&paths))
            .await?
            .map_err(|e| anyhow::anyhow!(e))
    }
    /// Stop the embedding worker and wait for it to be gone, for an operation
    /// that takes files out from under it.
    pub(super) async fn stop_worker_for_removal(&self) -> Result<(), String> {
        self.worker_manager.request_shutdown();
        let deadline = tokio::time::Instant::now() + WORKER_REMOVAL_DEADLINE;
        while self.worker_manager.live_pid().is_some() {
            if tokio::time::Instant::now() >= deadline {
                return Err("The embedding worker did not stop in time.".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(())
    }
    /// Remove Python environments. Any of them is rebuilt from its lock the
    /// next time a model needs it, the base one included.
    pub async fn remove_python_environments(&self, ids: Vec<String>) -> Result<(), String> {
        self.ensure_no_active_embed_task(
            "Python environments cannot be removed while an index is being built.",
        )?;
        self.stop_worker_for_removal().await?;
        remove_python_envs(self.worker_manager.paths(), &ids).await
    }
    /// Delete a model the user added: its weights, what is recorded about it,
    /// and its entry in the catalog. Its Python environment stays, because
    /// other models may share it.
    pub async fn remove_custom_model(
        &self,
        engine: wilkes_core::types::EmbeddingEngine,
        model_id: String,
    ) -> Result<(), String> {
        self.ensure_no_active_embed_task(
            "Models cannot be removed while an index is being built.",
        )?;
        let settings = self.settings().await;
        let selected = &settings.semantic.selected;
        if selected.engine == engine && selected.model.model_id() == model_id {
            return Err("Choose another model before removing the selected one.".to_string());
        }
        self.stop_worker_for_removal().await?;
        // The files are on this disk whatever the settings say about where
        // embeddings run.
        let installer = self.embedder_installer(
            &EmbeddingLocation::local_from_settings(&settings, engine),
            engine,
            wilkes_core::types::EmbedderModel(model_id.clone()),
        )?;
        let model_dir = self.model_dir.clone();
        tokio::task::spawn_blocking(move || installer.uninstall(&model_dir))
            .await
            .map_err(|e| format!("Model removal panicked: {e}"))?
            .map_err(|e| format!("{e:#}"))?;
        self.update_semantic_settings(|mut semantic| {
            semantic
                .custom_models
                .retain(|m| !(m.engine == engine && m.model_id == model_id));
            semantic
        })
        .await;
        Ok(())
    }
    pub fn kill_generation_worker(&self) {
        self.generate_manager.request_shutdown();
    }
    pub fn kill_recognition_worker(&self) {
        self.recognize_manager.request_shutdown();
    }
    /// Shut down every worker. A missed one leaks a multi-gigabyte process past
    /// app exit, and — during a cancellation — leaves the build waiting on it.
    pub fn kill_all_workers(&self) {
        for manager in self.managers() {
            manager.request_shutdown();
        }
    }
    /// What a cancellation does to the workers: kill without a grace period,
    /// and let nothing start another until the cancelled work has stopped.
    ///
    /// The generation worker is killed like the others but not sealed. No
    /// build submits to it, and sealing it would answer the user's next
    /// question with a dead worker for as long as the cancelled build took to
    /// finish writing — a cancel taking a feature down with it.
    pub fn seal_all_workers(&self) {
        self.worker_manager.seal();
        self.recognize_manager.seal();
        self.remote_seal.seal();
        self.generate_manager.request_shutdown();
    }
    pub(super) fn unseal_all_workers(&self) {
        self.worker_manager.unseal();
        self.recognize_manager.unseal();
        self.remote_seal.unseal();
    }
    /// Wait, at exit only, for a stopped build to finish publishing what it
    /// indexed.
    ///
    /// The one wait in the application that outlives a cancellation, and it is
    /// not a cancellation budget: by the time it is reached every model
    /// process is dead and no document is being read. What is left is the
    /// merge that saves the documents the build finished, and a quit that
    /// refused to wait for it would throw away the hours it is saving.
    pub(super) async fn await_winddown(&self) {
        let waiter = self.winddown.notified();
        tokio::pin!(waiter);
        if !self.embed_task_is_running() {
            return;
        }
        info!(
            "AppContext::shutdown: waiting up to {WINDDOWN_GRACE:?} for the stopped build to \
             finish writing"
        );
        if tokio::time::timeout(WINDDOWN_GRACE, waiter).await.is_err() {
            warn!(
                "AppContext::shutdown: the stopped build was still writing after \
                 {WINDDOWN_GRACE:?}; exiting without it."
            );
        }
    }
    pub async fn set_worker_timeout(&self, secs: u64) -> anyhow::Result<()> {
        self.worker_manager
            .send(ManagerCommand::SetTimeout(secs))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    // ── Startup restore ───────────────────────────────────────────────────────
}
