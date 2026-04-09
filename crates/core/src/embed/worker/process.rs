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
