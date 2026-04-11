use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::mpsc;

use super::ipc::{WorkerEvent, WorkerRequest};
use super::process::{kill_after_timeout, ROOF_KNOCK_TIMEOUT};
use super::runtime::supervised_manager_loop;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerStatus {
    pub active: bool,
    pub engine: Option<String>,
    pub model: Option<String>,
    pub device: Option<String>,
    pub request_mode: Option<String>,
    pub pid: Option<u32>,
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

pub enum ManagerCommand {
    Submit {
        req: Box<WorkerRequest>,
        reply: mpsc::Sender<WorkerEvent>,
    },
    ShutdownWorker,
    SetTimeout(u64),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum ManagerEvent {
    WorkerStarting,
    ReindexingDone,
}

type SenderSlot = Arc<std::sync::Mutex<mpsc::Sender<ManagerCommand>>>;

#[derive(Clone)]
pub struct WorkerManager {
    sender: SenderSlot,
    active_pid: Arc<AtomicU32>,
    /// Status snapshot updated by the manager loop; readable without going through the loop.
    status: Arc<RwLock<WorkerStatus>>,
}

impl WorkerManager {
    pub fn new(
        paths: WorkerPaths,
    ) -> (
        Self,
        mpsc::Receiver<ManagerEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (tx, rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(32);
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
        let sender: SenderSlot = Arc::new(std::sync::Mutex::new(tx));
        let fut = supervised_manager_loop(
            paths,
            rx,
            event_tx,
            Arc::clone(&active_pid),
            Arc::clone(&sender),
            Arc::clone(&status),
        );
        (
            Self {
                sender,
                active_pid,
                status,
            },
            event_rx,
            fut,
        )
    }

    pub fn status(&self) -> WorkerStatus {
        self.status.read().unwrap().clone()
    }

    pub async fn send(
        &self,
        cmd: ManagerCommand,
    ) -> Result<(), mpsc::error::SendError<ManagerCommand>> {
        let sender = self.sender.lock().unwrap().clone();
        sender.send(cmd).await
    }

    pub fn try_send(
        &self,
        cmd: ManagerCommand,
    ) -> Result<(), mpsc::error::TrySendError<ManagerCommand>> {
        self.sender.lock().unwrap().try_send(cmd)
    }

    pub fn blocking_send(
        &self,
        cmd: ManagerCommand,
    ) -> Result<(), mpsc::error::SendError<ManagerCommand>> {
        self.sender.lock().unwrap().blocking_send(cmd)
    }

    pub fn request_shutdown(&self) {
        let _ = self.try_send(ManagerCommand::ShutdownWorker);
        self.roof_knock_active(ROOF_KNOCK_TIMEOUT);
    }

    pub fn roof_knock_active(&self, timeout: Duration) {
        let pid = self.active_pid.load(Ordering::Relaxed);
        if pid != 0 {
            let active_pid = Arc::clone(&self.active_pid);
            std::thread::spawn(move || {
                kill_after_timeout(pid, timeout, "request_shutdown");
                if !super::process::pid_is_alive(pid) {
                    let _ =
                        active_pid.compare_exchange(pid, 0, Ordering::Relaxed, Ordering::Relaxed);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::types::EmbeddingEngine;
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
        assert!(!status.active);
        assert_eq!(status.timeout_secs, 300);
    }

    #[tokio::test]
    async fn test_worker_paths_resolve_shapes_paths() {
        let dir = tempdir().unwrap();
        let paths = WorkerPaths::resolve(dir.path());

        assert_eq!(paths.data_dir, dir.path());
        assert_eq!(paths.venv_dir, dir.path().join("sbert_venv"));
        assert_eq!(
            paths.worker_bin.file_name().and_then(|s| s.to_str()),
            Some("wilkes-rust-worker")
        );
    }

    #[tokio::test]
    async fn test_worker_manager_try_send_and_blocking_send() {
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

        manager.try_send(ManagerCommand::SetTimeout(55)).unwrap();
        let manager_for_blocking = manager.clone();
        tokio::task::spawn_blocking(move || {
            manager_for_blocking
                .blocking_send(ManagerCommand::SetTimeout(77))
                .unwrap();
        })
        .await
        .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let status = manager.status();
        assert_eq!(status.timeout_secs, 77);
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

        manager.request_shutdown();
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
        manager.send(ManagerCommand::ShutdownWorker).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let status = manager.status();
        assert_eq!(status.timeout_secs, 100);
    }

    #[tokio::test]
    async fn test_worker_manager_submit() {
        let dir = tempfile::tempdir().unwrap();
        let worker_bin = dir.path().join("fake_worker");
        std::fs::write(&worker_bin, "").unwrap();

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

        manager
            .send(ManagerCommand::Submit {
                req,
                reply: reply_tx,
            })
            .await
            .unwrap();

        let _ = reply_rx.recv().await;
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

        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

        let status = manager.status();
        assert!(!status.active);
    }

    #[tokio::test]
    async fn test_worker_manager_spawn_fail() {
        let dir = tempdir().unwrap();
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

        manager
            .send(ManagerCommand::Submit {
                req,
                reply: reply_tx,
            })
            .await
            .unwrap();

        match reply_rx.recv().await {
            Some(WorkerEvent::Error(e)) => assert!(e.contains("Failed to spawn worker")),
            _ => panic!("Expected spawn error"),
        }
    }

    #[tokio::test]
    async fn test_request_shutdown_is_non_blocking() {
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

        manager.request_shutdown();
    }
}
