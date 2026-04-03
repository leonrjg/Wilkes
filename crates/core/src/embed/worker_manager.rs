use std::path::PathBuf;
use std::process::Stdio;
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

#[derive(Clone)]
pub struct WorkerManager {
    sender: mpsc::Sender<ManagerCommand>,
}

impl WorkerManager {
    pub fn new(paths: WorkerPaths) -> (Self, impl std::future::Future<Output = ()> + Send) {
        let (tx, rx) = mpsc::channel(32);
        let fut = manager_loop(paths, rx);
        (Self { sender: tx }, fut)
    }

    pub fn sender(&self) -> &mpsc::Sender<ManagerCommand> {
        &self.sender
    }
}

struct ActiveProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

async fn manager_loop(paths: WorkerPaths, mut rx: mpsc::Receiver<ManagerCommand>) {
    let mut active_process: Option<ActiveProcess> = None;
    let mut active_engine = None;
    let mut active_model = None;
    
    // Default 5 minute idle timeout
    let mut idle_timeout = Duration::from_secs(300);

    loop {
        let cmd = match timeout(idle_timeout, rx.recv()).await {
            Ok(Some(cmd)) => cmd,
            Ok(None) => break, // Channel closed
            Err(_) => {
                // Timeout elapsed, kill active child if any
                if let Some(mut proc) = active_process.take() {
                    tracing::info!("WorkerManager: Idle timeout reached, killing worker process.");
                    let _ = proc.child.kill().await;
                    active_engine = None;
                    active_model = None;
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
                    let _ = proc.child.kill().await;
                    active_engine = None;
                    active_model = None;
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

                let needs_restart = active_process.is_none() 
                    || active_engine != Some(req.engine.clone())
                    || active_model != Some(req.model.clone());

                if needs_restart {
                    if let Some(mut proc) = active_process.take() {
                        let _ = proc.child.kill().await;
                    }

                    let mut command = match req.engine {
                        EmbeddingEngine::SBERT => {
                            let mut c = Command::new(&paths.python_path);
                            c.arg(&paths.script_path);
                            c
                        }
                        _ => Command::new(&paths.worker_bin),
                    };

                    match command
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()
                    {
                        Ok(mut child) => {
                            let stdout = child.stdout.take().unwrap();
                            
                            // Optionally handle stderr by spawning a task
                            if let Some(stderr) = child.stderr.take() {
                                tokio::spawn(async move {
                                    let mut reader = BufReader::new(stderr);
                                    let mut line = String::new();
                                    while let Ok(n) = reader.read_line(&mut line).await {
                                        if n == 0 { break; }
                                        tracing::info!("[worker-stderr] {}", line.trim_end());
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
                        let _ = proc.child.kill().await;
                        active_process = None;
                        continue;
                    }

                    let mut line = String::new();
                    loop {
                        line.clear();
                        match proc.stdout.read_line(&mut line).await {
                            Ok(0) => {
                                let _ = reply.send(WorkerEvent::Error("Worker process closed stdout unexpectedly".to_string())).await;
                                let _ = proc.child.kill().await;
                                active_process = None;
                                break;
                            }
                            Ok(_) => {
                                match serde_json::from_str::<WorkerEvent>(line.trim()) {
                                    Ok(event) => {
                                        let is_end = matches!(event, WorkerEvent::Done | WorkerEvent::Error(_));
                                        if reply.send(event).await.is_err() {
                                            break; // Receiver dropped
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
                                let _ = proc.child.kill().await;
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
