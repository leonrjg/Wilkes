//! Building the semantic index and downloading models: embed tasks, index
//! jobs, progress and cancellation.

use super::*;

pub struct EmbedTaskHandle {
    pub operation: EmbedOperation,
    pub cancel: CancellationToken,
    pub cancel_flag: Arc<AtomicBool>,
    pub join: BuildHandle,
    /// What this build will say on its way out, said once — by the build
    /// itself, or by the user's cancel on its behalf. See [`BuildSettlement`].
    pub settlement: Arc<BuildSettlement>,
}

/// How long a cancellation gets, from the user asking to everything they asked
/// to stop having stopped.
///
/// The same three seconds the roof knock is worth, because it is the same
/// promise: a process that has not gone by then is killed rather than waited
/// for. Nothing in the cancel path waits for anything that is not bounded by
/// it.
pub const CANCEL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// A running build, held by callers that are not allowed to wait for it.
///
/// A [`JoinHandle`] offers `.await`, and the cancel path used to take it: it
/// raised the flag, killed the workers, and then waited for the build to
/// unwind — through whatever page it was rendering, an integrity check over
/// the temporary database and a merge into the live one. None of those is
/// bounded by anything, and all of them happened with the user looking at
/// "Cancelling…". So the cancel path is handed this instead. It cannot be
/// awaited; not waiting is a property of the type rather than something each
/// future caller has to remember.
pub struct BuildHandle(JoinHandle<anyhow::Result<()>>);

impl BuildHandle {
    pub fn new(join: JoinHandle<anyhow::Result<()>>) -> Self {
        Self(join)
    }

    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    /// End the task at its next await point.
    ///
    /// Only honest for work that *is* endable that way — a download, which
    /// spends its life waiting on a socket. A build spends its life on a
    /// blocking thread, where an abort reaches nothing; what ends a build is
    /// the cancel flag and the killed workers.
    pub fn abort(&self) {
        self.0.abort();
    }

    /// Watch the task against `deadline` and report how long it actually took.
    ///
    /// Consumes the handle: this is the only way to observe a build ending,
    /// and it observes it from a task of its own, so "when did the build
    /// stop" can never become something the user is made to wait for. An
    /// overrun is logged as an error naming the invariant, on the reasoning
    /// `WorkerRuntime` uses for workers started in a loop — the shape that
    /// produces it lives in the build, and this is the only place that can see
    /// it happen.
    pub fn watch(
        self,
        what: &'static str,
        deadline: std::time::Duration,
        when_ended: impl FnOnce(std::time::Duration, bool) + Send + 'static,
    ) {
        let mut join = self.0;
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut overran = false;
            if tokio::time::timeout(deadline, &mut join).await.is_err() {
                overran = true;
                error!(
                    "{what} was still running {deadline:?} after the user cancelled it. A \
                     cancellation is over in three seconds: everything it kills is killed \
                     without a grace period, and nothing it does waits on work that is not \
                     bounded by that. Whatever this is waiting on belongs in a process that can \
                     be killed, or behind a check of the cancel flag. See the \"a cancelled \
                     build is over in three seconds\" invariant in AGENTS.md."
                );
                let _ = (&mut join).await;
            }
            let elapsed = started.elapsed();
            if overran {
                error!("{what} finally ended {elapsed:?} after the user cancelled it.");
            } else {
                info!("{what} ended {elapsed:?} after the user cancelled it.");
            }
            when_ended(elapsed, overran);
        });
    }
}

/// What a build says on its way out, said exactly once.
///
/// A build settles itself: it closes its job in the journal and tells the
/// interface which terminal state it reached. A *cancelled* build cannot,
/// because the user has to be told at the moment they ask and the build may be
/// seconds from answering — still rendering a page, checking the temporary
/// database, merging it. So the canceller settles on the build's behalf, and
/// this is what keeps the two from both doing it, or neither.
pub struct BuildSettlement {
    /// The job this build opened, once it has opened one. Empty while the
    /// scope is still being walked, and for a build whose journal could not be
    /// opened at all — which is logged where it happens and is not a reason to
    /// refuse to build.
    pub(super) job: PLMutex<Option<(Arc<Mutex<IndexJobJournal>>, i64)>>,
    pub(super) settled: AtomicBool,
}

impl BuildSettlement {
    pub fn new() -> Self {
        Self {
            job: PLMutex::new(None),
            settled: AtomicBool::new(false),
        }
    }

    pub(super) fn record_job(&self, job: Option<(Arc<Mutex<IndexJobJournal>>, i64)>) {
        *self.job.lock() = job;
    }

    pub(super) fn job(&self) -> Option<(Arc<Mutex<IndexJobJournal>>, i64)> {
        self.job.lock().clone()
    }

    /// Claim the right to say how this build ended. The first caller gets it;
    /// everyone after it has nothing left to report.
    pub(super) fn claim(&self) -> bool {
        !self.settled.swap(true, Ordering::AcqRel)
    }
}

impl Default for BuildSettlement {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone)]
pub enum EmbedOperation {
    Download,
    Build,
}

impl EmbedOperation {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Download => "Download",
            Self::Build => "Build",
        }
    }
}

/// How many of a job's documents the activity view is given at a time.
///
/// A corpus can hold a hundred thousand files and none of that is a list anyone
/// scrolls; the job's counts say how many there are, and the rows shown are the
/// ones that need attention or are moving. Failures and unfinished documents
/// sort first, so the bound never hides them behind a wall of successes.
pub(super) const ACTIVITY_DOCUMENT_LIMIT: usize = 500;

/// The documents a build is over, and the job whose verdicts it carries on
/// from. The two travel together because a scope narrower than the root is
/// always narrower *because* an earlier job already settled the rest.
pub(crate) struct JobScope {
    pub(super) documents: Vec<PathBuf>,
    pub(super) continues: Option<i64>,
}

