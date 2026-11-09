use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::ipc::{CancelSignal, WorkerEvent, WorkerKind, WorkerRequest, WorkerRole};
use super::manager::{
    GenerationWorkerStatus, ManagerCommand, ManagerEvent, WorkerPaths, WorkerStatus,
};
use super::process::{Stop, WorkerProcess};
use super::residency::{self, KindResidency, Residency, MIN_TENURE_WHILE_OTHERS_WAIT};
use super::{EnvNeeds, DEFAULT_IDLE_TIMEOUT_SECS};

/// How often a wait the seal has to interrupt looks at it. Far inside the three
/// seconds a cancellation has.
const SEAL_POLL: Duration = Duration::from_millis(100);

pub(super) async fn until_sealed(sealed: &AtomicBool) {
    while !sealed.load(Ordering::Acquire) {
        tokio::time::sleep(SEAL_POLL).await;
    }
}

#[async_trait]
pub(crate) trait WorkerSession: Send + Sync {
    fn start_request(
        &self,
        req_json: String,
        reply: mpsc::Sender<WorkerEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;

    async fn shutdown(&self, pid_slot: &AtomicU32, stop: Stop);

    /// Ask the worker to stop the request it is currently serving. Only the
    /// generation mode acts on this; every other mode ignores the line.
    async fn cancel_active_request(&self) -> Result<(), ()>;
}

pub(super) type ActiveProcessSlot = Arc<std::sync::Mutex<Option<Arc<dyn WorkerSession>>>>;

#[async_trait]
trait WorkerProcessSpawner: Send + Sync {
    async fn spawn(
        &self,
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<Arc<dyn WorkerSession>, String>;
}

struct RealWorkerProcessSpawner;

#[async_trait]
impl WorkerProcessSpawner for RealWorkerProcessSpawner {
    async fn spawn(
        &self,
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<Arc<dyn WorkerSession>, String> {
        let proc = WorkerProcess::spawn(paths, req, active_pid, reply).await?;
        Ok(Arc::new(proc))
    }
}

#[async_trait]
impl WorkerSession for WorkerProcess {
    fn start_request(
        &self,
        req_json: String,
        reply: mpsc::Sender<WorkerEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> {
        let proc = self.clone();
        Box::pin(async move { proc.send_request(&req_json, &reply).await })
    }

    async fn shutdown(&self, pid_slot: &AtomicU32, stop: Stop) {
        WorkerProcess::shutdown(self, pid_slot, stop).await;
    }

    async fn cancel_active_request(&self) -> Result<(), ()> {
        let line = serde_json::to_string(&CancelSignal { cancel: true })
            .expect("CancelSignal serialization cannot fail");
        WorkerProcess::send_out_of_band(self, &line).await
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn supervised_manager_loop(
    paths: WorkerPaths,
    kind: WorkerKind,
    initial_rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    active_process: ActiveProcessSlot,
    sender_slot: Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>,
    status: Arc<RwLock<WorkerStatus>>,
    sealed: Arc<AtomicBool>,
) {
    let mut rx = initial_rx;
    let spawner: Arc<dyn WorkerProcessSpawner> = Arc::new(RealWorkerProcessSpawner);
    // Resolved once, here, rather than per start: the kind a manager supervises
    // is fixed for its whole life, and so is the semaphore that admits it.
    let residency = residency::for_kind(kind);
    loop {
        let runtime = WorkerRuntime::new(
            paths.clone(),
            rx,
            event_tx.clone(),
            Arc::clone(&active_pid),
            Arc::clone(&active_process),
            Arc::clone(&status),
            Arc::clone(&spawner),
            Arc::clone(&residency),
            Arc::clone(&sealed),
        );
        let handle = tokio::task::spawn(runtime.run());
        match handle.await {
            Ok(()) => break,
            Err(e) if e.is_panic() => {
                tracing::error!("WorkerManager: loop panicked, restarting: {e:?}");
                rx = restart_runtime_after_panic(&active_pid, &status, &sender_slot);
            }
            Err(e) => {
                tracing::error!("WorkerManager: loop task cancelled: {e:?}");
                break;
            }
        }
    }
}

/// Clears everything that describes the *running process*. `role` is left
/// alone: it is the manager's identity, set once at construction, and a
/// manager whose worker died is still the manager for that role.
fn reset_worker_status(status: &Arc<RwLock<WorkerStatus>>) {
    if let Ok(mut current) = status.write() {
        current.active = false;
        current.engine = None;
        current.model = None;
        current.device = None;
        current.request_mode = None;
        current.pid = None;
    }
}

fn reset_after_runtime_panic(
    active_pid: &Arc<AtomicU32>,
    status: &Arc<RwLock<WorkerStatus>>,
    sender_slot: &Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>,
) -> mpsc::Receiver<ManagerCommand> {
    active_pid.store(0, Ordering::Relaxed);
    reset_worker_status(status);
    let (new_tx, new_rx) = mpsc::channel(32);
    *sender_slot.lock().unwrap() = new_tx;
    new_rx
}

struct WorkerRuntime {
    paths: WorkerPaths,
    rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    active_process_slot: ActiveProcessSlot,
    status: Arc<RwLock<WorkerStatus>>,
    spawner: Arc<dyn WorkerProcessSpawner>,
    active_process: Option<Arc<dyn WorkerSession>>,
    active_role: Option<WorkerRole>,
    active_model: Option<String>,
    active_device: Option<String>,
    /// The Python environment the active worker runs in.
    active_env: Option<EnvNeeds>,
    idle_timeout: Duration,
    /// Commands taken off the channel while a request was in flight, held back
    /// until it finished. Only the cancel signal is acted on mid-request.
    deferred: VecDeque<ManagerCommand>,
    /// What admits a worker of this manager's kind, shared with every other
    /// manager of that kind in the process.
    residency_kind: Arc<KindResidency>,
    /// When this manager took the permit it holds, for the tenure it keeps
    /// before giving way to a manager waiting behind it.
    resident_since: Option<tokio::time::Instant>,
    /// Held for exactly as long as this manager has a live worker. Taken before
    /// the spawn and dropped in `clear_active_worker`, which is where every
    /// stop funnels — an idle timeout, a restart for a new kind, a closed
    /// channel and a death under a request alike — so there is no path that
    /// ends a worker without admitting the next one.
    residency: Option<Residency>,
    /// Whether the worker this manager last held ended by dying under a
    /// request, rather than by being stopped on purpose.
    last_stop_was_a_death: bool,
    /// Raised while the user's cancel is in force. Read at the one place a
    /// worker is born and the one place a request is sent, which is what makes
    /// a kill final: see [`super::manager::WorkerManager::seal`].
    sealed: Arc<AtomicBool>,
    /// How many workers this manager has started to replace one that died
    /// under a request, since the last deliberate stop.
    ///
    /// Zero is the only correct value. A worker is started inside a loop
    /// exactly when this climbs: the loop's next iteration submits, finds no
    /// process, and spawns a replacement that reloads the model — which is how
    /// a cancel comes to be outrun once per iteration. Counted and logged
    /// rather than merely prevented, because the shape that produces it lives
    /// in the caller and this is the only place that can see it happening.
    replacements: u32,
}

enum NextCommand {
    Received(ManagerCommand),
    ChannelClosed,
    IdleTimeout,
    /// Another manager of this kind is waiting and this one has held the
    /// permit for its tenure: give it up now, between requests.
    GiveWay,
}

/// Resolves when a holder that took its permit at `since` owes it to a
/// manager waiting behind it: someone is waiting, and the tenure is up.
/// Never, for a manager holding nothing.
async fn give_way_due(kind: Arc<KindResidency>, since: Option<tokio::time::Instant>) {
    let Some(since) = since else {
        return std::future::pending().await;
    };
    loop {
        kind.until_others_wait().await;
        tokio::time::sleep_until(since + MIN_TENURE_WHILE_OTHERS_WAIT).await;
        if kind.others_waiting() {
            return;
        }
    }
}

fn serialize_request_for_worker(req: &WorkerRequest) -> Result<String, String> {
    serde_json::to_string(req).map_err(|e| format!("Serialize error: {e}"))
}

/// Whether this request needs a fresh worker process.
///
/// What the running process *is*, not which role it last served. A worker
/// holds a small map of loaded models rather than a single one, so an engine
/// change within a kind — or a layout request arriving between two
/// recognition requests — is served by the map and costs at most a model load.
/// Restarting for that would throw away every other model in the process to
/// load one, which is exactly the thrash the map exists to avoid: reading a
/// document alternates page detection, page reading and formula reading
/// continuously.
///
/// Two things force a restart here, and both are properties of the process
/// rather than of the model. A kind change, because a kind is what a manager
/// supervises for its whole life and two kinds never share a process. And a
/// [`WorkerProcessKind`] change, because SBERT embedding runs in the Python
/// sidecar and everything else in the Rust worker binary — a running sidecar
/// cannot serve a candle request whatever is in its map. The third, a sidecar
/// in the wrong Python environment, is in `ensure_worker`: it is the one
/// question that is not about the role.
fn should_restart_worker(
    active_process: bool,
    active_role: Option<WorkerRole>,
    req_role: WorkerRole,
) -> bool {
    let serving = |role: WorkerRole| (role.kind(), role.process_kind());
    !active_process || active_role.map(serving) != Some(serving(req_role))
}

/// Deliver the cancel signal to whatever worker is active. Takes the slot
/// rather than `&self` so it can also be called from inside `handle_submit`,
/// where the runtime is busy driving the request being cancelled.
async fn deliver_cancel(slot: &ActiveProcessSlot) {
    let active = slot.lock().unwrap().clone();
    match active {
        Some(proc) => {
            if proc.cancel_active_request().await.is_err() {
                tracing::warn!("WorkerManager: could not deliver cancel signal to the worker");
            }
        }
        None => tracing::debug!("WorkerManager: cancel requested with no active worker"),
    }
}

fn restart_runtime_after_panic(
    active_pid: &Arc<AtomicU32>,
    status: &Arc<RwLock<WorkerStatus>>,
    sender_slot: &Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>,
) -> mpsc::Receiver<ManagerCommand> {
    reset_after_runtime_panic(active_pid, status, sender_slot)
}

impl WorkerRuntime {
    #[allow(clippy::too_many_arguments)]
    fn new(
        paths: WorkerPaths,
        rx: mpsc::Receiver<ManagerCommand>,
        event_tx: mpsc::Sender<ManagerEvent>,
        active_pid: Arc<AtomicU32>,
        active_process_slot: ActiveProcessSlot,
        status: Arc<RwLock<WorkerStatus>>,
        spawner: Arc<dyn WorkerProcessSpawner>,
        residency_kind: Arc<KindResidency>,
        sealed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            paths,
            rx,
            event_tx,
            active_pid,
            active_process_slot,
            status,
            spawner,
            active_process: None,
            active_role: None,
            active_model: None,
            active_device: None,
            active_env: None,
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            deferred: VecDeque::new(),
            residency_kind,
            resident_since: None,
            residency: None,
            sealed,
            last_stop_was_a_death: false,
            replacements: 0,
        }
    }

    async fn run(mut self) {
        loop {
            match self.next_command().await {
                NextCommand::Received(cmd) => self.handle_command(cmd).await,
                NextCommand::ChannelClosed => {
                    self.handle_channel_closed().await;
                    break;
                }
                NextCommand::IdleTimeout => self.handle_idle_timeout().await,
                NextCommand::GiveWay => self.give_way().await,
            }
        }
    }

    async fn next_command(&mut self) -> NextCommand {
        // Commands received while a request held the loop are replayed first,
        // in arrival order, before anything newer off the channel.
        if let Some(cmd) = self.deferred.pop_front() {
            return NextCommand::Received(cmd);
        }
        let give_way = give_way_due(Arc::clone(&self.residency_kind), self.resident_since);
        tokio::select! {
            biased;
            cmd = self.rx.recv() => match cmd {
                Some(cmd) => NextCommand::Received(cmd),
                None => NextCommand::ChannelClosed,
            },
            () = tokio::time::sleep(self.idle_timeout) => NextCommand::IdleTimeout,
            () = give_way => NextCommand::GiveWay,
        }
    }

    /// Whether the permit this manager holds is owed to one waiting behind it.
    fn owes_residency(&self) -> bool {
        self.residency.is_some()
            && self.residency_kind.others_waiting()
            && self
                .resident_since
                .is_some_and(|since| since.elapsed() >= MIN_TENURE_WHILE_OTHERS_WAIT)
    }

    /// Stop the worker so a manager of the same kind in another workspace can
    /// start one. Deliberate, between requests: this manager's next request
    /// queues for the permit behind the one it gave way to.
    async fn give_way(&mut self) {
        if self.active_process.is_some() {
            tracing::info!(
                "WorkerManager: another workspace is waiting for a worker of this kind; \
                 stopping this one after {:?} so it can start.",
                self.resident_since.map(|since| since.elapsed())
            );
            self.clear_active_worker().await;
        }
        self.note_deliberate_stop();
    }

    async fn handle_channel_closed(&mut self) {
        if self.active_process.is_some() {
            tracing::info!("WorkerManager: channel closed, killing worker process.");
            self.clear_active_worker().await;
        }
    }

    async fn handle_idle_timeout(&mut self) {
        if self.active_process.is_some() {
            tracing::info!("WorkerManager: Idle timeout reached, killing worker process.");
            self.clear_active_worker().await;
        }
        self.note_deliberate_stop();
    }

    /// A stop somebody asked for. It ends the run of replacements, so the
    /// next start is a first start rather than the next rung of a loop.
    fn note_deliberate_stop(&mut self) {
        self.last_stop_was_a_death = false;
        self.replacements = 0;
    }

    async fn handle_command(&mut self, cmd: ManagerCommand) {
        match cmd {
            ManagerCommand::ShutdownWorker => {
                if self.active_process.is_some() {
                    tracing::info!("WorkerManager: roof knocking worker process per user request.");
                    self.clear_active_worker().await;
                }
                self.note_deliberate_stop();
            }
            ManagerCommand::SetTimeout(secs) => {
                self.idle_timeout = Duration::from_secs(secs);
                self.update_timeout(secs);
                tracing::info!("WorkerManager: Idle timeout updated to {} seconds.", secs);
            }
            ManagerCommand::CancelActiveRequest => {
                deliver_cancel(&self.active_process_slot).await;
            }
            ManagerCommand::Submit { req, reply } => {
                self.handle_submit(req, reply).await;
            }
        }
    }

    async fn handle_submit(&mut self, req: Box<WorkerRequest>, reply: mpsc::Sender<WorkerEvent>) {
        // The cancel is in force. Answering `Gone` rather than serving this is
        // what the seal is for: every loop that submits already treats a gone
        // worker as terminal, so a caller that was mid-document when the user
        // pressed cancel ends here instead of loading a model to finish work
        // nobody is waiting for. Sealed means sealed for a live worker too —
        // it is being killed as this runs, and feeding it a request would only
        // put a doomed round trip between the user and their cancel.
        if self.sealed.load(Ordering::Acquire) {
            tracing::info!(
                "WorkerManager: refusing a {:?} request: the user cancelled and starts are sealed.",
                req.role
            );
            let _ = reply
                .send(WorkerEvent::Gone(
                    "the user cancelled: this worker is sealed".to_string(),
                ))
                .await;
            return;
        }

        // Nobody is waiting for the answer. A request sits in this channel for
        // as long as the requests ahead of it take, and a worker for it can
        // wait far longer than that for residency; its caller — an HTTP
        // request whose client gave up, say — may be long gone by its turn.
        // Starting a worker for it anyway loads a model nobody will use and
        // holds the kind's one permit while it idles out, barring every other
        // workspace's requests for those minutes. A ReasonIR-8B probe did
        // exactly that an hour after the request that sent it was dropped.
        if reply.is_closed() {
            tracing::info!(
                "WorkerManager: dropping a {:?} request for {:?}: its caller is gone.",
                req.role,
                req.model
            );
            return;
        }

        let req_json = match serialize_request_for_worker(&req) {
            Ok(json) => json,
            Err(e) => {
                let _ = reply
                    .send(WorkerEvent::Error(format!("Serialize error: {e}")))
                    .await;
                return;
            }
        };

        // A request queued behind others never gives this manager a gap of
        // its own, so the turn is also offered here, before the next request
        // is served: otherwise a steady stream of them would hold the permit
        // for as long as the stream lasts.
        if self.owes_residency() {
            self.give_way().await;
        }

        let log_req = req.redacted_for_log();
        tracing::info!(
            "WorkerManager: sending request: {:?}",
            serde_json::to_string(&log_req).unwrap_or_default()
        );

        if self.ensure_worker(&req, &reply).await.is_err() {
            return;
        }

        self.maybe_hot_swap_tracking(&req);

        let Some(proc) = self.active_process.clone() else {
            return;
        };

        // The request runs to its terminal event, but the command channel keeps
        // being served while it does. A cancel is only useful mid-request, and
        // draining it here — rather than after the request returns — is what
        // makes the out-of-band signal reach the worker in time. Every other
        // command is deferred so requests stay serialised: one request owns the
        // worker's stdin/stdout until it finishes.
        let request = proc.start_request(req_json, reply);
        tokio::pin!(request);
        let slot = Arc::clone(&self.active_process_slot);
        let mut channel_closed = false;
        let outcome = loop {
            tokio::select! {
                biased;
                result = &mut request => break result,
                cmd = self.rx.recv(), if !channel_closed => match cmd {
                    Some(ManagerCommand::CancelActiveRequest) => deliver_cancel(&slot).await,
                    Some(other) => self.deferred.push_back(other),
                    // The sender is gone; stop polling it and let the main loop
                    // observe the closure once the request has finished.
                    None => channel_closed = true,
                },
            }
        };

        if outcome.is_err() {
            // Died under the request rather than being stopped: whatever
            // starts the next one is starting a replacement.
            self.last_stop_was_a_death = true;
            self.clear_active_worker().await;
        }
    }

    async fn ensure_worker(
        &mut self,
        req: &WorkerRequest,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let needs_restart =
            should_restart_worker(self.active_process.is_some(), self.active_role, req.role)
                || self.active_env != req.python_env;

        if !needs_restart {
            return Ok(());
        }

        if self.active_process.is_some() {
            tracing::info!(
                "WorkerManager: restarting worker (role: {:?} -> {:?}, model: {:?} -> {:?}, device: {:?} -> {:?})",
                self.active_role,
                req.role,
                self.active_model,
                req.model,
                self.active_device,
                req.device
            );
            self.clear_active_worker().await;
        } else {
            tracing::info!(
                "WorkerManager: starting new worker for role: {:?}, model: {:?}, device: {:?}",
                req.role,
                req.model,
                req.device
            );
        }

        let _ = self.event_tx.send(ManagerEvent::WorkerStarting).await;

        if self.last_stop_was_a_death {
            self.replacements += 1;
            tracing::error!(
                "WorkerManager: starting worker #{} to replace one that died under a request \
                 (role {:?}, model {:?}). A worker started to replace a killed one is a worker \
                 started inside a loop: the caller is submitting per item where it should submit \
                 once, and a cancel will be outrun once per item. See the \"workers are never \
                 started inside a loop\" invariant in AGENTS.md.",
                self.replacements,
                req.role,
                req.model,
            );
        }

        // Taken here rather than at the top of `ensure_worker`, because a
        // restart releases the old worker's permit on its way through
        // `clear_active_worker` above: asking for one before that ran would be
        // this manager queueing behind itself.
        if self.residency.is_none() {
            let admitted = tokio::select! {
                biased;
                admitted = self.residency_kind.admit() => admitted,
                // A cancel cannot wait for another workspace's worker to go.
                () = until_sealed(&self.sealed) => {
                    tracing::info!(
                        "WorkerManager: abandoning a {:?} start while it waited for residency: \
                         the user cancelled.",
                        req.role
                    );
                    let _ = reply
                        .send(WorkerEvent::Gone(
                            "the user cancelled: this worker is sealed".to_string(),
                        ))
                        .await;
                    return Err(());
                }
                // The wait for another workspace's worker to go has no bound
                // of its own, so it ends when the caller stops waiting too —
                // see the same check in `handle_submit`.
                () = reply.closed() => {
                    tracing::info!(
                        "WorkerManager: abandoning a {:?} start for {:?} while it waited for \
                         residency: its caller is gone.",
                        req.role,
                        req.model
                    );
                    return Err(());
                }
            };
            match admitted {
                Ok(permit) => {
                    self.residency = Some(permit);
                    self.resident_since = Some(tokio::time::Instant::now());
                }
                Err(e) => {
                    // The semaphore lives for the process and is never closed,
                    // so this cannot happen — but a worker started without a
                    // permit is one nothing bounds, so it is an error rather
                    // than a start.
                    let message = format!("worker residency unavailable: {e}");
                    tracing::error!("WorkerManager: {message}");
                    let _ = reply.send(WorkerEvent::Error(message)).await;
                    return Err(());
                }
            }
        }

        // A start can be minutes of `pip` building an environment, with no
        // worker yet for a cancel to kill. Dropping the spawn is what stops
        // that: the install runs as a child that dies with it.
        let spawned = tokio::select! {
            biased;
            spawned = self.spawner.spawn(&self.paths, req, &self.active_pid, reply) => spawned,
            () = until_sealed(&self.sealed) => {
                tracing::info!(
                    "WorkerManager: abandoning a {:?} start: the user cancelled.",
                    req.role
                );
                self.residency = None;
                self.resident_since = None;
                let _ = reply
                    .send(WorkerEvent::Gone(
                        "the user cancelled: this worker is sealed".to_string(),
                    ))
                    .await;
                return Err(());
            }
        };

        match spawned {
            Ok(proc) => {
                self.last_stop_was_a_death = false;
                self.active_process = Some(Arc::clone(&proc));
                *self.active_process_slot.lock().unwrap() = Some(proc);
                self.active_role = Some(req.role);
                self.active_model = Some(req.model.clone());
                self.active_device = Some(req.device.clone());
                self.active_env = req.python_env.clone();
                self.update_status_active(req.role, &req.model, &req.device, &req.mode);
                Ok(())
            }
            Err(e) => {
                // Nothing was started, so nothing is holding weights: keeping
                // the permit would bar every other manager of this kind on
                // behalf of a process that does not exist.
                self.residency = None;
                self.resident_since = None;
                let _ = reply.send(WorkerEvent::Error(e)).await;
                Err(())
            }
        }
    }

    fn maybe_hot_swap_tracking(&mut self, req: &WorkerRequest) {
        if self.active_process.is_none() || self.active_role != Some(req.role) {
            return;
        }

        if self.active_model.as_deref() != Some(req.model.as_str())
            || self.active_device.as_deref() != Some(req.device.as_str())
        {
            tracing::info!(
                "WorkerManager: hot-swapping model (model: {:?} -> {:?}, device: {:?} -> {:?})",
                self.active_model,
                req.model,
                self.active_device,
                req.device
            );
            self.active_model = Some(req.model.clone());
            self.active_device = Some(req.device.clone());
            self.update_status_active(req.role, &req.model, &req.device, &req.mode);
        }
    }

    async fn clear_active_worker(&mut self) {
        if let Some(proc) = self.active_process.take() {
            let needs_shutdown = self.active_process_slot.lock().unwrap().take().is_some();
            if needs_shutdown {
                // Every stop the runtime itself decides on happens between
                // requests, where the worker is reading its stdin and the
                // knock is answered. A user cancel does not come through
                // here; it goes straight at the process.
                proc.shutdown(&self.active_pid, Stop::RoofKnock).await;
            }
        }
        self.active_role = None;
        self.active_model = None;
        self.active_device = None;
        self.active_env = None;
        // After the shutdown, never before it: the permit says a process of
        // this kind holds models, and until the stop returns one still does.
        self.residency = None;
        self.resident_since = None;
        self.update_status_idle();
    }

    fn update_status_active(
        &self,
        role: WorkerRole,
        model: &str,
        device: &str,
        request_mode: &str,
    ) {
        if let Ok(mut status) = self.status.write() {
            status.active = true;
            // `role` is owned by the manager, not by the request: see
            // `reset_worker_status`.
            status.engine = Some(role.engine_str().to_string());
            status.model = Some(model.to_string());
            status.device = if role.as_str() == "generate" {
                None
            } else {
                Some(device.to_string())
            };
            status.generation = if role.as_str() == "generate" {
                Some(GenerationWorkerStatus {
                    requested_device: device.to_string(),
                    ..GenerationWorkerStatus::default()
                })
            } else {
                None
            };
            status.request_mode = Some(request_mode.to_string());
            let pid = self.active_pid.load(Ordering::Relaxed);
            status.pid = if pid == 0 { None } else { Some(pid) };
        }
    }

    fn update_status_idle(&self) {
        if let Ok(mut status) = self.status.write() {
            status.active = false;
            status.engine = None;
            status.model = None;
            status.device = None;
            status.request_mode = None;
            status.pid = None;
            status.generation = None;
        }
    }

    fn update_timeout(&self, secs: u64) {
        if let Ok(mut status) = self.status.write() {
            status.timeout_secs = secs;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::GenerationEngine;
    use crate::types::EmbeddingEngine;
    use std::sync::atomic::AtomicUsize;
    use tokio::time::timeout;

    struct FakeSession {
        cancel_calls: Arc<AtomicUsize>,
        send_calls: Arc<AtomicUsize>,
        shutdown_calls: Arc<AtomicUsize>,
        send_should_fail: bool,
    }

    #[async_trait]
    impl WorkerSession for FakeSession {
        fn start_request(
            &self,
            _req_json: String,
            _reply: mpsc::Sender<WorkerEvent>,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> {
            let send_calls = Arc::clone(&self.send_calls);
            let send_should_fail = self.send_should_fail;
            Box::pin(async move {
                send_calls.fetch_add(1, Ordering::Relaxed);
                if send_should_fail {
                    return Err(());
                }
                Ok(())
            })
        }

        async fn shutdown(&self, _pid_slot: &AtomicU32, _stop: Stop) {
            self.shutdown_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn cancel_active_request(&self) -> Result<(), ()> {
            self.cancel_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct FakeSpawner {
        spawn_calls: Arc<AtomicUsize>,
        send_calls: Arc<AtomicUsize>,
        shutdown_calls: Arc<AtomicUsize>,
        spawn_should_fail: bool,
        send_should_fail: bool,
    }

    #[async_trait]
    impl WorkerProcessSpawner for FakeSpawner {
        async fn spawn(
            &self,
            _paths: &WorkerPaths,
            _req: &WorkerRequest,
            _active_pid: &AtomicU32,
            _reply: &mpsc::Sender<WorkerEvent>,
        ) -> Result<Arc<dyn WorkerSession>, String> {
            self.spawn_calls.fetch_add(1, Ordering::Relaxed);
            if self.spawn_should_fail {
                return Err("Failed to spawn worker: fake failure".to_string());
            }
            Ok(Arc::new(FakeSession {
                cancel_calls: Arc::new(AtomicUsize::new(0)),
                send_calls: Arc::clone(&self.send_calls),
                shutdown_calls: Arc::clone(&self.shutdown_calls),
                send_should_fail: self.send_should_fail,
            }))
        }
    }

    type TestRuntime = (
        WorkerRuntime,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        mpsc::Sender<ManagerCommand>,
        mpsc::Receiver<ManagerEvent>,
    );

    fn test_runtime() -> TestRuntime {
        test_runtime_sharing(Arc::new(KindResidency::new(1)))
    }

    /// A runtime admitted by a caller-supplied semaphore, so a test can build
    /// two that contend the way two managers of one kind in two workspaces do.
    fn test_runtime_sharing(residency: Arc<KindResidency>) -> TestRuntime {
        let paths = WorkerPaths {
            python_override: None,
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            uv_bin: std::path::PathBuf::from("venv"),
            worker_bin: std::path::PathBuf::from("worker"),
            data_dir: std::path::PathBuf::from("data"),
        };
        let (_tx, rx) = mpsc::channel(4);
        let (event_tx, event_rx) = mpsc::channel(4);
        let active_pid = Arc::new(AtomicU32::new(0));
        // Seeded the way `WorkerManager::new` seeds it: the role comes from the
        // manager and the runtime only ever fills in the process around it.
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: false,
            role: Some("embed".to_string()),
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
            generation: None,
        }));
        let active_process_slot: ActiveProcessSlot = Arc::new(std::sync::Mutex::new(None));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
            active_process_slot,
            status,
            Arc::new(FakeSpawner {
                spawn_calls: Arc::clone(&spawn_calls),
                send_calls: Arc::clone(&send_calls),
                shutdown_calls: Arc::clone(&shutdown_calls),
                spawn_should_fail: false,
                send_should_fail: false,
            }),
            residency,
            Arc::new(AtomicBool::new(false)),
        );
        (
            runtime,
            spawn_calls,
            send_calls,
            shutdown_calls,
            _tx,
            event_rx,
        )
    }

    fn embed_request(model: &str) -> WorkerRequest {
        WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: model.to_string(),
            device: "cpu".to_string(),
            texts: None,
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        }
    }

    /// The counter that names the loop invariant.
    ///
    /// A caller submitting per item, whose worker was killed under the first
    /// item, submits again for the second. The runtime has no process left, so
    /// it starts one — which is the whole defect, and the thing a cancel has
    /// to outrun once per item. It has to be visible where it happens: the
    /// caller cannot see it, and a progress bar reading "Cancelling..." while
    /// a fresh worker loads its model says nothing about why.
    #[tokio::test]
    async fn a_worker_started_to_replace_one_that_died_is_counted() {
        let (mut runtime, spawn_calls, _send, _shutdown, _tx, _events) = test_runtime();
        // The session fails its request, which is what a killed worker looks
        // like from here.
        runtime.spawner = Arc::new(FakeSpawner {
            spawn_calls: Arc::clone(&spawn_calls),
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::new(AtomicUsize::new(0)),
            spawn_should_fail: false,
            send_should_fail: true,
        });

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            runtime.replacements, 0,
            "the first worker of a run is not a replacement"
        );

        // The next iteration of the caller's loop.
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(
            spawn_calls.load(Ordering::Relaxed),
            2,
            "the second submit had no process and started one"
        );
        assert_eq!(
            runtime.replacements, 1,
            "starting a worker to replace one that died under a request is the violation"
        );
    }

    /// Two managers of one kind cannot hold a worker at the same time.
    ///
    /// This is the whole bound. The host builds a manager per kind per
    /// `AppContext`, and it opens an `AppContext` per workspace anything
    /// reaches — so before this, two workspaces reading at once meant two
    /// recognize workers, each keeping four models resident, and the machine
    /// held two copies of weights that are gigabytes apiece.
    #[tokio::test]
    async fn a_second_manager_of_one_kind_waits_for_the_first_to_let_go() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut first, first_spawns, _s, _sd, _tx, _ev) =
            test_runtime_sharing(Arc::clone(&shared));
        let (mut second, second_spawns, _s2, _sd2, _tx2, _ev2) =
            test_runtime_sharing(Arc::clone(&shared));

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(first_spawns.load(Ordering::Relaxed), 1);

        // The second manager's request while the first still holds its worker.
        // It must not start one: the assertion is that this future does not
        // finish, so it is raced against a timeout rather than awaited.
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        let blocked = second.handle_submit(Box::new(embed_request("model-b")), reply_tx);
        assert!(
            timeout(Duration::from_millis(50), blocked).await.is_err(),
            "the second manager started a worker while the first still held one"
        );
        assert_eq!(
            second_spawns.load(Ordering::Relaxed),
            0,
            "no second worker of this kind may exist"
        );

        // The first worker goes at its idle timeout, which is what releases the
        // permit — and the second is admitted without anything telling it to
        // retry.
        first.handle_idle_timeout().await;
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        assert!(
            timeout(
                Duration::from_secs(5),
                second.handle_submit(Box::new(embed_request("model-b")), reply_tx)
            )
            .await
            .is_ok(),
            "the permit the first worker held was not released when it died"
        );
        assert_eq!(second_spawns.load(Ordering::Relaxed), 1);
    }

