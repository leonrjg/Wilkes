use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use super::{
    apply_command_plan, build_command_plan, parse_worker_stdout_line, ProtocolReadOutcome,
    ROOF_KNOCK_TIMEOUT,
};
use crate::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use crate::embed::worker::manager::WorkerPaths;

struct WorkerInner {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
}

pub(crate) struct WorkerProcess {
    inner: Arc<Mutex<WorkerInner>>,
}

fn spawn_stderr_forwarder(stderr: std::process::ChildStderr) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let clean = strip_ansi_escapes::strip_str(line.trim_end());
                    tracing::info!("[worker-stderr] {clean}");
                }
                Err(e) => {
                    tracing::warn!("Worker stderr forwarder failed: {e}");
                    break;
                }
            }
        }
    });
}

fn wait_for_process_exit(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> Result<bool, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let handle = child.as_raw_handle() as HANDLE;
    let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let wait_rc = unsafe { WaitForSingleObject(handle, timeout_ms) };
    match wait_rc {
        WAIT_OBJECT_0 => {
            let _ = child.wait().map_err(|e| format!("Failed to reap worker: {e}"))?;
            Ok(true)
        }
        WAIT_TIMEOUT => Ok(false),
        other => Err(format!(
            "WaitForSingleObject failed while waiting for worker exit: {other}"
        )),
    }
}

fn terminate_process(child: &mut std::process::Child) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::TerminateProcess;

    let handle = child.as_raw_handle() as HANDLE;
    let rc = unsafe { TerminateProcess(handle, 1) };
    if rc == 0 {
        return Err(format!(
            "TerminateProcess failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
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
            .spawn()
            .map_err(|e| format!("Failed to spawn worker: {e}"))?;

        active_pid.store(child.id(), Ordering::Relaxed);

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture worker stdout".to_string())?;

        if let Some(stderr) = child.stderr.take() {
            spawn_stderr_forwarder(stderr);
        }

        Ok(Self {
            inner: Arc::new(Mutex::new(WorkerInner {
                child,
                stdout: BufReader::new(stdout),
            })),
        })
    }

    pub(crate) async fn shutdown(&mut self, pid_slot: &AtomicU32) {
        let inner = Arc::clone(&self.inner);
        let result = tokio::task::spawn_blocking(move || {
            let mut inner = inner.lock().unwrap();
            let pid = inner.child.id();
            tracing::info!("WorkerProcess::shutdown: closing stdin for pid {pid}");
            drop(inner.child.stdin.take());

            match wait_for_process_exit(&mut inner.child, ROOF_KNOCK_TIMEOUT) {
                Ok(true) => Ok(()),
                Ok(false) => {
                    tracing::warn!(
                        "WorkerProcess::shutdown: pid {} ignored EOF grace period, calling TerminateProcess",
                        pid
                    );
                    terminate_process(&mut inner.child)?;
                    let exited = wait_for_process_exit(&mut inner.child, ROOF_KNOCK_TIMEOUT)?;
                    if !exited {
                        return Err(
                            "Worker did not exit after TerminateProcess within timeout".to_string()
                        );
                    }
                    Ok(())
                }
                Err(e) => {
                    tracing::warn!("WorkerProcess::shutdown: graceful wait failed: {e}");
                    terminate_process(&mut inner.child)?;
                    let _ = wait_for_process_exit(&mut inner.child, ROOF_KNOCK_TIMEOUT);
                    Ok(())
                }
            }
        })
        .await;

        if let Err(e) = result {
            tracing::warn!("WorkerProcess::shutdown thread join failed: {e}");
        } else if let Ok(Err(e)) = result {
            tracing::warn!("{e}");
        }

        pid_slot.store(0, Ordering::Relaxed);
    }

    pub(crate) async fn send_request(
        &mut self,
        req_json: &str,
        reply: &mpsc::Sender<WorkerEvent>,
    ) -> Result<(), ()> {
        let req_json = req_json.to_string();
        let reply = reply.clone();
        let reply_outer = reply.clone();
        let inner = Arc::clone(&self.inner);

        let result = tokio::task::spawn_blocking(move || {
            let mut inner = inner.lock().unwrap();

            let mut success = false;
            if let Some(stdin) = inner.child.stdin.as_mut() {
                if stdin.write_all(req_json.as_bytes()).is_ok()
                    && stdin.write_all(b"\n").is_ok()
                    && stdin.flush().is_ok()
                {
                    success = true;
                }
            }

            if !success {
                let _ = reply.blocking_send(WorkerEvent::Error(
                    "Failed to write to worker stdin".to_string(),
                ));
                return Err(());
            }

            let mut line = String::new();
            loop {
                line.clear();
                match inner.stdout.read_line(&mut line) {
                    Ok(0) => {
                        let _ = reply.blocking_send(WorkerEvent::Error(
                            "Worker process closed stdout unexpectedly".to_string(),
                        ));
                        return Err(());
                    }
                    Ok(_) => match parse_worker_stdout_line(&line) {
                        ProtocolReadOutcome::Emit(event) => {
                            let is_end = matches!(event, WorkerEvent::Done | WorkerEvent::Error(_));
                            if reply.blocking_send(event).is_err() {
                                return Ok(());
                            }
                            if is_end {
                                return Ok(());
                            }
                        }
                        ProtocolReadOutcome::IgnoreNonProtocolLine => {}
                        ProtocolReadOutcome::ClosedStdout => {
                            let _ = reply.blocking_send(WorkerEvent::Error(
                                "Worker process closed stdout unexpectedly".to_string(),
                            ));
                            return Err(());
                        }
                        ProtocolReadOutcome::ReadError(message) => {
                            tracing::warn!("{message}");
                        }
                    },
                    Err(e) => {
                        let _ = reply.blocking_send(WorkerEvent::Error(format!(
                            "Failed to read from worker: {e}"
                        )));
                        return Err(());
                    }
                }
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(e) => {
                tracing::warn!("WorkerProcess::send_request thread join failed: {e}");
                let _ = reply_outer
                    .send(WorkerEvent::Error(
                        "Worker request thread failed unexpectedly".to_string(),
                    ))
                    .await;
                Err(())
            }
        }
    }
}