#[derive(Clone, Debug)]
pub(super) struct BuildIndexPlan {
    pub(super) root_path: PathBuf,
    pub(super) location: EmbeddingLocation,
    pub(super) chunk_size: usize,
    pub(super) chunk_overlap: usize,
    pub(super) supported_extensions: Vec<String>,
    /// The documents this build is over. `None` means every indexable file
    /// under the root, walked when the build starts; `Some` is a chosen subset
    /// — the documents a stopped job never reached, or the ones the user asked
    /// to retry.
    pub(super) documents: Option<Vec<PathBuf>>,
    /// The job this one continues, whose verdicts it inherits for every
    /// document outside its own scope.
    pub(super) continues: Option<i64>,
}

#[derive(Clone, Debug)]
pub(super) struct DownloadModelPlan {
    pub(super) location: EmbeddingLocation,
}

impl AppContext {
    pub(crate) fn embed_task_is_running(&self) -> bool {
        if self.embed_cancel_in_progress.load(Ordering::Acquire) {
            return true;
        }
        let guard = self.embed_task.lock();
        guard.as_ref().is_some_and(|task| !task.join.is_finished())
    }
    /// Why the next job cannot start yet. A cancelled build's wind-down is a
    /// different answer from a build that is still indexing, and the user who
    /// just pressed cancel is exactly the one who will see it — the cancel
    /// returned to them at once, and the writing it left behind did not.
    pub(super) fn embed_busy_reason(&self) -> Option<&'static str> {
        if self.embed_cancel_in_progress.load(Ordering::Acquire) {
            return Some(
                "The cancelled build is still saving what it indexed. Try again in a moment.",
            );
        }
        let guard = self.embed_task.lock();
        guard
            .as_ref()
            .is_some_and(|task| !task.join.is_finished())
            .then_some("A build is already in progress.")
    }
    /// The job is over — finished, failed, or cancelled and finally wound
    /// down. Workers are admitted again before the flag drops, because the
    /// flag is what lets the next build start and a build that started into
    /// sealed managers would be refused its first request.
    pub(super) fn clear_embed_task(&self) {
        *self.embed_task.lock() = None;
        self.unseal_all_workers();
        self.embed_cancel_in_progress
            .store(false, Ordering::Release);
        self.winddown.notify_waiters();
    }
    /// Start an index build in the background. Progress, completion, and errors
    /// are emitted through the `EventEmitter` as `embed-progress`, `embed-done`,
    /// and `embed-error` events. Returns as soon as the task is spawned.
    pub async fn start_build_index(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        self.start_build_index_over(root, selected, None).await
    }
    /// Build `root`, over every indexable file under it or over `documents`.
    ///
    /// One entry point for all three ways a build starts — the user asking for
    /// the directory, continuing a stopped job, retrying its failures — because
    /// they differ only in which documents are in scope, and having each of
    /// them own a copy of the cancellation and journalling lifecycle is how one
    /// of the copies ends up not raising the cancel flag before killing.
    pub(crate) async fn start_build_index_over(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
        documents: Option<JobScope>,
    ) -> Result<(), String> {
        self.ensure_writable().map_err(|error| error.to_string())?;
        info!(
            "AppContext::start_build_index: root={}, engine={}, model={}, documents={}",
            root,
            selected.engine.as_str(),
            selected.model.model_id(),
            documents.as_ref().map_or_else(
                || "whole root".to_string(),
                |s| s.documents.len().to_string()
            )
        );
        let plan = match self.prepare_build_index(&root, &selected, documents).await {
            Ok(plan) => plan,
            Err(err) => {
                info!("AppContext::start_build_index: prepare failed: {err}");
                self.emit_embed_error("Build", err.clone());
                return Err(err);
            }
        };

        self.events
            .emit("manager-event", serde_json::json!("Reindexing"));

        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let settlement = Arc::new(BuildSettlement::new());
        let join = Arc::clone(&self).spawn_build_index_task(
            plan,
            selected,
            cancel.clone(),
            Arc::clone(&cancel_flag),
            Arc::clone(&settlement),
        );

        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Build,
            cancel,
            cancel_flag,
            join: BuildHandle::new(join),
            settlement,
        });
        info!("AppContext::start_build_index: build task registered");
        Ok(())
    }
    /// Run `work` as this workspace's build: registered in `embed_task`, so
    /// that [`Self::cancel_embed`] stops it — flag first, then every worker
    /// sealed and killed, within [`CANCEL_DEADLINE`] — and a second build
    /// cannot start beside it.
    ///
    /// For a managed projection catching up with its corpus, which is a build
    /// in every sense the user's is: it loads a model and embeds documents
    /// with it. It used to run inline in whichever request asked for it, in no
    /// slot, so nothing could stop it and nothing reported it running.
    ///
    /// Two things differ from a build the user starts, each because the caller
    /// differs. A busy slot is waited for rather than refused, since the
    /// callers are imports arriving concurrently and every one of them is owed
    /// its catch-up. And the caller awaits the outcome, since it answers an
    /// HTTP request with it; a caller that goes away — the client gave up —
    /// cancels the build rather than leaving it running for nobody.
    ///
    /// `work` is handed the cancel flag and must check it between documents.
    pub(crate) async fn run_as_embed_task<Fut>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<AtomicBool>) -> Fut,
    ) -> anyhow::Result<()>
    where
        Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let mut work = Some(work);
        let (settlement, done_rx) = loop {
            let notified = self.winddown.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut guard = self.embed_task.lock();
                let free = !self.embed_cancel_in_progress.load(Ordering::Acquire)
                    && guard.as_ref().is_none_or(|task| task.join.is_finished());
                if free {
                    let cancel = CancellationToken::new();
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    let settlement = Arc::new(BuildSettlement::new());
                    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
                    let work = work
                        .take()
                        .expect("work is taken once, when the slot is won")(
                        Arc::clone(&cancel_flag),
                    );
                    let ctx = Arc::clone(self);
                    let task_settlement = Arc::clone(&settlement);
                    let join = tokio::spawn(async move {
                        let result = work.await;
                        // A cancelled build was settled by its canceller.
                        task_settlement.claim();
                        ctx.clear_embed_task();
                        // The caller may have gone, which is how a cancel
                        // starts; nobody is left to tell.
                        let _ = done_tx.send(result);
                        Ok(())
                    });
                    *guard = Some(EmbedTaskHandle {
                        operation: EmbedOperation::Build,
                        cancel,
                        cancel_flag,
                        join: BuildHandle::new(join),
                        settlement: Arc::clone(&settlement),
                    });
                    break (settlement, done_rx);
                }
            }
            notified.await;
        };

        struct CancelIfAbandoned {
            ctx: Arc<AppContext>,
            settlement: Option<Arc<BuildSettlement>>,
        }
        impl Drop for CancelIfAbandoned {
            fn drop(&mut self) {
                let Some(settlement) = self.settlement.take() else {
                    return;
                };
                let ours = self
                    .ctx
                    .embed_task
                    .lock()
                    .as_ref()
                    .is_some_and(|task| Arc::ptr_eq(&task.settlement, &settlement));
                if !ours {
                    return;
                }
                info!("AppContext::run_as_embed_task: caller went away; cancelling its build");
                match tokio::runtime::Handle::try_current() {
                    Ok(runtime) => {
                        let ctx = Arc::clone(&self.ctx);
                        runtime.spawn(async move { ctx.cancel_embed().await });
                    }
                    // Only at exit, where `shutdown` cancels every build itself.
                    Err(error) => warn!(
                        "AppContext::run_as_embed_task: no runtime to cancel an abandoned build \
                         on: {error}"
                    ),
                }
            }
        }
        let mut abandoned = CancelIfAbandoned {
            ctx: Arc::clone(self),
            settlement: Some(settlement),
        };
        let outcome = done_rx.await;
        abandoned.settlement = None;
        match outcome {
            Ok(result) => result,
            // The task was cancelled before it could answer.
            Err(_) => Err(anyhow::anyhow!("the build was cancelled")),
        }
    }

    // ── Download model ────────────────────────────────────────────────────────
    /// Download a model in the background and load it into state on success.
    pub async fn start_download_model(
        self: Arc<Self>,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        info!(
            "AppContext::start_download_model: engine={}, model={}",
            selected.engine.as_str(),
            selected.model.model_id()
        );
        let plan = match self.prepare_download_model(&selected).await {
            Ok(plan) => plan,
            Err(err) => {
                info!("AppContext::start_download_model: prepare failed: {err}");
                self.emit_embed_error("Download", err.clone());
                return Err(err);
            }
        };
        let join = Arc::clone(&self).spawn_download_model_task(plan, selected);

        let cancel = CancellationToken::new();
        *self.embed_task.lock() = Some(EmbedTaskHandle {
            operation: EmbedOperation::Download,
            cancel,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            join: BuildHandle::new(join),
            settlement: Arc::new(BuildSettlement::new()),
        });
        info!("AppContext::start_download_model: download task registered");
        Ok(())
    }
    pub(super) async fn prepare_build_index(
        &self,
        root: &str,
        selected: &SelectedEmbedder,
        documents: Option<JobScope>,
    ) -> Result<BuildIndexPlan, String> {
        if let Some(reason) = self.embed_busy_reason() {
            return Err(reason.into());
        }

        let root_path = PathBuf::from(root);
        let settings = self.settings().await;
        self.validate_index_root(
            &root_path,
            &settings,
            "Choose a directory before building an index.",
        )
        .await?;
        if documents.is_some() {
            self.ensure_subset_build_matches_index(&root_path, selected)
                .await?;
        }

        Ok(BuildIndexPlan {
            root_path,
            location: EmbeddingLocation::from_settings(&settings, selected.engine),
            chunk_size: settings.semantic.chunk_size,
            chunk_overlap: settings.semantic.chunk_overlap,
            supported_extensions: settings.supported_extensions.clone(),
            documents: documents.as_ref().map(|scope| scope.documents.clone()),
            continues: documents.and_then(|scope| scope.continues),
        })
    }
    /// A build over part of a root adds to the index it finds; it cannot add
    /// to an index built with another model, and the build's answer to that
    /// is to publish its few documents *in place of* the whole index. So a
    /// partial build — continuing, retrying, or indexing what is missing — is
    /// refused unless the existing index was built with `selected`. With no
    /// index on disk there is nothing to replace, and nothing to refuse.
    pub(super) async fn ensure_subset_build_matches_index(
        &self,
        root: &Path,
        selected: &SelectedEmbedder,
    ) -> Result<(), String> {
        if !self.data_dir.join("semantic_index.db").exists() {
            return Ok(());
        }
        let status =
            crate::commands::embed::get_index_status(&self.data_dir, Some(root.to_path_buf()))
                .await
                .map_err(|e| format!("{e:#}"))?;
        if status.engine != selected.engine
            || status.model_id != selected.model.model_id()
            || status.dimension != selected.dimension
        {
            return Err(format!(
                "The index was built with {} ({}, {} dimensions), not the selected {} ({}, {} \
                 dimensions). Indexing only some documents with a different model would replace \
                 the whole index; rebuild it with the selected model instead.",
                status.model_id,
                status.engine.as_str(),
                status.dimension,
                selected.model.model_id(),
                selected.engine.as_str(),
                selected.dimension,
            ));
        }
        Ok(())
    }
    /// The workspace's index job journal, opened on first use.
    ///
    /// Opening is also when jobs left `Running` by a process that no longer
    /// exists are adopted as interrupted. That is done here rather than in a
    /// startup hook because this is the first moment in the process at which it
    /// is knowably true, and because a hook is something a new entry point can
    /// forget to call.
    pub(crate) fn job_journal(&self) -> anyhow::Result<Arc<Mutex<IndexJobJournal>>> {
        let mut guard = self.job_journal.lock();
        if let Some(journal) = guard.as_ref() {
            return Ok(Arc::clone(journal));
        }
        let journal = IndexJobJournal::open(&self.data_dir)?;
        match journal.adopt_orphaned_jobs() {
            Ok(0) => {}
            Ok(adopted) => warn!(
                "[AppContext] {adopted} index job(s) were left running by a process that is gone; \
                 they are now reported as interrupted"
            ),
            Err(e) => error!("[AppContext] could not adopt orphaned index jobs: {e:#}"),
        }
        let journal = Arc::new(Mutex::new(journal));
        *guard = Some(Arc::clone(&journal));
        Ok(journal)
    }
    /// What the most recent indexing job for `root` did, document by document.
    pub async fn index_activity(&self, root: PathBuf) -> Result<IndexActivity, String> {
        let journal = self.job_journal().map_err(|e| format!("{e:#}"))?;
        let journal = journal.lock().map_err(|_| {
            "The index job journal is unavailable after an earlier panic".to_string()
        })?;
        journal
            .activity_for_root(&root, ACTIVITY_DOCUMENT_LIMIT)
            .map_err(|e| format!("{e:#}"))
    }
    /// The documents of `root`'s most recent job that ended with `outcome`,
    /// together with the job they came from.
    pub(super) fn job_documents_with_outcome(
        &self,
        root: &Path,
        outcome: DocumentOutcome,
    ) -> Result<JobScope, String> {
        let journal = self.job_journal().map_err(|e| format!("{e:#}"))?;
        let journal = journal.lock().map_err(|_| {
            "The index job journal is unavailable after an earlier panic".to_string()
        })?;
        let Some(job) = journal
            .latest_for_root(root)
            .map_err(|e| format!("{e:#}"))?
        else {
            return Err(format!(
                "No indexing job has been recorded for {}",
                root.display()
            ));
        };
        let documents = journal
            .paths_with_outcome(job.id, outcome)
            .map_err(|e| format!("{e:#}"))?;
        Ok(JobScope {
            documents,
            continues: Some(job.id),
        })
    }
    /// Index the documents the last job for `root` never reached.
    ///
    /// Failed documents are deliberately not included. A continuation is for
    /// work that was interrupted; re-attempting work that was attempted and
    /// broke is [`Self::retry_failed_documents`], and the difference matters
    /// because a corpus with one unreadable file would otherwise re-attempt it
    /// on every continuation forever.
    pub async fn continue_index_job(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        let root_path = PathBuf::from(&root);
        let scope = self.job_documents_with_outcome(&root_path, DocumentOutcome::Pending)?;
        if scope.documents.is_empty() {
            return Err("There is no remaining work for this directory.".into());
        }
        info!(
            "AppContext::continue_index_job: {} document(s) left for {root}",
            scope.documents.len()
        );
        self.start_build_index_over(root, selected, Some(scope))
            .await
    }
    /// Re-attempt the documents the last job for `root` failed on.
    pub async fn retry_failed_documents(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        let root_path = PathBuf::from(&root);
        let scope = self.job_documents_with_outcome(&root_path, DocumentOutcome::Failed)?;
        if scope.documents.is_empty() {
            return Err("No documents failed in this directory's last indexing job.".into());
        }
        info!(
            "AppContext::retry_failed_documents: retrying {} document(s) for {root}",
            scope.documents.len()
        );
        self.start_build_index_over(root, selected, Some(scope))
            .await
    }
    /// Index the documents under `root` that the semantic index does not hold.
    ///
    /// The catch-up for a root that is indexed except for some of its files —
    /// typically ones added while Wilkes was not running, which the directory
    /// watcher never saw. The scope is exactly what [`Self::index_coverage`]
    /// counts as missing, so the number the interface offers is the number
    /// this reads, and nothing the index already holds is touched. Documents
    /// an earlier job failed on are unindexed and so in scope: asking for the
    /// missing documents is asking for those too.
    ///
    /// It continues the root's latest job, when there is one, so that job's
    /// verdicts on everything outside the scope carry over to this one.
    pub async fn index_missing_documents(
        self: Arc<Self>,
        root: String,
        selected: SelectedEmbedder,
    ) -> Result<(), String> {
        let root_path = PathBuf::from(&root);
        let extensions = self.settings().await.supported_extensions.clone();
        // Read up front and let the lock go: the walk below is filesystem
        // work, as in `index_coverage`.
        let (settled_empty, continues) = {
            let journal = self.job_journal().map_err(|e| format!("{e:#}"))?;
            let journal = journal.lock().map_err(|_| {
                "The index job journal is unavailable after an earlier panic".to_string()
            })?;
            let empty = journal
                .paths_with_outcome_for_root(&root_path, DocumentOutcome::Empty)
                .map_err(|e| format!("{e:#}"))?;
            let latest = journal
                .latest_for_root(&root_path)
                .map_err(|e| format!("{e:#}"))?;
            (empty, latest.map(|job| job.id))
        };
        let documents = crate::commands::embed::missing_documents(
            &self.data_dir,
            root_path,
            extensions,
            settled_empty,
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
        if documents.is_empty() {
            return Err(
                "Every document in this directory is already in the semantic index.".into(),
            );
        }
        info!(
            "AppContext::index_missing_documents: {} document(s) missing from {root}",
            documents.len()
        );
        self.start_build_index_over(
            root,
            selected,
            Some(JobScope {
                documents,
                continues,
            }),
        )
        .await
    }
    pub(super) async fn prepare_download_model(
        &self,
        selected: &SelectedEmbedder,
    ) -> Result<DownloadModelPlan, String> {
        if let Some(reason) = self.embed_busy_reason() {
            return Err(reason.into());
        }

        let settings = self.settings().await;
        let Some(root_path) = settings.last_directory.clone() else {
            return Err(
                "Choose a directory before downloading a model and building an index.".to_string(),
            );
        };
        self.validate_index_root(
            &root_path,
            &settings,
            "Choose a directory before downloading a model and building an index.",
        )
        .await?;

        Ok(DownloadModelPlan {
            location: EmbeddingLocation::from_settings(&settings, selected.engine),
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_index_options(
        model_dir: PathBuf,
        index_dir: PathBuf,
        plan: &BuildIndexPlan,
        reporter: BuildReporter,
        cancel_flag: Arc<AtomicBool>,
        documents: Vec<PathBuf>,
        scope: BuildScope,
    ) -> crate::commands::embed::BuildIndexOptions {
        crate::commands::embed::BuildIndexOptions {
            model_dir,
            index_dir,
            reporter,
            cancel_flag,
            documents,
            scope,
            chunk_size: plan.chunk_size,
            chunk_overlap: plan.chunk_overlap,
            supported_extensions: plan.supported_extensions.clone(),
        }
    }
    /// Open a job for this build, or report that it will not be resumable.
    ///
    /// A journal that cannot be opened does not stop the build: indexing the
    /// corpus is the job, and losing the ability to describe it afterwards is
    /// not a reason to refuse. It is logged loudly, because an activity view
    /// that is empty for no visible reason is the failure this would otherwise
    /// present as.
    pub(super) fn begin_job(
        &self,
        root: &Path,
        documents: &[PathBuf],
        continues: Option<i64>,
    ) -> Option<(Arc<Mutex<IndexJobJournal>>, i64)> {
        let journal = match self.job_journal() {
            Ok(journal) => journal,
            Err(e) => {
                error!(
                    "[AppContext] the index job journal could not be opened; this build will not \
                     be resumable: {e:#}"
                );
                return None;
            }
        };
        let job_id = {
            let mut guard = match journal.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match guard.begin_continuing(root, documents, continues) {
                Ok(job_id) => job_id,
                Err(e) => {
                    error!(
                        "[AppContext] the index job could not be recorded; this build will not be \
                         resumable: {e:#}"
                    );
                    return None;
                }
            }
        };
        Some((journal, job_id))
    }
    /// Close a job in whatever state it ended.
    pub(super) fn end_job(
        &self,
        job: &Option<(Arc<Mutex<IndexJobJournal>>, i64)>,
        state: JobState,
        detail: Option<&str>,
    ) {
        let Some((journal, job_id)) = job else {
            return;
        };
        let guard = match journal.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // An empty detail string is how a cancellation is signalled to the
        // interface elsewhere; it is not an explanation, so it is not stored.
        let detail = detail.filter(|d| !d.is_empty());
        if let Err(e) = guard.finish(*job_id, state, detail) {
            error!(
                "[AppContext] index job {job_id} could not be closed as {}: {e:#}",
                state.as_str()
            );
        }
    }
    /// Drop the job history for a root, or for the whole workspace.
    pub(super) fn forget_jobs(&self, root: Option<&Path>) {
        let journal = match self.job_journal() {
            Ok(journal) => journal,
            Err(e) => {
                error!("[AppContext] could not open the index job journal to clear it: {e:#}");
                return;
            }
        };
        let guard = match journal.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let cleared = match root {
            Some(root) => guard.forget_root(root),
            None => guard.forget_all(),
        };
        if let Err(e) = cleared {
            error!("[AppContext] could not clear the index job history: {e:#}");
        }
    }
    pub(super) fn cleanup_partial_index_files(data_dir: &Path) {
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
    }
    /// The application's event stream, for code in sibling modules that has
    /// progress to report and no other way to reach the shell.
    pub(crate) fn emitter(&self) -> Arc<dyn EventEmitter> {
        Arc::clone(&self.events)
    }
    pub(super) fn emit_progress_event(&self, progress: &EmbedProgress) {
        self.events.emit(
            "embed-progress",
            serde_json::to_value(progress).unwrap_or_default(),
        );
        // The same count, on the stream that follows the index being rewritten
        // whatever is rewriting it — the one the watcher's passes report on.
        if let EmbedProgress::Build(build) = progress {
            emit_reindex_progress(self.events.as_ref(), ReindexProgress::from(build));
        }
    }
    pub(super) async fn open_built_index(
        &self,
        data_dir: PathBuf,
        identity: wilkes_core::embed::EmbeddingSpaceIdentity,
    ) -> Result<SemanticIndex, String> {
        match tokio::task::spawn_blocking(move || SemanticIndex::open_exact(&data_dir, &identity))
            .await
        {
            Ok(Ok(index)) => Ok(index),
            Ok(Err(err)) => Err(err.to_string()),
            Err(err) => Err(err.to_string()),
        }
    }
    pub(super) fn start_build_watcher(
        self: &Arc<Self>,
        root_path: PathBuf,
        _index_arc: Arc<Mutex<Option<SemanticIndex>>>,
        _embedder: Arc<dyn Embedder>,
        _indexing: IndexingConfig,
    ) {
        self.start_directory_watcher(root_path);
    }
    pub(super) async fn finish_build_index(
        self: &Arc<Self>,
        plan: &BuildIndexPlan,
        selected: &SelectedEmbedder,
        data_dir: &Path,
        embedder: Arc<dyn Embedder>,
    ) -> Result<(), String> {
        let mut index = self
            .open_built_index(data_dir.to_path_buf(), embedder.embedding_space_identity())
            .await?;
        index
            .activate_root(&plan.root_path)
            .map_err(|e| e.to_string())?;
        let actual_dim = index.status().dimension;
        let index_arc = Arc::new(Mutex::new(Some(index)));

        self.invalidate_topic_tree_cache();
        *self.embedder.lock() = Some(Arc::clone(&embedder));
        *self.index.lock() = Arc::clone(&index_arc);

        self.start_build_watcher(
            plan.root_path.clone(),
            index_arc,
            embedder,
            IndexingConfig {
                chunk_size: plan.chunk_size,
                chunk_overlap: plan.chunk_overlap,
                supported_extensions: plan.supported_extensions.clone(),
            },
        );

        self.update_semantic_settings(|s| SemanticSettings {
            index_path: Some(data_dir.join("semantic_index.db")),
            selected: SelectedEmbedder {
                dimension: actual_dim,
                ..selected.clone()
            },
            enabled: true,
            ..s
        })
        .await;

        // The build's renditions are text the document text index can copy.
        self.request_text_index_reconcile();
        self.events
            .emit("embed-done", serde_json::json!({ "operation": "Build" }));
        Ok(())
    }
    pub(super) async fn forward_embed_progress(
        events: Arc<dyn EventEmitter>,
        mut progress_rx: mpsc::Receiver<EmbedProgress>,
    ) {
        while let Some(progress) = progress_rx.recv().await {
            events.emit(
                "embed-progress",
                serde_json::to_value(&progress).unwrap_or_default(),
            );
        }
    }
    pub(super) fn spawn_build_index_task(
        self: Arc<Self>,
        plan: BuildIndexPlan,
        selected: SelectedEmbedder,
        cancel: CancellationToken,
        cancel_flag: Arc<AtomicBool>,
        settlement: Arc<BuildSettlement>,
    ) -> JoinHandle<anyhow::Result<()>> {
        // The index is the workspace's; the model artefacts are the
        // installation's. Passing `data_dir` for both is what made every
        // workspace download its own copy of the same model — and derive its
        // own embedding-space identity from that copy.
        let data_dir = self.data_dir.clone();
        let model_dir = self.model_dir.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = tokio::sync::mpsc::channel::<EmbedProgress>(128);
        let cancel_for_task = cancel.clone();

        tokio::spawn(async move {
            info!(
                "AppContext::spawn_build_index_task: entered for root={}",
                plan.root_path.display()
            );
            enum TerminalEvent {
                Done,
                Cancelled,
            }

            struct TerminalEventGuard {
                events: Arc<dyn EventEmitter>,
                terminal: Option<TerminalEvent>,
            }

            impl Drop for TerminalEventGuard {
                fn drop(&mut self) {
                    let Some(event) = self.terminal.take() else {
                        return;
                    };
                    let name = match event {
                        TerminalEvent::Done => "ReindexingDone",
                        TerminalEvent::Cancelled => "ReindexingCancelled",
                    };
                    self.events.emit("manager-event", serde_json::json!(name));
                }
            }

            let mut terminal_event = TerminalEventGuard {
                events: Arc::clone(&ctx.events),
                terminal: None,
            };

            // The scope is settled, and recorded, before a byte of the model is
            // fetched. A job whose document list is only known once the build
            // is under way cannot say what is left until it has already
            // finished finding out.
            let (documents, scope) = match plan.documents.clone() {
                Some(documents) => (documents, BuildScope::Subset),
                None => {
                    let root = plan.root_path.clone();
                    let extensions = plan.supported_extensions.clone();
                    let walked = tokio::task::spawn_blocking(move || {
                        crate::commands::embed::collect_indexable_paths(&root, &extensions)
                    })
                    .await
                    .unwrap_or_default();
                    (walked, BuildScope::WholeRoot)
                }
            };
            info!(
                "AppContext::spawn_build_index_task: {} document(s) in scope",
                documents.len()
            );

            let job = ctx.begin_job(&plan.root_path, &documents, plan.continues);
            // Recorded where the canceller can reach it: a cancel closes the
            // job at the moment it is asked for, and by then this task may be
            // seconds from being able to answer for itself.
            settlement.record_job(job.clone());
            let reporter = match job.clone() {
                Some((journal, job_id)) => BuildReporter::journalled(progress_tx, journal, job_id),
                None => BuildReporter::without_journal(progress_tx),
            };

            let options = Self::build_index_options(
                model_dir,
                data_dir.clone(),
                &plan,
                reporter,
                Arc::clone(&cancel_flag),
                documents,
                scope,
            );
            let build_ctx = Arc::clone(&ctx);
            let build_location = plan.location.clone();
            let build_selected = selected.clone();
            let build_root = plan.root_path.clone();
            let build_fut = async move {
                let installer = build_ctx
                    .embedder_installer(
                        &build_location,
                        build_selected.engine,
                        build_selected.model.clone(),
                    )
                    .map_err(anyhow::Error::msg)?;
                crate::commands::embed::build_index_with_installer(build_root, installer, options)
                    .await
            };
            tokio::pin!(build_fut);
            let mut progress_rx = progress_rx;

            loop {
                tokio::select! {
                    biased;

                    _ = cancel_for_task.cancelled(), if !cancel_flag.load(Ordering::Relaxed) => {
                        info!("AppContext::spawn_build_index_task: cancellation observed");
                        cancel_flag.store(true, Ordering::Relaxed);
                        // Every worker, not the embedder alone: the build is
                        // waiting on whichever one it last submitted to, and a
                        // kill that misses that one is not a kill. Sealed as
                        // well as killed, so that this build cannot start one
                        // again on its way out.
                        ctx.seal_all_workers();
                    }

                    res = &mut build_fut => {
                        info!("AppContext::spawn_build_index_task: build future completed");
                        // A cancelled build has already been settled — by the
                        // canceller, at the moment the user asked, because a
                        // verdict delivered whenever this thread finished
                        // unwinding is one the user waited for. The claim is
                        // what says so; the disk it leaves behind is still
                        // this task's to tidy.
                        let ours = settlement.claim();
                        match res {
                            Ok(embedder) => {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    Self::cleanup_partial_index_files(&data_dir);
                                    if ours {
                                        ctx.end_job(&job, JobState::Cancelled, None);
                                        terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                        ctx.emit_embed_error("Build", "");
                                    }
                                } else if let Err(err) = ctx
                                    .finish_build_index(&plan, &selected, &data_dir, embedder)
                                    .await
                                {
                                    if ours {
                                        ctx.end_job(&job, JobState::Failed, Some(&err));
                                        terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                        ctx.emit_embed_error("Build", err);
                                    }
                                } else if ours {
                                    ctx.end_job(&job, JobState::Completed, None);
                                    terminal_event.terminal = Some(TerminalEvent::Done);
                                }
                            }
                            Err(e) => {
                                if cancel_flag.load(Ordering::Relaxed) {
                                    Self::cleanup_partial_index_files(&data_dir);
                                    if ours {
                                        ctx.end_job(&job, JobState::Cancelled, None);
                                        terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                        ctx.emit_embed_error("Build", "");
                                    }
                                } else {
                                    let detail = format!("{e:#}");
                                    if ours {
                                        ctx.end_job(&job, JobState::Failed, Some(&detail));
                                        terminal_event.terminal = Some(TerminalEvent::Cancelled);
                                        ctx.emit_embed_error("Build", detail);
                                    }
                                }
                            }
                        }
                        break;
                    }

                    Some(p) = progress_rx.recv() => {
                        if !cancel_flag.load(Ordering::Relaxed) {
                            ctx.emit_progress_event(&p);
                        }
                    }
                }
            }
            ctx.clear_embed_task();
            Ok(())
        })
    }
    pub(super) fn spawn_download_model_task(
        self: Arc<Self>,
        plan: DownloadModelPlan,
        selected: SelectedEmbedder,
    ) -> JoinHandle<anyhow::Result<()>> {
        let model_dir = self.model_dir.clone();
        let ctx = Arc::clone(&self);
        let (progress_tx, progress_rx) = mpsc::channel::<EmbedProgress>(64);

        tokio::spawn(async move {
            let forward = tokio::spawn(Self::forward_embed_progress(
                Arc::clone(&ctx.events),
                progress_rx,
            ));

            let installer =
                ctx.embedder_installer(&plan.location, selected.engine, selected.model.clone());
            let result = match &installer {
                Ok(installer) => installer.install(&model_dir, progress_tx).await,
                Err(error) => {
                    drop(progress_tx);
                    Err(anyhow::anyhow!("{error}"))
                }
            };

            let _ = forward.await;

            match result.and_then(|()| installer.map_err(anyhow::Error::msg)) {
                Ok(installer) => {
                    if let Err(e) = ctx.probe_and_load_downloaded_model_with(installer).await {
                        ctx.emit_embed_error("Download", e);
                    } else {
                        ctx.events
                            .emit("embed-done", serde_json::json!({ "operation": "Download" }));
                    }
                }
                Err(e) => {
                    ctx.emit_embed_error("Download", format!("{e:#}"));
                }
            }

            ctx.clear_embed_task();
            Ok(())
        })
    }
    pub(super) async fn probe_and_load_downloaded_model_with(
        self: &Arc<Self>,
        installer: Arc<dyn EmbedderInstaller>,
    ) -> Result<(), String> {
        match self.load_embedder(installer).await {
            Ok(Some(embedder)) => {
                *self.embedder.lock() = Some(embedder);
                Ok(())
            }
            Ok(None) => Err("The download left no model files to load".to_string()),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    // ── Embed lifecycle ───────────────────────────────────────────────────────
    /// Stop what the user asked to stop, and be done before they can ask
    /// twice.
    ///
    /// Everything this does is bounded: the flag goes up, every worker the
    /// build can reach is sealed and killed without a grace period, and the
    /// build's verdict is written and announced *here* rather than whenever
    /// the thread that was cancelled gets round to unwinding. Nothing is
    /// awaited that is not bounded by [`CANCEL_DEADLINE`].
    ///
    /// This used to end by waiting for the build task, which reads as tidying
    /// up and is not: a build unwinds through the page it was rendering, an
    /// integrity check over the temporary database and a merge into the live
    /// one, none of them bounded by anything, all of them with the user
    /// looking at "Cancelling…". That work still happens — it is what saves
    /// the documents the build finished — but it happens behind the
    /// cancellation rather than inside it, watched against the deadline and
    /// killed off the user's path.
    pub async fn cancel_embed(self: &Arc<Self>) {
        info!("AppContext::cancel_embed: requested");
        let Some(task) = self.embed_task.lock().take() else {
            info!("AppContext::cancel_embed: no active task");
            self.kill_all_workers();
            return;
        };

        // The flag is raised *before* the workers die, and that order is the
        // whole point. A killed worker surfaces as an error on whatever the
        // build was waiting for; the build then reaches its next
        // between-document check. Raising the flag afterwards leaves a window
        // in which that check reads false, the next document starts, and its
        // first request spawns a replacement worker — a cancel undone by the
        // very thing it did.
        self.embed_cancel_in_progress.store(true, Ordering::Release);
        task.cancel_flag.store(true, Ordering::Relaxed);
        task.cancel.cancel();

        info!("AppContext::cancel_embed: sealing and killing every worker");
        self.seal_all_workers();

        match task.operation {
            EmbedOperation::Build => {
                // The user is told now, by the one asked to stop rather than
                // by the one being stopped. A build that has already settled
                // itself — it finished as the cancel arrived — keeps its own
                // verdict; the claim is what decides between them.
                if task.settlement.claim() {
                    let job = task.settlement.job();
                    self.end_job(&job, JobState::Cancelled, None);
                    self.events
                        .emit("manager-event", serde_json::json!("ReindexingCancelled"));
                    self.emit_embed_error("Build", "");
                }
                self.audit_cancel_deadline();
                let ctx = Arc::clone(self);
                task.join
                    .watch("The build", CANCEL_DEADLINE, move |_elapsed, _overran| {
                        // Whatever it was doing, it is not doing it any more:
                        // admit workers again and let the next job start. Here
                        // rather than only at the end of the build task, because a
                        // build task that panicked would otherwise leave the
                        // application refusing to build for the rest of the
                        // session.
                        ctx.clear_embed_task();
                    });
            }
            EmbedOperation::Download => {
                info!("AppContext::cancel_embed: aborting download task");
                self.emit_embed_error(task.operation.as_str(), "");
                task.join.abort();
                self.clear_embed_task();
            }
        }

        info!("AppContext::cancel_embed: completed");
    }
    /// Check, at the deadline, that what the user cancelled is gone.
    ///
    /// The seal means nothing can have started since, so a worker still alive
    /// three seconds after a kill is a kill that did not land. It is killed
    /// again and named in the log, loud rather than silent: a cancellation the
    /// application has to repeat is one the user has already waited out.
    pub(super) fn audit_cancel_deadline(self: &Arc<Self>) {
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(CANCEL_DEADLINE).await;
            if !ctx.embed_cancel_in_progress.load(Ordering::Acquire) {
                return;
            }
            for manager in ctx.managers() {
                let Some(pid) = manager.live_pid() else {
                    continue;
                };
                error!(
                    "AppContext::cancel_embed: worker pid {pid} was still alive \
                     {CANCEL_DEADLINE:?} after the user cancelled; killing it again. Everything a \
                     cancellation kills is killed without a grace period and is gone within the \
                     deadline. See the \"a cancelled build is over in three seconds\" invariant \
                     in AGENTS.md."
                );
                manager.seal();
            }
        });
    }
    pub async fn delete_index(&self, root: Option<PathBuf>) -> anyhow::Result<()> {
        self.ensure_writable().map_err(anyhow::Error::msg)?;
        self.cancel_all_completions();
        self.invalidate_topic_tree_cache();
        // The job history goes with the coverage it describes. A report about
        // documents this workspace no longer has an index for is a report about
        // nothing, and it would offer to continue work into an index that has
        // been deleted underneath it.
        if let Some(root) = root {
            crate::commands::embed::delete_index(&self.data_dir, Some(root.clone())).await?;
            self.forget_jobs(Some(&root));
            let index_arc = self.index.lock().clone();
            if let Ok(mut guard) = index_arc.lock() {
                if let Some(idx) = guard.as_mut() {
                    let _ = idx.delete_root(&root);
                }
            };
        } else {
            self.detach_search_index();
            self.detach_semantic_embedder();
            crate::commands::embed::delete_index(&self.data_dir, None).await?;
            self.forget_jobs(None);
            self.update_semantic_settings(|s| SemanticSettings {
                index_path: None,
                ..s
            })
            .await;
        }
        Ok(())
    }
    pub async fn get_index_status(&self, root: Option<PathBuf>) -> anyhow::Result<IndexStatus> {
        crate::commands::embed::get_index_status(&self.data_dir, root).await
    }
    /// How much of each of `roots` the semantic index covers.
    ///
    /// Answers a question no status row can: whether a root's directory has
    /// moved on from what was indexed. The interface asks it so that switching
    /// to a root can say "this one is not indexed" instead of quietly starting
    /// hours of inference the user did not ask for.
    ///
    /// Reads only. Both sources are consulted because neither is complete on
    /// its own: the index says which documents it holds, and the journal says
    /// which ones were read and found to hold no text — those have no index row
    /// and never will, and counting them as missing would leave a root that has
    /// been fully read permanently reported as incomplete.
    pub async fn index_coverage(&self, roots: Vec<PathBuf>) -> Result<Vec<RootCoverage>, String> {
        let extensions = self.settings().await.supported_extensions.clone();
        // Read the journal's verdicts up front and let its lock go: the walks
        // below are filesystem work, and a build running beside them writes to
        // the journal a document at a time.
        let settled_empty = {
            let journal = self.job_journal().map_err(|e| format!("{e:#}"))?;
            let journal = journal.lock().map_err(|_| {
                "The index job journal is unavailable after an earlier panic".to_string()
            })?;
            roots
                .iter()
                .map(|root| {
                    journal
                        .paths_with_outcome_for_root(root, DocumentOutcome::Empty)
                        .map_err(|e| format!("{e:#}"))
                })
                .collect::<Result<Vec<_>, String>>()?
        };
        crate::commands::embed::root_coverage(&self.data_dir, roots, extensions, settled_empty)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    // ── Worker management ─────────────────────────────────────────────────────
}
