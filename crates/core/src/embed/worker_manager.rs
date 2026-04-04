use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::embed::worker_ipc::{WorkerEvent, WorkerRequest};
use crate::types::EmbeddingEngine;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerStatus {
    pub active: bool,
    pub engine: Option<String>,
    pub model: Option<String>,
    pub timeout_secs: u64,
}

#[derive(Clone)]
pub struct WorkerPaths {
    pub python_path: PathBuf,
    pub script_path: PathBuf,
    pub worker_bin: PathBuf,
}

pub enum ManagerCommand {
    Submit {
        req: WorkerRequest,
        reply: mpsc::Sender<WorkerEvent>,
    },
    GetStatus(tokio::sync::oneshot::Sender<WorkerStatus>),
    KillWorker,
    SetTimeout(u64),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ManagerEvent {
    WorkerStarting,
    ReindexingDone,
}

#[derive(Clone)]
pub struct WorkerManager {
    sender: mpsc::Sender<ManagerCommand>,
    /// PID of the active worker process. 0 = no active worker.
    active_pid: Arc<AtomicU32>,
}

impl WorkerManager {
    pub fn new(paths: WorkerPaths) -> (Self, mpsc::Receiver<ManagerEvent>, impl std::future::Future<Output = ()> + Send) {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);
        let active_pid = Arc::new(AtomicU32::new(0));
        let fut = manager_loop(paths, rx, event_tx, Arc::clone(&active_pid));
        (Self { sender: tx, active_pid }, event_rx, fut)
    }

    pub fn sender(&self) -> &mpsc::Sender<ManagerCommand> {
        &self.sender
    }

    /// Kill the active worker process immediately via SIGKILL.
    /// Bypasses the manager loop — goes straight to the OS.
    pub fn kill_active(&self) {
        let pid = self.active_pid.swap(0, Ordering::Relaxed);
        if pid != 0 {
            tracing::info!("WorkerManager::kill_active: sending SIGKILL to pid {pid}");
            #[cfg(unix)]
            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
        }
    }
}

struct ActiveProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

/// Kill the child, wait for it to exit, and clear the shared PID slot.
async fn kill_and_reap(proc: &mut ActiveProcess, pid_slot: &AtomicU32) {
    let _ = proc.child.kill().await;
    let _ = proc.child.wait().await;
    pid_slot.store(0, Ordering::Relaxed);
}

