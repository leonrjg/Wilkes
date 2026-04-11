use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::{
    apply_command_plan, build_command_plan, clear_active_stopper, force_stop_active,
    hard_kill_pid, parse_worker_stdout_line, pid_is_alive, register_active_stopper,
    ActiveStopper, ProtocolReadOutcome, ROOF_KNOCK_TIMEOUT,
};
use crate::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use crate::embed::worker::manager::WorkerPaths;

pub(crate) struct WorkerProcess {
    child: tokio::process::Child,
    stdout: BufReader<tokio::process::ChildStdout>,
}

struct UnixPidStopper {
    pid: u32,
}

impl ActiveStopper for UnixPidStopper {
    fn force_stop(&self, label: &str) {
        let pid = self.pid;
        if pid == 0 {
            return;
        }

        tracing::info!(
            "WorkerProcess::{label}: waiting up to {:?} for pid {pid} to exit after EOF",
            ROOF_KNOCK_TIMEOUT
        );
        let start = std::time::Instant::now();
        while start.elapsed() < ROOF_KNOCK_TIMEOUT {
            if !pid_is_alive(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        if pid_is_alive(pid) {
            tracing::warn!(
                "WorkerProcess::{label}: pid {pid} ignored EOF grace period, hard-killing"
            );
            hard_kill_pid(pid, label);
        }
    }
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
            register_active_stopper(std::sync::Arc::new(UnixPidStopper { pid }));
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
        drop(self.child.stdin.take());
        force_stop_active("shutdown");
        if timeout(ROOF_KNOCK_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        clear_active_stopper();
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
