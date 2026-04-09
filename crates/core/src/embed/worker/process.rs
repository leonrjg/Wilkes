use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::ipc::{WorkerEvent, WorkerRequest};
use super::manager::WorkerPaths;
use super::python_env::setup_python_env;
use crate::types::EmbeddingEngine;

pub(super) struct WorkerProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

impl WorkerProcess {
    pub(super) async fn spawn(
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
    ) -> Result<Self, String> {
        let mut command = build_command(paths, req).await?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to spawn worker: {e}"))?;

        if let Some(pid) = child.id() {
            active_pid.store(pid, Ordering::Relaxed);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture worker stdout".to_string())?;

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut line = String::new();
                while let Ok(n) = reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    let clean = strip_ansi_escapes::strip_str(line.trim_end());
                    tracing::info!("[worker-stderr] {clean}");
                    line.clear();
                }
            });
        }

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
        })
    }

    pub(super) async fn shutdown(&mut self, pid_slot: &AtomicU32) {
        drop(self.child.stdin.take());
        if timeout(Duration::from_secs(2), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        pid_slot.store(0, Ordering::Relaxed);
    }

    pub(super) async fn send_request(
        &mut self,
        req_json: &str,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let mut success = false;
        if let Some(stdin) = self.child.stdin.as_mut() {
            if stdin.write_all(req_json.as_bytes()).await.is_ok()
                && stdin.write_all(b"\n").await.is_ok()
                && stdin.flush().await.is_ok()
            {
                success = true;
            }
        }

        if !success {
            let _ = reply
                .send(WorkerEvent::Error(
                    "Failed to write to worker stdin".to_string(),
                ))
                .await;
            return Err(());
        }

        let mut line = String::new();
        loop {
            line.clear();
            match self.stdout.read_line(&mut line).await {
                Ok(0) => {
                    let _ = reply
                        .send(WorkerEvent::Error(
                            "Worker process closed stdout unexpectedly".to_string(),
                        ))
                        .await;
                    return Err(());
                }
                Ok(_) => match serde_json::from_str::<WorkerEvent>(line.trim()) {
                    Ok(event) => {
                        let is_end = matches!(event, WorkerEvent::Done | WorkerEvent::Error(_));

                        if reply.send(event).await.is_err() {
                            return Ok(());
                        }

                        if is_end {
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        let raw = line.trim();
                        if !raw.is_empty() {
                            tracing::warn!(
                                "Ignoring non-protocol worker stdout line: {e}, raw line: {raw}"
                            );
                        }
                    }
                },
                Err(e) => {
                    let _ = reply
                        .send(WorkerEvent::Error(format!(
                            "Failed to read from worker: {e}"
                        )))
                        .await;
                    return Err(());
                }
            }
        }
    }
}

async fn build_command(paths: &WorkerPaths, req: &WorkerRequest) -> Result<Command, String> {
    Ok(match req.engine {
        EmbeddingEngine::SBERT => {
            let python = setup_python_env(paths).await?;
            let cache_root = paths.data_dir.join("huggingface");
            let xdg_cache_root = paths.data_dir.join(".cache");
            std::fs::create_dir_all(&cache_root).map_err(|e| {
                format!(
                    "Failed to create Hugging Face cache directory {}: {e}",
                    cache_root.display()
                )
            })?;
            std::fs::create_dir_all(&xdg_cache_root).map_err(|e| {
                format!(
                    "Failed to create XDG cache directory {}: {e}",
                    xdg_cache_root.display()
                )
            })?;
            let mut command = Command::new(&python);
            command.env("PYTORCH_ENABLE_MPS_FALLBACK", "1");
            command.env("PYTHONPATH", &paths.python_package_dir);
            command.env("HOME", &paths.data_dir);
            command.env("XDG_CACHE_HOME", &xdg_cache_root);
            command.env("HF_HOME", &cache_root);
            command.env("HF_HUB_CACHE", cache_root.join("hub"));
            command.env("HF_ASSETS_CACHE", cache_root.join("assets"));
            command.env("HF_XET_CACHE", cache_root.join("xet"));
            command.env("HF_HUB_DISABLE_XET", "1");
            command.arg("-m");
            command.arg("wilkes_python_worker");
            command
        }
        _ => Command::new(&paths.worker_bin),
    })
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn write_executable(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_candle_worker_script(path: &Path, output_protocol: bool) {
        let body = if output_protocol {
            r#"#!/bin/sh
read req
echo noise-line
echo '{"Embeddings":[[1.0,2.0]]}'
echo '"Done"'
exit 0
"#
        } else {
            r#"#!/bin/sh
read req
exit 0
"#
        };
        write_executable(path, body);
    }

    fn write_fake_python_suite(path: &Path, venv_dir: &Path) {
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "-c" ]; then
    printf '%s\n' "3.9.6"
    exit 0
fi

case "$*" in
*"-m venv"*)
    mkdir -p "{venv}/bin"
    cat > "{venv}/bin/python3" <<'EOF'
#!/bin/sh
if [ "$1" = "-m" ] && [ "$2" = "wilkes_python_worker" ]; then
  read req
  echo noise-line
  echo '{{"Embeddings":[[1.0,2.0]]}}'
  echo '"Done"'
  exit 0
fi
exit 0
EOF
    chmod +x "{venv}/bin/python3"
    exit 0
    ;;
esac

exit 0
"#,
            venv = venv_dir.display()
        );
        write_executable(path, &script);
    }

    #[tokio::test]
    async fn test_worker_process_send_request_protocol() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");
        write_candle_worker_script(&worker_bin, true);

        let paths = WorkerPaths {
            python_path: dir.path().join("python"),
            python_package_dir: dir.path().to_path_buf(),
            requirements_path: dir.path().join("requirements.txt"),
            venv_dir: dir.path().join("venv"),
            worker_bin: worker_bin.clone(),
            data_dir: dir.path().to_path_buf(),
        };
        std::fs::write(&paths.requirements_path, "torch\n").unwrap();

        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: dir.path().to_path_buf(),
            engine: EmbeddingEngine::Candle,
            model: "m".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: Some(vec!["hello".to_string()]),
            supported_extensions: vec![],
        };

        let active_pid = AtomicU32::new(0);
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid).await.unwrap();
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        proc.send_request(&req_json, &reply_tx).await.unwrap();

        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Embeddings(v) => assert_eq!(v, vec![vec![1.0, 2.0]]),
            other => panic!("expected embeddings, got {other:?}"),
        }
        assert!(matches!(reply_rx.recv().await.unwrap(), WorkerEvent::Done));

        proc.shutdown(&active_pid).await;
        assert_eq!(active_pid.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_worker_process_send_request_closed_stdout() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");
        write_candle_worker_script(&worker_bin, false);

        let paths = WorkerPaths {
            python_path: dir.path().join("python"),
            python_package_dir: dir.path().to_path_buf(),
            requirements_path: dir.path().join("requirements.txt"),
            venv_dir: dir.path().join("venv"),
            worker_bin: worker_bin.clone(),
            data_dir: dir.path().to_path_buf(),
        };
        std::fs::write(&paths.requirements_path, "torch\n").unwrap();

        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: dir.path().to_path_buf(),
            engine: EmbeddingEngine::Candle,
            model: "m".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: Some(vec!["hello".to_string()]),
            supported_extensions: vec![],
        };

        let active_pid = AtomicU32::new(0);
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid).await.unwrap();
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        let res = proc.send_request(&req_json, &reply_tx).await;
        assert!(res.is_err());
        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Error(msg) => assert!(msg.contains("closed stdout")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_worker_process_spawn_sbert_build_command() {
        let dir = tempdir().unwrap();
        let python_path = dir.path().join("python");
        let venv_dir = dir.path().join("venv");
        write_fake_python_suite(&python_path, &venv_dir);

        let requirements_path = dir.path().join("requirements.txt");
        std::fs::write(&requirements_path, "torch\n").unwrap();

        let paths = WorkerPaths {
            python_path,
            python_package_dir: dir.path().to_path_buf(),
            requirements_path,
            venv_dir,
            worker_bin: dir.path().join("unused-worker"),
            data_dir: dir.path().to_path_buf(),
        };

        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: dir.path().to_path_buf(),
            engine: EmbeddingEngine::SBERT,
            model: "intfloat/e5-small-v2".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: Some(vec!["hello".to_string()]),
            supported_extensions: vec![],
        };

        let active_pid = AtomicU32::new(0);
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid).await.unwrap();
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        proc.send_request(&req_json, &reply_tx).await.unwrap();

        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Embeddings(v) => assert_eq!(v, vec![vec![1.0, 2.0]]),
            other => panic!("expected embeddings, got {other:?}"),
        }
        assert!(matches!(reply_rx.recv().await.unwrap(), WorkerEvent::Done));
    }
}
