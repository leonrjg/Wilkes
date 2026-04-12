use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::{
    apply_command_plan, build_command_plan, parse_worker_stdout_line, ProtocolReadOutcome,
    ROOF_KNOCK_TIMEOUT,
};
use crate::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use crate::embed::worker::manager::WorkerPaths;

pub(crate) struct WorkerProcess {
    child: tokio::process::Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

async fn spawn_stderr_forwarder(stderr: tokio::process::ChildStderr) {
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
}

impl WorkerProcess {
    pub(crate) async fn spawn(
        paths: &WorkerPaths,
        req: &WorkerRequest,
        active_pid: &AtomicU32,
    ) -> Result<Self, String> {
        let plan = build_command_plan(paths, req).await?;
        let mut command = apply_command_plan(&plan);
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
            tokio::spawn(spawn_stderr_forwarder(stderr));
        }

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
        })
    }

    pub(crate) async fn shutdown(&mut self, pid_slot: &AtomicU32) {
        let pid = self.child.id().unwrap_or(0);
        drop(self.child.stdin.take());
        tracing::info!(
            "WorkerProcess::shutdown: sent EOF to pid {pid}; waiting up to {:?} for graceful exit",
            ROOF_KNOCK_TIMEOUT
        );
        if timeout(ROOF_KNOCK_TIMEOUT, self.child.wait()).await.is_err() {
            tracing::warn!(
                "WorkerProcess::shutdown: pid {pid} did not exit during grace period; hard-killing"
            );
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        } else {
            tracing::info!("WorkerProcess::shutdown: pid {pid} exited during grace period");
        }
        pid_slot.store(0, Ordering::Relaxed);
    }

    pub(crate) async fn send_request(
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
                Ok(0) => match ProtocolReadOutcome::ClosedStdout {
                    ProtocolReadOutcome::ClosedStdout => {
                        let _ = reply
                            .send(WorkerEvent::Error(
                                "Worker process closed stdout unexpectedly".to_string(),
                            ))
                            .await;
                        return Err(());
                    }
                    _ => unreachable!(),
                },
                Ok(_) => match parse_worker_stdout_line(&line) {
                    ProtocolReadOutcome::Emit(event) => {
                        let is_end = matches!(event, WorkerEvent::Done | WorkerEvent::Error(_));
                        if reply.send(event).await.is_err() {
                            return Ok(());
                        }
                        if is_end {
                            return Ok(());
                        }
                    }
                    ProtocolReadOutcome::IgnoreNonProtocolLine => {}
                    ProtocolReadOutcome::ClosedStdout => {
                        let _ = reply
                            .send(WorkerEvent::Error(
                                "Worker process closed stdout unexpectedly".to_string(),
                            ))
                            .await;
                        return Err(());
                    }
                    ProtocolReadOutcome::ReadError(message) => {
                        tracing::warn!("{message}");
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
