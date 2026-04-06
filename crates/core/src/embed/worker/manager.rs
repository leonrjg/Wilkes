use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ipc::{WorkerEvent, WorkerRequest};
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
    pub python_package_dir: PathBuf,
    pub requirements_path: PathBuf,
    pub venv_dir: PathBuf,
    pub worker_bin: PathBuf,
}

impl WorkerPaths {
    /// Attempt to resolve all paths automatically.
    pub fn resolve(data_dir: &Path) -> Self {
        use crate::path::{resolve_python, resolve_python_package_dir};

        let python_path = resolve_python().unwrap_or_default();
        let python_package_dir = resolve_python_package_dir().unwrap_or_default();
        let requirements_path = if python_package_dir.exists() {
            python_package_dir.join("requirements.txt")
        } else {
            PathBuf::new()
        };
        let venv_dir = data_dir.join("sbert_venv");
        let worker_bin = std::env::current_exe()
            .unwrap_or_default()
            .with_file_name("wilkes-rust-worker");

        Self {
            python_path,
            python_package_dir,
            requirements_path,
            venv_dir,
            worker_bin,
        }
    }
}

type SenderSlot = Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>;

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
    sender: SenderSlot,
    /// PID of the active worker process. 0 = no active worker.
    active_pid: Arc<AtomicU32>,
}

impl WorkerManager {
    pub fn new(paths: WorkerPaths) -> (Self, mpsc::Receiver<ManagerEvent>, impl std::future::Future<Output = ()> + Send) {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);
        let active_pid = Arc::new(AtomicU32::new(0));
        let sender: SenderSlot = Arc::new(std::sync::Mutex::new(tx));
        let fut = supervised_manager_loop(paths, rx, event_tx, Arc::clone(&active_pid), Arc::clone(&sender));
        (Self { sender, active_pid }, event_rx, fut)
    }

    pub async fn send(&self, cmd: ManagerCommand) -> Result<(), mpsc::error::SendError<ManagerCommand>> {
        let sender = self.sender.lock().unwrap().clone();
        sender.send(cmd).await
    }

    pub fn try_send(&self, cmd: ManagerCommand) -> Result<(), mpsc::error::TrySendError<ManagerCommand>> {
        self.sender.lock().unwrap().try_send(cmd)
    }

    pub fn blocking_send(&self, cmd: ManagerCommand) -> Result<(), mpsc::error::SendError<ManagerCommand>> {
        self.sender.lock().unwrap().blocking_send(cmd)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn test_worker_manager_status_inactive() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
        };

        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);

        let (tx, rx) = oneshot::channel();
        manager.send(ManagerCommand::GetStatus(tx)).await.unwrap();
        
        let status = rx.await.unwrap();
        assert_eq!(status.active, false);
        assert_eq!(status.timeout_secs, 300);
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

fn venv_python(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python3")
    }
}

/// Runs a subprocess, forwarding each line of stdout and stderr to tracing.
/// Returns an error string if the process fails to spawn or exits non-zero.
async fn run_setup_step(program: &Path, args: Vec<OsString>, label: &str) -> Result<(), String> {
    tracing::info!("[python-setup] {label}");
    let mut child = Command::new(program)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("[python-setup] Failed to start {label}: {e}"))?;

    let (line_tx, mut line_rx) = mpsc::channel::<String>(64);

    if let Some(stdout) = child.stdout.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).await.map(|n| n > 0).unwrap_or(false) {
                let _ = tx.send(line.trim_end().to_string()).await;
                line.clear();
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader.read_line(&mut line).await.map(|n| n > 0).unwrap_or(false) {
                let _ = tx.send(line.trim_end().to_string()).await;
                line.clear();
            }
        });
    }

    drop(line_tx);
    while let Some(line) = line_rx.recv().await {
        if !line.is_empty() {
            tracing::info!("[python-setup] {line}");
        }
    }

    let status = child.wait().await.map_err(|e| format!("[python-setup] {label} wait failed: {e}"))?;
    if !status.success() {
        return Err(format!("[python-setup] {label} failed (exit code {:?})", status.code()));
    }
    Ok(())
}

/// Ensures the Python virtualenv exists and has the correct packages installed.
/// Returns the path to the venv's Python interpreter on success.
async fn setup_python_env(paths: &WorkerPaths) -> Result<PathBuf, String> {
    let python = venv_python(&paths.venv_dir);
    let stamp = paths.venv_dir.join(".requirements_installed");

    // Check if setup can be skipped.
    let current_requirements = std::fs::read_to_string(&paths.requirements_path)
        .map_err(|e| format!("[python-setup] Cannot read requirements.txt: {e}"))?;

    if python.exists() && stamp.exists() {
        let installed = std::fs::read_to_string(&stamp).unwrap_or_default();
        if installed == current_requirements {
            tracing::info!("[python-setup] Virtualenv up to date, skipping setup.");
            return Ok(python);
        }
        tracing::info!("[python-setup] Requirements changed, reinstalling.");
    } else {
        tracing::info!("[python-setup] Setting up Python environment in {}", paths.venv_dir.display());
    }

    // Create the virtualenv.
    run_setup_step(
        &paths.python_path,
        vec!["-m".into(), "venv".into(), paths.venv_dir.as_os_str().to_owned()],
        "Create virtualenv",
    ).await?;

    // Ensure pip is available.
    run_setup_step(
        &python,
        vec!["-m".into(), "ensurepip".into(), "--upgrade".into()],
        "Ensure pip",
    ).await?;

    // Install requirements.
    run_setup_step(
        &python,
        vec!["-m".into(), "pip".into(), "install".into(), "-r".into(), paths.requirements_path.as_os_str().to_owned()],
        "Install requirements",
    ).await?;

    // Write stamp so we can skip next time.
    if let Err(e) = std::fs::write(&stamp, &current_requirements) {
        tracing::warn!("[python-setup] Failed to write requirements stamp: {e}");
    }

    tracing::info!("[python-setup] Python environment ready.");
    Ok(python)
}

async fn supervised_manager_loop(
    paths: WorkerPaths,
    initial_rx: mpsc::Receiver<ManagerCommand>,
    event_tx: mpsc::Sender<ManagerEvent>,
    active_pid: Arc<AtomicU32>,
    sender_slot: SenderSlot,
) {
    let mut rx = initial_rx;
    loop {
        let handle = tokio::task::spawn(manager_loop(
            paths.clone(),
            rx,
            event_tx.clone(),
            Arc::clone(&active_pid),
        ));
        match handle.await {
            Ok(()) => break, // channel closed normally, exit supervisor
            Err(e) if e.is_panic() => {
                tracing::error!("WorkerManager: loop panicked, restarting: {e:?}");
                active_pid.store(0, Ordering::Relaxed);
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

                let mut log_req = req.clone();
                log_req.texts = None;
                tracing::info!("WorkerManager: sending request: {:?}", serde_json::to_string(&log_req).unwrap_or_default());

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
                            let python = match setup_python_env(&paths).await {
                                Ok(p) => p,
                                Err(e) => {
                                    let _ = reply.send(WorkerEvent::Error(e)).await;
                                    continue;
                                }
                            };
                            let mut c = Command::new(&python);
                            c.env("PYTHONPATH", &paths.python_package_dir);
                            c.arg("-m");
                            c.arg("wilkes_python_worker");
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


