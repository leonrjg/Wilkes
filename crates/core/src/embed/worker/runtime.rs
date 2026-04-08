use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ipc::{WorkerEvent, WorkerRequest};
use super::manager::{ManagerCommand, ManagerEvent, WorkerPaths, WorkerStatus};
use super::process::WorkerProcess;
use crate::types::EmbeddingEngine;

pub(super) async fn supervised_manager_loop(
    paths: WorkerPaths,
    initial_rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    sender_slot: Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>,
    status: Arc<RwLock<WorkerStatus>>,
) {
    let mut rx = initial_rx;
    loop {
        let runtime = WorkerRuntime::new(
            paths.clone(),
            rx,
            event_tx.clone(),
            Arc::clone(&active_pid),
            Arc::clone(&status),
        );
        let handle = tokio::task::spawn(runtime.run());
        match handle.await {
            Ok(()) => break,
            Err(e) if e.is_panic() => {
                tracing::error!("WorkerManager: loop panicked, restarting: {e:?}");
                active_pid.store(0, Ordering::Relaxed);
                if let Ok(mut current) = status.write() {
                    current.active = false;
                    current.engine = None;
                    current.model = None;
                }
                let (new_tx, new_rx) = mpsc::channel(32);
                *sender_slot.lock().unwrap() = new_tx;
                rx = new_rx;
            }
            Err(e) => {
                tracing::error!("WorkerManager: loop task cancelled: {e:?}");
                break;
            }
        }
    }
}

struct WorkerRuntime {
    paths: WorkerPaths,
    rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    status: Arc<RwLock<WorkerStatus>>,
    active_process: Option<WorkerProcess>,
    active_engine: Option<EmbeddingEngine>,
    active_model: Option<String>,
    active_device: Option<String>,
    idle_timeout: Duration,
}

impl WorkerRuntime {
    fn new(
        paths: WorkerPaths,
        rx: mpsc::Receiver<ManagerCommand>,
        event_tx: mpsc::Sender<ManagerEvent>,
        active_pid: Arc<AtomicU32>,
        status: Arc<RwLock<WorkerStatus>>,
    ) -> Self {
        Self {
            paths,
            rx,
            event_tx,
            active_pid,
            status,
            active_process: None,
            active_engine: None,
            active_model: None,
            active_device: None,
            idle_timeout: Duration::from_secs(300),
        }
    }

    async fn run(mut self) {
        loop {
            let cmd = match timeout(self.idle_timeout, self.rx.recv()).await {
                Ok(Some(cmd)) => cmd,
                Ok(None) => {
                    if self.active_process.is_some() {
                        tracing::info!("WorkerManager: channel closed, killing worker process.");
                        self.clear_active_worker().await;
                    }
                    break;
                }
                Err(_) => {
                    if self.active_process.is_some() {
                        tracing::info!(
                            "WorkerManager: Idle timeout reached, killing worker process."
                        );
                        self.clear_active_worker().await;
                    }
                    continue;
                }
            };

            self.handle_command(cmd).await;
        }
    }

    async fn handle_command(&mut self, cmd: ManagerCommand) {
        match cmd {
            ManagerCommand::KillWorker => {
                if self.active_process.is_some() {
                    tracing::info!("WorkerManager: Killing worker process per user request.");
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
        let req_json = match serde_json::to_string(&req) {
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
        let needs_restart = self.active_process.is_none() || self.active_engine != Some(req.engine);

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

        match WorkerProcess::spawn(&self.paths, req, &self.active_pid).await {
            Ok(proc) => {
                self.active_process = Some(proc);
                self.active_engine = Some(req.engine);
                self.active_model = Some(req.model.clone());
                self.active_device = Some(req.device.clone());
                self.update_status_active(req.engine, &req.model);
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
            self.update_status_active(req.engine, &req.model);
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

    fn update_status_active(&self, engine: EmbeddingEngine, model: &str) {
        if let Ok(mut status) = self.status.write() {
            status.active = true;
            status.engine = Some(engine.as_str().to_string());
            status.model = Some(model.to_string());
        }
    }

    fn update_status_idle(&self) {
        if let Ok(mut status) = self.status.write() {
            status.active = false;
            status.engine = None;
            status.model = None;
        }
    }

    fn update_timeout(&self, secs: u64) {
        if let Ok(mut status) = self.status.write() {
            status.timeout_secs = secs;
        }
    }
}
