use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ipc::{WorkerEvent, WorkerRequest};
use super::manager::{ManagerCommand, ManagerEvent, WorkerPaths, WorkerStatus};
use super::process::WorkerProcess;
use crate::types::EmbeddingEngine;

#[async_trait]
pub(crate) trait WorkerSession: Send {
    async fn send_request(
        &mut self,
        req_json: &str,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()>;

    async fn shutdown(&mut self, pid_slot: &AtomicU32);
}

#[async_trait]
trait WorkerProcessSpawner: Send + Sync {
    async fn spawn(
        &self,
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
    ) -> Result<Box<dyn WorkerSession>, String>;
}

struct RealWorkerProcessSpawner;

#[async_trait]
impl WorkerProcessSpawner for RealWorkerProcessSpawner {
    async fn spawn(
        &self,
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
    ) -> Result<Box<dyn WorkerSession>, String> {
        let proc = WorkerProcess::spawn(paths, req, active_pid).await?;
        Ok(Box::new(proc))
    }
}

#[async_trait]
impl WorkerSession for WorkerProcess {
    async fn send_request(
        &mut self,
        req_json: &str,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        WorkerProcess::send_request(self, req_json, reply).await
    }

    async fn shutdown(&mut self, pid_slot: &AtomicU32) {
        WorkerProcess::shutdown(self, pid_slot).await;
    }
}

pub(super) async fn supervised_manager_loop(
    paths: WorkerPaths,
    initial_rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
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
    status: Arc<RwLock<WorkerStatus>>,
    spawner: Arc<dyn WorkerProcessSpawner>,
    active_process: Option<Box<dyn WorkerSession>>,
    active_engine: Option<EmbeddingEngine>,
    active_model: Option<String>,
    active_device: Option<String>,
    idle_timeout: Duration,
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
    active_engine: Option<EmbeddingEngine>,
    req_engine: EmbeddingEngine,
) -> bool {
    !active_process || active_engine != Some(req_engine)
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
        status: Arc<RwLock<WorkerStatus>>,
        spawner: Arc<dyn WorkerProcessSpawner>,
    ) -> Self {
        Self {
            paths,
            rx,
            event_tx,
            active_pid,
            status,
            spawner,
            active_process: None,
            active_engine: None,
            active_model: None,
            active_device: None,
            idle_timeout: Duration::from_secs(300),
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

        let mut log_req = req.clone();
        log_req.texts = None;
        tracing::info!(
            "WorkerManager: sending request: {:?}",
            serde_json::to_string(&log_req).unwrap_or_default()
        );

        if self.ensure_worker(&req, &reply).await.is_err() {
            return;
        }

        self.maybe_hot_swap_tracking(&req);

        if let Some(proc) = self.active_process.as_mut() {
            if proc.send_request(&req_json, &reply).await.is_err() {
                proc.shutdown(&self.active_pid).await;
                self.active_process = None;
                self.update_status_idle();
            }
        }
    }

    async fn ensure_worker(
        &mut self,
        req: &WorkerRequest,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let needs_restart = should_restart_worker(
            self.active_process.is_some(),
            self.active_engine,
            req.engine,
        );

        if !needs_restart {
            return Ok(());
        }

        if self.active_process.is_some() {
            tracing::info!(
                "WorkerManager: restarting worker (engine: {:?} -> {:?}, model: {:?} -> {:?}, device: {:?} -> {:?})",
                self.active_engine,
                req.engine,
                self.active_model,
                req.model,
                self.active_device,
                req.device
            );
            self.clear_active_worker().await;
        } else {
            tracing::info!(
                "WorkerManager: starting new worker for engine: {:?}, model: {:?}, device: {:?}",
                req.engine,
                req.model,
                req.device
            );
        }

        let _ = self.event_tx.send(ManagerEvent::WorkerStarting).await;

        match self.spawner.spawn(&self.paths, req, &self.active_pid).await {
            Ok(proc) => {
                self.active_process = Some(proc);
                self.active_engine = Some(req.engine);
                self.active_model = Some(req.model.clone());
                self.active_device = Some(req.device.clone());
                self.update_status_active(req.engine, &req.model, &req.device, &req.mode);
                Ok(())
            }
            Err(e) => {
                let _ = reply.send(WorkerEvent::Error(e)).await;
                Err(())
            }
        }
    }

    fn maybe_hot_swap_tracking(&mut self, req: &WorkerRequest) {
        if self.active_process.is_none() || self.active_engine != Some(req.engine) {
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
            self.update_status_active(req.engine, &req.model, &req.device, &req.mode);
        }
    }

    async fn clear_active_worker(&mut self) {
        if let Some(mut proc) = self.active_process.take() {
            proc.shutdown(&self.active_pid).await;
        }
        self.active_engine = None;
        self.active_model = None;
        self.active_device = None;
        self.update_status_idle();
    }

    fn update_status_active(
        &self,
        engine: EmbeddingEngine,
        model: &str,
        device: &str,
        request_mode: &str,
    ) {
        if let Ok(mut status) = self.status.write() {
            status.active = true;
            status.engine = Some(engine.as_str().to_string());
            status.model = Some(model.to_string());
            status.device = Some(device.to_string());
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
    use std::sync::atomic::AtomicUsize;

    struct FakeSession {
        send_calls: Arc<AtomicUsize>,
        shutdown_calls: Arc<AtomicUsize>,
        send_should_fail: bool,
    }

    #[async_trait]
    impl WorkerSession for FakeSession {
        async fn send_request(
            &mut self,
            _req_json: &str,
            _reply: &mpsc::Sender<WorkerEvent>,
        ) -> Result<(), ()> {
            self.send_calls.fetch_add(1, Ordering::Relaxed);
            if self.send_should_fail {
                return Err(());
            }
            Ok(())
        }

        async fn shutdown(&mut self, _pid_slot: &AtomicU32) {
            self.shutdown_calls.fetch_add(1, Ordering::Relaxed);
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
        ) -> Result<Box<dyn WorkerSession>, String> {
            self.spawn_calls.fetch_add(1, Ordering::Relaxed);
            if self.spawn_should_fail {
                return Err("Failed to spawn worker: fake failure".to_string());
            }
            Ok(Box::new(FakeSession {
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
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: false,
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
        }));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
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
            engine: Some("candle".to_string()),
            model: Some("model-a".to_string()),
            device: Some("cpu".to_string()),
            request_mode: Some("embed".to_string()),
            pid: Some(44),
            timeout_secs: 300,
        }));
        let (old_tx, _old_rx) = mpsc::channel(1);
        let sender_slot = Arc::new(std::sync::Mutex::new(old_tx));

        let _new_rx = reset_after_runtime_panic(&active_pid, &status, &sender_slot);

        assert_eq!(active_pid.load(Ordering::Relaxed), 0);
        let status = status.read().unwrap();
        assert!(!status.active);
        assert!(status.engine.is_none());
        assert!(status.model.is_none());
    }

    #[test]
    fn test_restart_runtime_after_panic_resets_state() {
        let active_pid = Arc::new(AtomicU32::new(12));
        let status = Arc::new(RwLock::new(WorkerStatus {
            active: true,
            engine: Some("candle".to_string()),
            model: Some("model-a".to_string()),
            device: Some("cpu".to_string()),
            request_mode: Some("embed".to_string()),
            pid: Some(12),
            timeout_secs: 300,
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
    fn test_should_restart_worker_uses_active_session_and_engine() {
        assert!(should_restart_worker(false, None, EmbeddingEngine::Candle));
        assert!(should_restart_worker(
            true,
            Some(EmbeddingEngine::Candle),
            EmbeddingEngine::SBERT
        ));
        assert!(!should_restart_worker(
            true,
            Some(EmbeddingEngine::Candle),
            EmbeddingEngine::Candle
        ));
    }

    #[test]
    fn test_serialize_request_for_worker_round_trips_json() {
        let req = WorkerRequest {
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(16),
            chunk_overlap: Some(4),
            paths: None,
            supported_extensions: vec!["txt".to_string()],
        };

        let json = serialize_request_for_worker(&req).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["mode"], "embed");
        assert_eq!(value["model"], "model-a");
        assert_eq!(value["device"], "cpu");
        assert_eq!(value["chunk_size"], 16);
    }

    #[tokio::test]
    async fn test_handle_command_kill_worker_clears_active_process() {
        let (mut runtime, _spawn_calls, _send_calls, shutdown_calls, _tx, _event_rx) =
            test_runtime();
        runtime.active_process = Some(Box::new(FakeSession {
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        }));
        runtime.active_engine = Some(EmbeddingEngine::Candle);
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
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
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
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
        }));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
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
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
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
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
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
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
        };
        let second = WorkerRequest {
            engine: EmbeddingEngine::SBERT,
            ..first.clone()
        };
        let (reply_tx, _reply_rx) = mpsc::channel(4);

        runtime.ensure_worker(&first, &reply_tx).await.unwrap();
        runtime.ensure_worker(&second, &reply_tx).await.unwrap();

        assert_eq!(spawn_calls.load(Ordering::Relaxed), 2);
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.active_engine, Some(EmbeddingEngine::SBERT));
    }

    #[test]
    fn test_maybe_hot_swap_tracking_returns_without_active_process() {
        let (mut runtime, _spawn_calls, _send_calls, _shutdown_calls, _tx, _event_rx) =
            test_runtime();
        let req = WorkerRequest {
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
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
            engine: None,
            model: None,
            device: None,
            request_mode: None,
            pid: None,
            timeout_secs: 300,
        }));
        let spawn_calls = Arc::new(AtomicUsize::new(0));
        let send_calls = Arc::new(AtomicUsize::new(0));
        let shutdown_calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = WorkerRuntime::new(
            paths,
            rx,
            event_tx,
            active_pid,
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
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            device: "cpu".to_string(),
            texts: Some(vec!["hello".to_string()]),
            mode: "embed".to_string(),
            root: std::path::PathBuf::from("root"),
            data_dir: std::path::PathBuf::from("data"),
            chunk_size: Some(0),
            chunk_overlap: Some(0),
            paths: None,
            supported_extensions: vec![],
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
        runtime.active_process = Some(Box::new(FakeSession {
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        }));
        runtime.active_engine = Some(EmbeddingEngine::Candle);
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
        runtime.active_process = Some(Box::new(FakeSession {
            send_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: Arc::clone(&shutdown_calls),
            send_should_fail: false,
        }));
        runtime.active_engine = Some(EmbeddingEngine::Candle);
        runtime.active_model = Some("model-a".to_string());
        runtime.active_device = Some("cpu".to_string());
        runtime.idle_timeout = std::time::Duration::from_millis(20);

        let handle = tokio::spawn(runtime.run());
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(shutdown_calls.load(Ordering::Relaxed), 1);
        drop(tx);
        handle.await.unwrap();
        assert!(!status.read().unwrap().active);
    }
}
