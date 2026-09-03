use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::timeout;

use super::{
    apply_command_plan, build_command_plan, parse_worker_stdout_line, ProtocolReadOutcome, Stop,
};
use crate::worker::ipc::{WorkerEvent, WorkerRequest};
use crate::worker::manager::WorkerPaths;

struct WorkerInner {
    child: tokio::process::Child,
}

#[derive(Clone)]
pub(crate) struct WorkerProcess {
    child: Arc<Mutex<WorkerInner>>,
    stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
    stdout: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
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

        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture worker stdout".to_string())?;

        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(spawn_stderr_forwarder(stderr));
        }

        Ok(Self {
            child: Arc::new(Mutex::new(WorkerInner { child })),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
        })
    }

    pub(crate) async fn shutdown(&self, pid_slot: &AtomicU32, stop: Stop) {
        let pid = self.child.lock().await.child.id().unwrap_or(0);
        drop(self.stdin.lock().await.take());
        let Some(grace) = stop.grace() else {
            tracing::info!("WorkerProcess::shutdown: killing pid {pid} without a grace period");
            let mut child = self.child.lock().await;
            let _ = child.child.kill().await;
            let _ = child.child.wait().await;
            pid_slot.store(0, Ordering::Relaxed);
            return;
        };
        tracing::info!(
            "WorkerProcess::shutdown: sent EOF to pid {pid}; waiting up to {grace:?} for graceful exit"
        );
        let mut child = self.child.lock().await;
        if timeout(grace, child.child.wait()).await.is_err() {
            tracing::warn!(
                "WorkerProcess::shutdown: pid {pid} did not exit during grace period; hard-killing"
            );
            let _ = child.child.kill().await;
            let _ = child.child.wait().await;
        } else {
            tracing::info!("WorkerProcess::shutdown: pid {pid} exited during grace period");
        }
        pid_slot.store(0, Ordering::Relaxed);
    }

    /// Write a single out-of-band line to worker stdin without waiting for the
    /// current request to finish. Used only for the generation cancel signal.
    pub(crate) async fn send_out_of_band(&self, line: &str) -> Result<(), ()> {
        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(());
        };
        if stdin.write_all(line.as_bytes()).await.is_err()
            || stdin.write_all(b"\n").await.is_err()
            || stdin.flush().await.is_err()
        {
            return Err(());
        }
        Ok(())
    }

    /// Read and discard worker output until the terminal event of the current
    /// request, leaving the pipe positioned for the next one.
    async fn drain_to_request_boundary(
        &self,
        stdout: &mut BufReader<tokio::process::ChildStdout>,
    ) -> Result<(), ()> {
        let mut line = String::new();
        let mut discarded = 0usize;
        loop {
            line.clear();
            match stdout.read_line(&mut line).await {
                // Worker died mid-drain; the process is finished either way.
                Ok(0) => return Err(()),
                Ok(_) => {
                    if let ProtocolReadOutcome::Emit(event) = parse_worker_stdout_line(&line) {
                        discarded += 1;
                        if event.is_terminal() {
                            tracing::debug!(
                                "WorkerProcess: drained {discarded} abandoned events to the request boundary"
                            );
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("WorkerProcess: failed to drain abandoned request: {e}");
                    return Err(());
                }
            }
        }
    }

    pub(crate) async fn send_request(
        &self,
        req_json: &str,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let mut success = false;
        if let Some(stdin) = self.stdin.lock().await.as_mut() {
            if stdin.write_all(req_json.as_bytes()).await.is_ok()
                && stdin.write_all(b"\n").await.is_ok()
                && stdin.flush().await.is_ok()
            {
                success = true;
            }
        }

        if !success {
            let _ = reply
                .send(WorkerEvent::Gone(
                    "failed to write to worker stdin".to_string(),
                ))
                .await;
            return Err(());
        }

        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line).await {
                Ok(0) => match ProtocolReadOutcome::ClosedStdout {
                    ProtocolReadOutcome::ClosedStdout => {
                        let _ = reply
                            .send(WorkerEvent::Gone(
                                "worker process closed stdout unexpectedly".to_string(),
                            ))
                            .await;
                        return Err(());
                    }
                    _ => unreachable!(),
                },
                Ok(_) => match parse_worker_stdout_line(&line) {
                    ProtocolReadOutcome::Emit(event) => {
                        let is_end = event.is_terminal();
                        if reply.send(event).await.is_err() {
                            // The receiver is gone, but the worker is still
                            // writing. Returning here would leave its remaining
                            // output in the pipe for the *next* request to read
                            // as its own — for a cancelled generation that is
                            // hundreds of lines. `send_request` owns the
                            // invariant "on return, the pipe is at a request
                            // boundary", so drain to the terminal event.
                            if is_end {
                                return Ok(());
                            }
                            return self.drain_to_request_boundary(&mut stdout).await;
                        }
                        if is_end {
                            return Ok(());
                        }
                    }
                    ProtocolReadOutcome::IgnoreNonProtocolLine => {}
                    ProtocolReadOutcome::ClosedStdout => {
                        let _ = reply
                            .send(WorkerEvent::Gone(
                                "worker process closed stdout unexpectedly".to_string(),
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
                        .send(WorkerEvent::Gone(format!(
                            "failed to read from worker: {e}"
                        )))
                        .await;
                    return Err(());
                }
            }
        }
    }
}
