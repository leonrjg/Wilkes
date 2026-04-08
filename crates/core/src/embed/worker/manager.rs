use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
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
    pub data_dir: PathBuf,
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
            data_dir: data_dir.to_path_buf(),
        }
    }
}

type SenderSlot = Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>;

pub enum ManagerCommand {
    Submit {
        req: Box<WorkerRequest>,
        reply: mpsc::Sender<WorkerEvent>,
    },
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
    /// Status snapshot updated by the manager loop; readable without going through the loop.
    status: Arc<RwLock<WorkerStatus>>,
}

impl WorkerManager {
    pub fn new(paths: WorkerPaths) -> (Self, mpsc::Receiver<ManagerEvent>, impl std::future::Future<Output = ()> + Send) {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);
        let active_pid = Arc::new(AtomicU32::new(0));
        let status = Arc::new(RwLock::new(WorkerStatus { active: false, engine: None, model: None, timeout_secs: 300 }));
        let sender: SenderSlot = Arc::new(std::sync::Mutex::new(tx));
        let fut = supervised_manager_loop(paths, rx, event_tx, Arc::clone(&active_pid), Arc::clone(&sender), Arc::clone(&status));
        (Self { sender, active_pid, status }, event_rx, fut)
    }

    pub fn status(&self) -> WorkerStatus {
        self.status.read().unwrap().clone()
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_worker_manager_status_inactive() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("python"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("reqs.txt"),
            venv_dir: PathBuf::from("venv"),
            worker_bin: PathBuf::from("worker"),
            data_dir: PathBuf::from("data"),
        };

        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);

        let status = manager.status();
        assert_eq!(status.active, false);
        assert_eq!(status.timeout_secs, 300);
        assert!(!status.active);
    }

    #[tokio::test]
    async fn test_worker_manager_lifecycle_no_process() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);
        
        manager.kill_active();
    }

    #[tokio::test]
    async fn test_worker_manager_commands() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);
        
        manager.send(ManagerCommand::SetTimeout(100)).await.unwrap();
        manager.send(ManagerCommand::KillWorker).await.unwrap();
        
        // Wait a bit for the loop to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        
        let status = manager.status();
        assert_eq!(status.timeout_secs, 100);
    }

    #[tokio::test]
    async fn test_worker_manager_submit() {
        let dir = tempfile::tempdir().unwrap();
        let worker_bin = dir.path().join("fake_worker");
        std::fs::write(&worker_bin, "").unwrap(); // just an empty file
        
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin,
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);

        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);
        let req = Box::new(WorkerRequest {
            mode: "test".to_string(),
            engine: EmbeddingEngine::Fastembed,
            model: "test_model".to_string(),
            data_dir: PathBuf::from("data"),
            device: "cpu".to_string(),
            root: PathBuf::from("root"),
            chunk_size: None,
            chunk_overlap: None,
            paths: None,
            supported_extensions: vec![],
            texts: None,
        });

        manager.send(ManagerCommand::Submit { req, reply: reply_tx }).await.unwrap();

        // Let the loop run
        let _ = reply_rx.recv().await;
    }

    #[tokio::test]
    async fn test_setup_python_env_mock() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A more robust mock that handles -m venv by creating the bin/python file.
            let script = r#"#!/bin/sh
    case "$*" in
    *"-m venv"*)
        # $3 is usually the venv path
        mkdir -p "$3/bin" || mkdir -p "$3/Scripts"
        touch "$3/bin/python3" || touch "$3/Scripts/python.exe"
        chmod +x "$3/bin/python3" 2>/dev/null || true
        ;;
    esac
    exit 0
    "#;
            std::fs::write(&python_path, script).unwrap();
            std::fs::set_permissions(&python_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            // Windows mock would need to be a .bat or similar.
            std::fs::write(&python_path, "@echo off\nexit 0").unwrap();
        }


        let requirements_path = dir.path().join("requirements.txt");
        std::fs::write(&requirements_path, "torch\n").unwrap();

        let paths = WorkerPaths {
            python_path: python_path.clone(),
            python_package_dir: dir.path().to_path_buf(),
            requirements_path: requirements_path.clone(),
            venv_dir: dir.path().join("venv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };

        // This will try to run "python -m venv", etc. 
        // Since our fake python returns 0 for everything, it should "succeed" setting up.
        let res = setup_python_env(&paths).await;
        assert!(res.is_ok());
        
        // Second call should skip setup because stamp exists
        let res2 = setup_python_env(&paths).await;
        assert!(res2.is_ok());
    }

    #[tokio::test]
    async fn test_run_setup_step_fail() {
        let dir = tempdir().unwrap();
        let bad_path = dir.path().join("non_existent");
        let res = run_setup_step(&bad_path, vec![], "test").await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_worker_manager_idle_timeout() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);

        manager.send(ManagerCommand::SetTimeout(1)).await.unwrap();
        
        // Let it time out
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
        
        let status = manager.status();
        assert!(!status.active);
    }

    #[tokio::test]
    async fn test_worker_manager_spawn_fail() {
        let dir = tempdir().unwrap();
        // A path that definitely doesn't exist
        let worker_bin = dir.path().join("this_does_not_exist");
        
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin,
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, loop_fut) = WorkerManager::new(paths);
        let _loop_handle = tokio::spawn(loop_fut);

        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);
        let req = Box::new(WorkerRequest {
            mode: "test".to_string(),
            engine: EmbeddingEngine::Candle,
            model: "m".to_string(),
            data_dir: PathBuf::from("data"),
            device: "cpu".to_string(),
            root: PathBuf::from("root"),
            chunk_size: None,
            chunk_overlap: None,
            paths: None,
            supported_extensions: vec![],
            texts: None,
        });

        manager.send(ManagerCommand::Submit { req, reply: reply_tx }).await.unwrap();

        match reply_rx.recv().await {
            Some(WorkerEvent::Error(e)) => assert!(e.contains("Failed to spawn worker")),
            _ => panic!("Expected spawn error"),
        }
    }
}