async fn manager_loop(paths: WorkerPaths, mut rx: mpsc::Receiver<ManagerCommand>, event_tx: mpsc::Sender<ManagerEvent>, active_pid: Arc<AtomicU32>) {
    let mut active_process: Option<ActiveProcess> = None;
    let mut active_engine = None;
    let mut active_model = None;
    let mut active_device = None;

    // Default 5 minute idle timeout
    let mut idle_timeout = Duration::from_secs(300);

    loop {
        let cmd = match timeout(idle_timeout, rx.recv()).await {
            Ok(Some(cmd)) => cmd,
            Ok(None) => {
                if let Some(mut proc) = active_process.take() {
                    tracing::info!("WorkerManager: channel closed, killing worker process.");
                    kill_and_reap(&mut proc, &active_pid).await;
                }
                break;
            }
            Err(_) => {
                if let Some(mut proc) = active_process.take() {
                    tracing::info!("WorkerManager: Idle timeout reached, killing worker process.");
                    kill_and_reap(&mut proc, &active_pid).await;
                    active_engine = None;
                    active_model = None;
                    active_device = None;
                }
                continue;
            }
        };

        match cmd {
            ManagerCommand::GetStatus(reply) => {
                let status = WorkerStatus {
                    active: active_process.is_some(),
                    engine: active_engine.as_ref().map(|e: &EmbeddingEngine| e.as_str().to_string()),
                    model: active_model.clone(),
                    timeout_secs: idle_timeout.as_secs(),
                };
                let _ = reply.send(status);
            }
            ManagerCommand::KillWorker => {
                if let Some(mut proc) = active_process.take() {
                    tracing::info!("WorkerManager: Killing worker process per user request.");
                    kill_and_reap(&mut proc, &active_pid).await;
                    active_engine = None;
                    active_model = None;
                    active_device = None;
                }
            }
            ManagerCommand::SetTimeout(secs) => {
                idle_timeout = Duration::from_secs(secs);
                tracing::info!("WorkerManager: Idle timeout updated to {} seconds.", secs);
            }
            ManagerCommand::Submit { req, reply } => {
                let req_json = match serde_json::to_string(&req) {
                    Ok(j) => j,
                    Err(e) => {
                        let _ = reply.send(WorkerEvent::Error(format!("Serialize error: {e}"))).await;
                        continue;
                    }
                };

                let log_req = if req_json.len() > 200 {
                    format!("{}...", &req_json[..200])
                } else {
                    req_json.clone()
                };
                tracing::info!("WorkerManager: sending request: {}", log_req);

                let needs_restart = active_process.is_none()
                    || active_engine != Some(req.engine.clone())
                    || active_model != Some(req.model.clone())
                    || active_device != Some(req.device.clone());

                if needs_restart {
                    if let Some(mut proc) = active_process.take() {
                        tracing::info!(
                            "WorkerManager: restarting worker (engine: {:?} -> {:?}, model: {:?} -> {:?}, device: {:?} -> {:?})",
                            active_engine, req.engine,
                            active_model, req.model,
                            active_device, req.device
                        );
                        kill_and_reap(&mut proc, &active_pid).await;
                    } else {
                        tracing::info!("WorkerManager: starting new worker for engine: {:?}, model: {:?}, device: {:?}", req.engine, req.model, req.device);
                    }

                    let _ = event_tx.send(ManagerEvent::WorkerStarting).await;

                    let mut command = match req.engine {
                        EmbeddingEngine::SBERT => {
                            let mut c = Command::new(&paths.python_path);
                            if let Some(parent) = paths.script_path.parent() {
                                c.env("PYTHONPATH", parent);
                            }
                            c.arg("-m");
                            c.arg("wilkes_worker");
                            c
                        }
                        _ => Command::new(&paths.worker_bin),
                    };

                    match command
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()
                    {
                        Ok(mut child) => {
                            // Publish the PID so kill_active() can reach it.
                            if let Some(pid) = child.id() {
                                active_pid.store(pid, Ordering::Relaxed);
                            }

                            let stdout = child.stdout.take().unwrap();

                            if let Some(stderr) = child.stderr.take() {
                                tokio::spawn(async move {
                                    let mut reader = BufReader::new(stderr);
                                    let mut line = String::new();
                                    while let Ok(n) = reader.read_line(&mut line).await {
                                        if n == 0 { break; }
                                        let clean = strip_ansi_escapes::strip_str(line.trim_end());
                                        tracing::info!("[worker-stderr] {clean}");
                                        line.clear();
                                    }
                                });
                            }

                            active_process = Some(ActiveProcess {
                                child,
                                stdout: BufReader::new(stdout),
                            });
                            active_engine = Some(req.engine.clone());
                            active_model = Some(req.model.clone());
                            active_device = Some(req.device.clone());
                        }
                        Err(e) => {
                            let _ = reply.send(WorkerEvent::Error(format!("Failed to spawn worker: {e}"))).await;
                            continue;
                        }
                    }
                }

                // We have an active child.
                if let Some(proc) = active_process.as_mut() {
                    let mut success = false;
                    if let Some(stdin) = proc.child.stdin.as_mut() {
                        if stdin.write_all(req_json.as_bytes()).await.is_ok() {
                            if stdin.write_all(b"\n").await.is_ok() {
                                if stdin.flush().await.is_ok() {
                                    success = true;
                                }
                            }
                        }
                    }

                    if !success {
                        let _ = reply.send(WorkerEvent::Error("Failed to write to worker stdin".to_string())).await;
                        kill_and_reap(proc, &active_pid).await;
                        active_process = None;
                        continue;
                    }

                    let mut line = String::new();
                    loop {
                        line.clear();
                        match proc.stdout.read_line(&mut line).await {
                            Ok(0) => {
                                let _ = reply.send(WorkerEvent::Error("Worker process closed stdout unexpectedly".to_string())).await;
                                kill_and_reap(proc, &active_pid).await;
                                active_process = None;
                                break;
                            }
                            Ok(_) => {
                                match serde_json::from_str::<WorkerEvent>(line.trim()) {
                                    Ok(event) => {
                                        let is_end = matches!(event, WorkerEvent::Done | WorkerEvent::Error(_));

                                        if reply.send(event).await.is_err() {
                                            break;
                                        }

                                        if is_end {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("Failed to parse worker event: {e}, raw line: {}", line.trim());
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = reply.send(WorkerEvent::Error(format!("Failed to read from worker: {e}"))).await;
                                kill_and_reap(proc, &active_pid).await;
                                active_process = None;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}