    /// A request whose caller has gone is not worth a worker. Its caller is
    /// the one that would read the answer; starting a process for it loads a
    /// model for nobody and holds the kind's permit until the idle timeout.
    #[tokio::test]
    async fn a_request_whose_caller_is_gone_starts_no_worker() {
        let (mut runtime, spawn_calls, _s, _sd, _tx, _ev) = test_runtime();
        let (reply_tx, reply_rx) = mpsc::channel(4);
        drop(reply_rx);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 0);
        assert!(runtime.residency.is_none());
    }

    /// The same, for a caller that leaves while the start waits for another
    /// workspace's worker to let go: the wait ends with it, and when the other
    /// worker does go, nothing starts in its place for a request nobody holds.
    #[tokio::test]
    async fn a_caller_leaving_ends_the_wait_for_residency() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut first, _first_spawns, _s, _sd, _tx, _ev) =
            test_runtime_sharing(Arc::clone(&shared));
        let (mut second, second_spawns, _s2, _sd2, _tx2, _ev2) =
            test_runtime_sharing(Arc::clone(&shared));

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;

        let (reply_tx, reply_rx) = mpsc::channel(4);
        let waiting = tokio::spawn(async move {
            second
                .handle_submit(Box::new(embed_request("model-b")), reply_tx)
                .await;
            second
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the second start did not wait");

        drop(reply_rx);
        let second = timeout(Duration::from_secs(3), waiting)
            .await
            .expect("the wait outlived its caller")
            .unwrap();
        assert_eq!(second_spawns.load(Ordering::Relaxed), 0);
        assert!(second.residency.is_none());

        first.handle_idle_timeout().await;
        assert_eq!(
            shared.available_permits(),
            1,
            "the abandoned start took the permit when the first worker let go"
        );
    }

    /// A worker that keeps being asked for something never reaches its idle
    /// timeout, so the idle timeout alone let one workspace hold the kind for
    /// as long as its requests kept coming. Once another manager waits and
    /// the holder has had its tenure, the holder gives the permit up at its
    /// next gap — and not before the tenure, which is what keeps two
    /// workspaces from trading a model load on every request.
    #[tokio::test(start_paused = true)]
    async fn an_idle_holder_gives_way_once_its_tenure_is_up() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut first, _first_spawns, _s, first_shutdowns, _tx, _ev) =
            test_runtime_sharing(Arc::clone(&shared));
        let (mut second, second_spawns, _s2, _sd2, _tx2, _ev2) =
            test_runtime_sharing(Arc::clone(&shared));

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        let held_from = tokio::time::Instant::now();