struct ActiveProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

/// Shut down the child process and clear the shared PID slot.
/// Closes stdin first so the worker can exit cleanly via its natural EOF condition,
/// then falls back to SIGKILL if it hasn't exited within the grace period.
async fn kill_and_reap(proc: &mut ActiveProcess, pid_slot: &AtomicU32) {
    drop(proc.child.stdin.take());
    if timeout(Duration::from_secs(2), proc.child.wait()).await.is_err() {
        let _ = proc.child.kill().await;
        let _ = proc.child.wait().await;
    }
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
    status: Arc<RwLock<WorkerStatus>>,
) {
    let mut rx = initial_rx;
    loop {
        let handle = tokio::task::spawn(manager_loop(
            paths.clone(),
            rx,
            event_tx.clone(),
            Arc::clone(&active_pid),
            Arc::clone(&status),
        ));
        match handle.await {
            Ok(()) => break, // channel closed normally, exit supervisor
            Err(e) if e.is_panic() => {
                tracing::error!("WorkerManager: loop panicked, restarting: {e:?}");
                active_pid.store(0, Ordering::Relaxed);
                if let Ok(mut s) = status.write() { s.active = false; s.engine = None; s.model = None; }
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

async fn manager_loop(paths: WorkerPaths, mut rx: mpsc::Receiver<ManagerCommand>, event_tx: mpsc::Sender<ManagerEvent>, active_pid: Arc<AtomicU32>, status: Arc<RwLock<WorkerStatus>>) {
    let mut active_process: Option<ActiveProcess> = None;
    let mut active_engine: Option<EmbeddingEngine> = None;
    let mut active_model: Option<String> = None;
    let mut active_device: Option<String> = None;

    // Default 5 minute idle timeout
    let mut idle_timeout = Duration::from_secs(300);

    macro_rules! set_status {
        (active: $eng:expr, $mdl:expr) => {
            if let Ok(mut s) = status.write() {
                s.active = true;
                s.engine = Some($eng.as_str().to_string());
                s.model = Some($mdl.clone());
            }
        };
        (idle) => {
            if let Ok(mut s) = status.write() {
                s.active = false;
                s.engine = None;
                s.model = None;
            }
        };
        (timeout: $secs:expr) => {
            if let Ok(mut s) = status.write() {
                s.timeout_secs = $secs;
            }
        };
    }

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
                    set_status!(idle);
                }
                continue;
            }
        };

        match cmd {
            ManagerCommand::KillWorker => {
                if let Some(mut proc) = active_process.take() {
                    tracing::info!("WorkerManager: Killing worker process per user request.");
                    kill_and_reap(&mut proc, &active_pid).await;
                    active_engine = None;
                    active_model = None;
                    active_device = None;
                    set_status!(idle);
                }
            }
            ManagerCommand::SetTimeout(secs) => {
                idle_timeout = Duration::from_secs(secs);
                set_status!(timeout: secs);
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
                    || active_engine != Some(req.engine);

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
                            c.env("HF_HUB_CACHE", &paths.data_dir);
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

                            active_engine = Some(req.engine);
                            active_model = Some(req.model.clone());
                            active_device = Some(req.device.clone());
                            set_status!(active: req.engine, req.model);
                            active_process = Some(ActiveProcess {
                                child,
                                stdout: BufReader::new(stdout),
                            });
                        }
                        Err(e) => {
                            let _ = reply.send(WorkerEvent::Error(format!("Failed to spawn worker: {e}"))).await;
                            continue;
                        }
                    }
                }

                // Same engine: the worker process can handle model/device changes in-process.
                // Update tracking so the next restart check reflects the current configuration.
                if !needs_restart
                    && (active_model.as_deref() != Some(req.model.as_str())
                        || active_device.as_deref() != Some(req.device.as_str()))
                {
                    tracing::info!(
                        "WorkerManager: hot-swapping model (model: {:?} -> {:?}, device: {:?} -> {:?})",
                        active_model, req.model, active_device, req.device
                    );
                    active_model = Some(req.model.clone());
                    active_device = Some(req.device.clone());
                    if let Some(engine) = active_engine {
                        set_status!(active: engine, req.model);
                    }
                }

                // We have an active child.
                if let Some(proc) = active_process.as_mut() {
                    let mut success = false;
                    if let Some(stdin) = proc.child.stdin.as_mut() {
                        if stdin.write_all(req_json.as_bytes()).await.is_ok()
                            && stdin.write_all(b"\n").await.is_ok()
                            && stdin.flush().await.is_ok()
                        {
                            success = true;
                        }
                    }

                    if !success {
                        let _ = reply.send(WorkerEvent::Error("Failed to write to worker stdin".to_string())).await;
                        kill_and_reap(proc, &active_pid).await;
                        active_process = None;
                        set_status!(idle);
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
                                set_status!(idle);
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
                                set_status!(idle);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}


