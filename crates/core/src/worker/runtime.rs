use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::{future::Future, pin::Pin};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ipc::{CancelSignal, WorkerEvent, WorkerRequest, WorkerRole};
use super::manager::{
    GenerationWorkerStatus, ManagerCommand, ManagerEvent, WorkerPaths, WorkerStatus,
};
use super::process::WorkerProcess;
use super::DEFAULT_IDLE_TIMEOUT_SECS;

#[async_trait]
pub(crate) trait WorkerSession: Send + Sync {
    fn start_request(
        &self,
        req_json: String,
        reply: mpsc::Sender<WorkerEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;

    async fn shutdown(&self, pid_slot: &AtomicU32);

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
    ) -> Result<Arc<dyn WorkerSession>, String> {
        let proc = WorkerProcess::spawn(paths, req, active_pid).await?;
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

    async fn shutdown(&self, pid_slot: &AtomicU32) {
        WorkerProcess::shutdown(self, pid_slot).await;
    }

    async fn cancel_active_request(&self) -> Result<(), ()> {
        let line = serde_json::to_string(&CancelSignal { cancel: true })
            .expect("CancelSignal serialization cannot fail");
        WorkerProcess::send_out_of_band(self, &line).await
    }
}

pub(super) async fn supervised_manager_loop(
    paths: WorkerPaths,
    initial_rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    active_process: ActiveProcessSlot,
    sender_slot: Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>,
    status: Arc<RwLock<WorkerStatus>>,
) {
    let mut rx = initial_rx;
    let spawner: Arc<dyn WorkerProcessSpawner> = Arc::new(RealWorkerProcessSpawner);
    loop {
        let runtime = WorkerRuntime::new(
            paths.clone(),
            rx,
            event_tx.clone(),
            Arc::clone(&active_pid),
            Arc::clone(&active_process),
            Arc::clone(&status),
            Arc::clone(&spawner),
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
    idle_timeout: Duration,
    /// Commands taken off the channel while a request was in flight, held back
    /// until it finished. Only the cancel signal is acted on mid-request.
    deferred: VecDeque<ManagerCommand>,
}

enum NextCommand {
    Received(ManagerCommand),
    ChannelClosed,
    IdleTimeout,
}

fn serialize_request_for_worker(req: &WorkerRequest) -> Result<String, String> {
    serde_json::to_string(req).map_err(|e| format!("Serialize error: {e}"))
}

fn should_restart_worker(
    active_process: bool,
    active_role: Option<WorkerRole>,
    req_role: WorkerRole,
) -> bool {
    !active_process || active_role != Some(req_role)
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
    fn new(
        paths: WorkerPaths,
        rx: mpsc::Receiver<ManagerCommand>,
        event_tx: mpsc::Sender<ManagerEvent>,
        active_pid: Arc<AtomicU32>,
        active_process_slot: ActiveProcessSlot,
        status: Arc<RwLock<WorkerStatus>>,
        spawner: Arc<dyn WorkerProcessSpawner>,
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
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            deferred: VecDeque::new(),
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
            }
        }
    }

    async fn next_command(&mut self) -> NextCommand {
        // Commands received while a request held the loop are replayed first,
        // in arrival order, before anything newer off the channel.
        if let Some(cmd) = self.deferred.pop_front() {
            return NextCommand::Received(cmd);
        }
        match timeout(self.idle_timeout, self.rx.recv()).await {
            Ok(Some(cmd)) => NextCommand::Received(cmd),
            Ok(None) => NextCommand::ChannelClosed,
            Err(_) => NextCommand::IdleTimeout,
        }
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
    }

    async fn handle_command(&mut self, cmd: ManagerCommand) {
        match cmd {
            ManagerCommand::ShutdownWorker => {
                if self.active_process.is_some() {
                    tracing::info!("WorkerManager: roof knocking worker process per user request.");
                    self.clear_active_worker().await;
                }
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
        let req_json = match serialize_request_for_worker(&req) {
            Ok(json) => json,
            Err(e) => {
                let _ = reply
                    .send(WorkerEvent::Error(format!("Serialize error: {e}")))
                    .await;
                return;
            }
        };

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
            self.clear_active_worker().await;
        }
    }

    async fn ensure_worker(
        &mut self,
        req: &WorkerRequest,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let needs_restart =
            should_restart_worker(self.active_process.is_some(), self.active_role, req.role);

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

        match self.spawner.spawn(&self.paths, req, &self.active_pid).await {
            Ok(proc) => {
                self.active_process = Some(Arc::clone(&proc));
                *self.active_process_slot.lock().unwrap() = Some(proc);
                self.active_role = Some(req.role);
                self.active_model = Some(req.model.clone());
                self.active_device = Some(req.device.clone());
                self.update_status_active(req.role, &req.model, &req.device, &req.mode);
                Ok(())
            }
            Err(e) => {
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
                proc.shutdown(&self.active_pid).await;
            }
        }
        self.active_role = None;
        self.active_model = None;
        self.active_device = None;
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

        async fn shutdown(&self, _pid_slot: &AtomicU32) {
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

    fn test_runtime() -> (
        WorkerRuntime,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        mpsc::Sender<ManagerCommand>,
        mpsc::Receiver<ManagerEvent>,
    ) {
        let paths = WorkerPaths {
            python_path: std::path::PathBuf::from("python"),
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            venv_dir: std::path::PathBuf::from("venv"),
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

        async fn shutdown(&self, _pid_slot: &AtomicU32) {}

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
            role: WorkerRole::Generate(GenerationEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: None,
            generate: None,
            recognize: None,
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
    fn test_should_restart_worker_uses_active_session_and_role() {
        let candle = WorkerRole::Embed(EmbeddingEngine::Candle);
        let sbert = WorkerRole::Embed(EmbeddingEngine::SBERT);
        let generate = WorkerRole::Generate(GenerationEngine::Candle);

        assert!(should_restart_worker(false, None, candle));
        assert!(should_restart_worker(true, Some(candle), sbert));
        assert!(!should_restart_worker(true, Some(candle), candle));

        // The reason generation needs its own manager: a shared one would
        // evict a multi-gigabyte model on every alternation.
        assert!(should_restart_worker(true, Some(candle), generate));
        assert!(should_restart_worker(true, Some(generate), candle));
        assert!(!should_restart_worker(true, Some(generate), generate));
    }

    #[test]
    fn test_serialize_request_for_worker_round_trips_json() {
        let req = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
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
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
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
            python_path: std::path::PathBuf::from("python"),
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            venv_dir: std::path::PathBuf::from("venv"),
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
        );

        let req = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
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
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&req, &reply_tx).await.unwrap();
        runtime.ensure_worker(&req, &reply_tx).await.unwrap();

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_ensure_worker_restart_clears_previous_process() {
        let (mut runtime, spawn_calls, _send_calls, shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let first = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
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
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
            mode: "embed".to_string(),
            model_dir: std::path::PathBuf::from("data"),
        };

        runtime.maybe_hot_swap_tracking(&req);

        assert!(runtime.active_model.is_none());
    }

    #[tokio::test]
    async fn test_handle_submit_send_failure_clears_active_worker() {
        let paths = WorkerPaths {
            python_path: std::path::PathBuf::from("python"),
            python_package_dir: std::path::PathBuf::from("pkg"),
            requirements_path: std::path::PathBuf::from("reqs.txt"),
            venv_dir: std::path::PathBuf::from("venv"),
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
        );

        let req = WorkerRequest {
            role: WorkerRole::Embed(EmbeddingEngine::Candle),
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            generate: None,
            recognize: None,
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