        // Nobody waits: the holder idles on, with no reason to stop early.
        assert!(
            timeout(Duration::from_secs(60), first.next_command())
                .await
                .is_err(),
            "a holder nobody is waiting behind gave way"
        );

        let (reply_tx, _reply_rx2) = mpsc::channel(4);
        let waiting = tokio::spawn(async move {
            second
                .handle_submit(Box::new(embed_request("model-b")), reply_tx)
                .await;
            second
        });
        tokio::task::yield_now().await;

        let next = timeout(Duration::from_secs(120), first.next_command())
            .await
            .expect("a holder someone waits behind gives way well before its idle timeout");
        assert!(matches!(next, NextCommand::GiveWay));
        assert!(held_from.elapsed() >= MIN_TENURE_WHILE_OTHERS_WAIT);
        first.give_way().await;
        assert_eq!(first_shutdowns.load(Ordering::Relaxed), 1);
        assert!(first.residency.is_none());

        let _second = timeout(Duration::from_secs(3), waiting)
            .await
            .expect("the waiting manager was admitted")
            .unwrap();
        assert_eq!(second_spawns.load(Ordering::Relaxed), 1);
    }

    /// The same turn, for a holder whose requests are back to back and so
    /// never has an idle gap: it gives way before serving its next request,
    /// and that request queues for the permit behind the manager it gave way
    /// to.
    #[tokio::test(start_paused = true)]
    async fn a_busy_holder_gives_way_between_requests() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut first, first_spawns, _s, first_shutdowns, _tx, _ev) =
            test_runtime_sharing(Arc::clone(&shared));
        let (mut second, second_spawns, _s2, _sd2, _tx2, _ev2) =
            test_runtime_sharing(Arc::clone(&shared));

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;

        let (reply_tx, _reply_rx2) = mpsc::channel(4);
        let waiting = tokio::spawn(async move {
            second
                .handle_submit(Box::new(embed_request("model-b")), reply_tx)
                .await;
            second
        });
        tokio::task::yield_now().await;

        // Within the tenure, the holder's next request is served as usual.
        let (reply_tx, _reply_rx3) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(first_shutdowns.load(Ordering::Relaxed), 0);
        assert!(!waiting.is_finished());

        tokio::time::sleep(MIN_TENURE_WHILE_OTHERS_WAIT).await;
        let (reply_tx, _reply_rx4) = mpsc::channel(4);
        let first_next = tokio::spawn(async move {
            first
                .handle_submit(Box::new(embed_request("model-a")), reply_tx)
                .await;
            first
        });
        let mut second = timeout(Duration::from_secs(3), waiting)
            .await
            .expect("the holder gave way before its next request")
            .unwrap();
        assert_eq!(first_shutdowns.load(Ordering::Relaxed), 1);
        assert_eq!(second_spawns.load(Ordering::Relaxed), 1);
        assert!(
            !first_next.is_finished(),
            "the holder's next request waits its turn behind the one it gave way to"
        );

        second.handle_idle_timeout().await;
        let _first = timeout(Duration::from_secs(3), first_next)
            .await
            .expect("the holder's request was served once its turn came")
            .unwrap();
        assert_eq!(first_spawns.load(Ordering::Relaxed), 2);
    }

    /// A user's cancel cannot wait behind another workspace's worker.
    #[tokio::test(start_paused = true)]
    async fn a_cancel_ends_the_wait_for_residency() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut first, _f, _s, _sd, _tx, _ev) = test_runtime_sharing(Arc::clone(&shared));
        let (mut second, second_spawns, _s2, _sd2, _tx2, _ev2) =
            test_runtime_sharing(Arc::clone(&shared));
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        first
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;

        let sealed = Arc::clone(&second.sealed);
        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        let waiting = tokio::spawn(async move {
            second
                .handle_submit(Box::new(embed_request("model-b")), reply_tx)
                .await;
        });
        tokio::task::yield_now().await;
        sealed.store(true, Ordering::Release);
        timeout(Duration::from_secs(3), waiting)
            .await
            .expect("a cancel has three seconds")
            .unwrap();
        assert!(matches!(reply_rx.recv().await, Some(WorkerEvent::Gone(_))));
        assert_eq!(second_spawns.load(Ordering::Relaxed), 0);
    }

    /// A spawn that fails started nothing, so it must hold nothing. Otherwise
    /// one manager that cannot start its worker bars every other manager of
    /// that kind for the life of the process.
    #[tokio::test]
    async fn a_worker_that_failed_to_start_holds_no_residency() {
        let shared = Arc::new(KindResidency::new(1));
        let (mut runtime, spawn_calls, _s, _sd, _tx, _ev) =
            test_runtime_sharing(Arc::clone(&shared));
        runtime.spawner = Arc::new(FakeSpawner {
            spawn_calls: Arc::clone(&spawn_calls),
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::new(AtomicUsize::new(0)),
            spawn_should_fail: true,
            send_should_fail: false,
        });

        let (reply_tx, _reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;

        assert!(runtime.residency.is_none());
        assert_eq!(
            shared.available_permits(),
            1,
            "a spawn that failed left the kind barred"
        );
    }

    /// The seal is the half of a cancel that makes the kill final. A caller
    /// that was already mid-document when the user pressed cancel submits
    /// once more on its way out; if that submit started a process, the user
    /// would have killed a worker and got a fresh one loading the same model
    /// back.
    /// A spawn that never returns, which is what a start looks like from here
    /// while `pip` is building an environment.
    struct StuckSpawner;

    #[async_trait]
    impl WorkerProcessSpawner for StuckSpawner {
        async fn spawn(
            &self,
            _paths: &WorkerPaths,
            _req: &WorkerRequest,
            _active_pid: &AtomicU32,
            _reply: &mpsc::Sender<WorkerEvent>,
        ) -> Result<Arc<dyn WorkerSession>, String> {
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancel_abandons_a_start_still_building_its_environment() {
        let (mut runtime, _spawn_calls, _send, _shutdown, _tx, _events) = test_runtime();
        runtime.spawner = Arc::new(StuckSpawner);
        let sealed = Arc::clone(&runtime.sealed);
        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        let submit = tokio::spawn(async move {
            runtime
                .handle_submit(Box::new(embed_request("model-a")), reply_tx)
                .await;
            runtime
        });
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!submit.is_finished(), "the start ended by itself");

        sealed.store(true, Ordering::Release);

        let runtime = timeout(Duration::from_secs(3), submit)
            .await
            .expect("a cancel has three seconds")
            .unwrap();
        assert!(matches!(reply_rx.recv().await, Some(WorkerEvent::Gone(_))));
        assert!(
            runtime.residency.is_none(),
            "an abandoned start kept the permit of a process that never existed"
        );
    }

    #[tokio::test]
    async fn a_worker_in_another_python_environment_is_replaced() {
        let (mut runtime, spawn_calls, _send, shutdown_calls, _tx, _events) = test_runtime();
        let (reply_tx, _reply_rx) = mpsc::channel(8);
        let other_env = EnvNeeds {
            packages: ["timm".to_string()].into_iter().collect(),
            ..EnvNeeds::default()
        };
        let mut on_series = embed_request("model-b");
        on_series.python_env = Some(other_env.clone());

        runtime
            .ensure_worker(&embed_request("model-a"), &reply_tx)
            .await
            .unwrap();
        // A model that shares the environment shares the worker.
        runtime
            .ensure_worker(&embed_request("model-c"), &reply_tx)
            .await
            .unwrap();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);

        runtime.ensure_worker(&on_series, &reply_tx).await.unwrap();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 2);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.active_env, Some(other_env));

        runtime.ensure_worker(&on_series, &reply_tx).await.unwrap();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 2);

        runtime
            .ensure_worker(&embed_request("model-a"), &reply_tx)
            .await
            .unwrap();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 3);
        assert_eq!(runtime.active_env, None);
    }

    #[tokio::test]
    async fn a_sealed_runtime_starts_nothing_and_answers_gone() {
        let (mut runtime, spawn_calls, _send, _shutdown, _tx, _events) = test_runtime();
        runtime.sealed.store(true, Ordering::Release);

        let (reply_tx, mut reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;

        assert_eq!(
            spawn_calls.load(Ordering::Relaxed),
            0,
            "a sealed manager started a worker"
        );
        // Gone rather than Error: every loop that submits already treats a
        // gone worker as terminal, and an Error is this item's failure with
        // the next one worth attempting.
        assert!(
            matches!(reply_rx.recv().await, Some(WorkerEvent::Gone(_))),
            "a sealed manager must answer as a worker that is not coming back"
        );
    }

    /// Sealing is not permanent. The next job the user asks for is served, and
    /// nothing has to remember to reset anything but the seal itself.
    #[tokio::test]
    async fn an_unsealed_runtime_serves_again() {
        let (mut runtime, spawn_calls, _send, _shutdown, _tx, _events) = test_runtime();
        runtime.sealed.store(true, Ordering::Release);
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 0);

        runtime.sealed.store(false, Ordering::Release);
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        runtime
            .handle_submit(Box::new(embed_request("model-a")), reply_tx)
            .await;
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
    }

    /// A stop somebody asked for ends the run. The next worker after an idle
    /// timeout or an explicit shutdown is a first worker, not the next rung of
    /// a loop, and counting it would cry wolf on every ordinary reuse.
    #[tokio::test]
    async fn a_deliberate_stop_ends_the_run_of_replacements() {
        let (mut runtime, _spawn, _send, _shutdown, _tx, _events) = test_runtime();
        runtime.last_stop_was_a_death = true;
        runtime.replacements = 3;

        runtime.handle_command(ManagerCommand::ShutdownWorker).await;

        assert!(!runtime.last_stop_was_a_death);
        assert_eq!(runtime.replacements, 0);
    }

    /// A session whose request only ends once the worker has been told to
    /// cancel — the shape of a generation stream.
    struct BlockingSession {
        until_cancelled: Arc<tokio::sync::Notify>,
        cancel_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WorkerSession for BlockingSession {
        fn start_request(
            &self,
            _req_json: String,
            _reply: mpsc::Sender<WorkerEvent>,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>> {
            let until_cancelled = Arc::clone(&self.until_cancelled);
            Box::pin(async move {
                until_cancelled.notified().await;
                Ok(())
            })
        }

        async fn shutdown(&self, _pid_slot: &AtomicU32, _stop: Stop) {}

        async fn cancel_active_request(&self) -> Result<(), ()> {
            self.cancel_calls.fetch_add(1, Ordering::Relaxed);
            self.until_cancelled.notify_one();
            Ok(())
        }
    }

    struct BlockingSpawner {
        session: Arc<BlockingSession>,
    }

    #[async_trait]
    impl WorkerProcessSpawner for BlockingSpawner {
        async fn spawn(
            &self,
            _paths: &WorkerPaths,
            _req: &WorkerRequest,
            _active_pid: &AtomicU32,
            _reply: &mpsc::Sender<WorkerEvent>,
        ) -> Result<Arc<dyn WorkerSession>, String> {
            Ok(Arc::clone(&self.session) as Arc<dyn WorkerSession>)
        }
    }

    /// Regression test for a cancel that could never arrive in time.
    ///
    /// `handle_submit` used to await the request to its terminal event before
    /// returning to the command loop, so `CancelActiveRequest` — which travels
    /// through that same loop — was only handled once generation had already
    /// finished. Here the request *only* ends when the cancel reaches the
    /// worker: if the loop stops serving commands mid-request, this deadlocks.
    #[tokio::test]
    async fn cancel_reaches_the_worker_while_the_request_is_still_running() {
        let (mut runtime, _spawn_calls, _send_calls, _shutdown_calls, tx, _event_rx) =
            test_runtime();
        let cancel_calls = Arc::new(AtomicUsize::new(0));
        let session = Arc::new(BlockingSession {
            until_cancelled: Arc::new(tokio::sync::Notify::new()),
            cancel_calls: Arc::clone(&cancel_calls),
        });
        runtime.spawner = Arc::new(BlockingSpawner {
            session: Arc::clone(&session),
        });

        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Generate(GenerationEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: None,
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "generate".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);
        let (timeout_done_tx, timeout_done_rx) = tokio::sync::oneshot::channel();

        let loop_handle = tokio::spawn(async move {
            runtime.run().await;
            let _ = timeout_done_tx.send(());
        });

        tx.send(ManagerCommand::Submit {
            req: Box::new(req),
            reply: reply_tx,
        })
        .await
        .unwrap();
        tx.send(ManagerCommand::CancelActiveRequest).await.unwrap();
        // Deferred until the request ends; proves ordering is preserved too.
        tx.send(ManagerCommand::SetTimeout(23)).await.unwrap();
        drop(tx);

        timeout(Duration::from_secs(5), timeout_done_rx)
            .await
            .expect("the command loop never served the cancel")
            .unwrap();
        loop_handle.await.unwrap();

        assert_eq!(cancel_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_handle_command_set_timeout_updates_status() {
        let (mut runtime, _spawn_calls, _send_calls, _shutdown_calls, _tx, _event_rx) =
            test_runtime();

        runtime.handle_command(ManagerCommand::SetTimeout(17)).await;

        assert_eq!(runtime.status.read().unwrap().timeout_secs, 17);
    }

    #[test]
    fn test_reset_helpers_clear_status_and_swap_sender() {
        let active_pid = Arc::new(AtomicU32::new(44));
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: true,
            role: Some("embed".to_string()),
            engine: Some("candle".to_string()),
            model: Some("model-a".to_string()),
            device: Some("cpu".to_string()),
            request_mode: Some("embed".to_string()),
            pid: Some(44),
            timeout_secs: 300,
            generation: None,
        }));
        let (old_tx, _old_rx) = mpsc::channel(1);
        let sender_slot = Arc::new(std::sync::Mutex::new(old_tx));

        let _new_rx = reset_after_runtime_panic(&active_pid, &status, &sender_slot);

        assert_eq!(active_pid.load(Ordering::Relaxed), 0);
        let status = status.read().unwrap();
        assert!(!status.active);
        assert!(status.engine.is_none());
        assert!(status.model.is_none());
        // The role is the manager's identity, not the dead process's.
        assert_eq!(status.role.as_deref(), Some("embed"));
    }

    #[test]
    fn test_restart_runtime_after_panic_resets_state() {
        let active_pid = Arc::new(AtomicU32::new(12));
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: true,
            role: Some("embed".to_string()),
            engine: Some("candle".to_string()),
            model: Some("model-a".to_string()),
            device: Some("cpu".to_string()),
            request_mode: Some("embed".to_string()),
            pid: Some(12),
            timeout_secs: 300,
            generation: None,
        }));
        let (old_tx, _old_rx) = mpsc::channel(1);
        let sender_slot = Arc::new(std::sync::Mutex::new(old_tx));

        let _new_rx = restart_runtime_after_panic(&active_pid, &status, &sender_slot);

        assert_eq!(active_pid.load(Ordering::Relaxed), 0);
        let status = status.read().unwrap();
        assert!(!status.active);
        assert!(status.engine.is_none());
        assert!(status.model.is_none());
    }

    #[test]
    fn test_should_restart_worker_uses_active_session_and_kind() {
        let candle = WorkerRole::Embed(EmbeddingEngine::Candle);
        let sbert = WorkerRole::Embed(EmbeddingEngine::SBERT);
        let generate = WorkerRole::Generate(GenerationEngine::Candle);

        assert!(should_restart_worker(false, None, candle));
        assert!(!should_restart_worker(true, Some(candle), candle));

        // Same kind and the same binary: served by the worker's model map, at
        // the cost of a load. Restarting would drop every other model in that
        // process to load one.
        let fastembed = WorkerRole::Embed(EmbeddingEngine::Fastembed);
        assert!(!should_restart_worker(true, Some(candle), fastembed));

        // Same kind, different process: SBERT is the Python sidecar, and no
        // model swap inside a running candle worker can produce one.
        assert!(should_restart_worker(true, Some(candle), sbert));
        assert!(should_restart_worker(true, Some(sbert), candle));

        // The reason generation needs its own manager: a shared one would
        // evict a multi-gigabyte model on every alternation.
        assert!(should_restart_worker(true, Some(candle), generate));
        assert!(should_restart_worker(true, Some(generate), candle));
        assert!(!should_restart_worker(true, Some(generate), generate));
    }

    /// Two engines that run in the same binary are a model swap inside one
    /// process, not a new process. The worker holds both; nothing is killed.
    #[tokio::test]
    async fn test_ensure_worker_reuses_one_process_across_engines_it_can_serve() {
        let (mut runtime, spawn_calls, _send_calls, shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let first = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let second = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::Fastembed),
            ..first.clone()
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&first, &reply_tx).await.unwrap();
        runtime.ensure_worker(&second, &reply_tx).await.unwrap();

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 0);
    }

    /// Detection and recognition alternate page by page within one document.
    /// They share a kind precisely so that alternation costs nothing: a
    /// restart here would reload the page reader for every page of the book.
    #[test]
    fn test_layout_and_recognition_share_one_worker_process() {
        use crate::extract::image::dispatch::RecognitionEngine;
        let recognize = WorkerRole::Recognize(RecognitionEngine::Onnx);
        let layout = WorkerRole::Layout(RecognitionEngine::Onnx);

        assert!(!should_restart_worker(true, Some(recognize), layout));
        assert!(!should_restart_worker(true, Some(layout), recognize));
        assert!(should_restart_worker(
            true,
            Some(WorkerRole::Embed(EmbeddingEngine::Candle)),
            layout
        ));
    }

    #[test]
    fn test_serialize_request_for_worker_round_trips_json() {
        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };

        let json = serialize_request_for_worker(&req).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["mode"], "embed");
        assert_eq!(value["model"], "model-a");
        assert_eq!(value["device"], "cpu");
        assert_eq!(value["texts"][0], "hello");
    }

    #[tokio::test]
    async fn test_handle_command_kill_worker_clears_active_process() {
        let (mut runtime, _spawn_calls, _send_calls, shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let proc: Arc<dyn WorkerSession> = Arc::new(FakeSession {
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        });
        *runtime.active_process_slot.lock().unwrap() = Some(Arc::clone(&proc));
        runtime.active_process = Some(proc);
        runtime.active_role = Some(WorkerRole::Embed(EmbeddingEngine::Candle));
        runtime.active_model = Some("model-a".to_string());
        runtime.active_device = Some("cpu".to_string());

        runtime.handle_command(ManagerCommand::ShutdownWorker).await;

        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert!(runtime.active_process.is_none());
    }

    #[tokio::test]
    async fn test_ensure_worker_and_hot_swap_reuses_session() {
        let (mut runtime, spawn_calls, send_calls, shutdown_calls, _tx, mut event_rx) =
            test_runtime();
        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, mut reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&req, &reply_tx).await.unwrap();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            runtime.status.read().unwrap().engine.as_deref(),
            Some("candle")
        );
        assert_eq!(
            runtime.status.read().unwrap().model.as_deref(),
            Some("model-a")
        );
        assert!(matches!(
            event_rx.recv().await,
            Some(ManagerEvent::WorkerStarting)
        ));

        runtime.maybe_hot_swap_tracking(&WorkerRequest {
            model: "model-b".to_string(),
            device: "gpu".to_string(),
            ..req.clone()
        });

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        assert_eq!(send_calls.load(Ordering::Relaxed), 0);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            runtime.status.read().unwrap().model.as_deref(),
            Some("model-b")
        );
        assert_eq!(
            runtime.status.read().unwrap().engine.as_deref(),
            Some("candle")
        );
        assert!(reply_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_ensure_worker_spawn_failure_sends_error() {
        let paths = WorkerPaths {
            python_override: None,
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            uv_bin: std::path::PathBuf::from("venv"),
            worker_bin: std::path::PathBuf::from("worker"),
            data_dir: std::path::PathBuf::from("data"),
        };
        let (_tx, rx) = mpsc::channel(4);
        let (event_tx, _event_rx) = mpsc::channel(4);
        let active_pid = Arc::new(AtomicU32::new(0));
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: false,
            role: None,
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
            generation: None,
        }));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
            Arc::new(std::sync::Mutex::new(None)),
            status,
            Arc::new(FakeSpawner {
                spawn_calls: Arc::clone(&spawn_calls),
                send_calls: Arc::clone(&send_calls),
                shutdown_calls: Arc::clone(&shutdown_calls),
                spawn_should_fail: true,
                send_should_fail: false,
            }),
            Arc::new(KindResidency::new(1)),
            Arc::new(AtomicBool::new(false)),
        );

        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, mut reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&req, &reply_tx).await.unwrap_err();
        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        match reply_rx.recv().await {
            Some(WorkerEvent::Error(msg)) => assert!(msg.contains("fake failure")),
            other => panic!("expected worker error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_ensure_worker_reuses_existing_process_without_restart() {
        let (mut runtime, spawn_calls, _send_calls, _shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&req, &reply_tx).await.unwrap();
        runtime.ensure_worker(&req, &reply_tx).await.unwrap();

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
    }

    /// SBERT runs in the Python sidecar and candle in the Rust worker binary,
    /// so this switch is a different process and not a different model: the
    /// map inside the running one cannot serve it whatever it holds.
    #[tokio::test]
    async fn test_ensure_worker_restart_clears_previous_process() {
        let (mut runtime, spawn_calls, _send_calls, shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let first = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let second = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::SBERT),
            ..first.clone()
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&first, &reply_tx).await.unwrap();
        runtime.ensure_worker(&second, &reply_tx).await.unwrap();

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 2);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            runtime.active_role,
            Some(WorkerRole::Embed(EmbeddingEngine::SBERT))
        );
    }

    #[test]
    fn test_maybe_hot_swap_tracking_returns_without_active_process() {
        let (mut runtime, _spawn_calls, _send_calls, _shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };

        runtime.maybe_hot_swap_tracking(&req);

        assert!(runtime.active_model.is_none());
    }

    #[tokio::test]
    async fn test_handle_submit_send_failure_clears_active_worker() {
        let paths = WorkerPaths {
            python_override: None,
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            uv_bin: std::path::PathBuf::from("venv"),
            worker_bin: std::path::PathBuf::from("worker"),
            data_dir: std::path::PathBuf::from("data"),
        };
        let (_tx, rx) = mpsc::channel(4);
        let (event_tx, _event_rx) = mpsc::channel(4);
        let active_pid = Arc::new(AtomicU32::new(0));
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: false,
            role: None,
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
            generation: None,
        }));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
            Arc::new(std::sync::Mutex::new(None)),
            status,
            Arc::new(FakeSpawner {
                spawn_calls: Arc::clone(&spawn_calls),
                send_calls: Arc::clone(&send_calls),
                shutdown_calls: Arc::clone(&shutdown_calls),
                spawn_should_fail: false,
                send_should_fail: true,
            }),
            Arc::new(KindResidency::new(1)),
            Arc::new(AtomicBool::new(false)),
        );

        let req = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
            python_env: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, mut reply_rx) = mpsc::channel(4);

        runtime.handle_submit(Box::new(req), reply_tx).await;

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
        assert_eq!(send_calls.load(Ordering::Relaxed), 1);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert!(reply_rx.try_recv().is_err());
        assert!(!runtime.status.read().unwrap().active);
    }

    #[tokio::test]
    async fn test_run_channel_closed_clears_active_worker() {
        let (mut runtime, _spawn_calls, _send_calls, shutdown_calls, tx, _event_rx) =
            test_runtime();
        let status = Arc::clone(&runtime.status);
        let proc: Arc<dyn WorkerSession> = Arc::new(FakeSession {
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        });
        *runtime.active_process_slot.lock().unwrap() = Some(Arc::clone(&proc));
        runtime.active_process = Some(proc);
        runtime.active_role = Some(WorkerRole::Embed(EmbeddingEngine::Candle));
        runtime.active_model = Some("model-a".to_string());
        runtime.active_device = Some("cpu".to_string());
        drop(tx);

        runtime.run().await;

        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert!(!status.read().unwrap().active);
    }

    #[tokio::test]
    async fn test_run_idle_timeout_clears_active_worker() {
        let (mut runtime, _spawn_calls, _send_calls, shutdown_calls, tx, _event_rx) =
            test_runtime();
        let status = Arc::clone(&runtime.status);
        let proc: Arc<dyn WorkerSession> = Arc::new(FakeSession {
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        });
        *runtime.active_process_slot.lock().unwrap() = Some(Arc::clone(&proc));
        runtime.active_process = Some(proc);
        runtime.active_role = Some(WorkerRole::Embed(EmbeddingEngine::Candle));
        runtime.active_model = Some("model-a".to_string());
        runtime.active_device = Some("cpu".to_string());
        runtime.idle_timeout = std::time::Duration::from_millis(20);

        let handle = tokio::spawn(runtime.run());
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        drop(tx);
        handle.await.unwrap();
        let status = status.read().unwrap();
        assert!(!status.active);
        // Going idle unloads the model, not the manager's identity.
        assert_eq!(status.role.as_deref(), Some("embed"));
    }
}
