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

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

struct JobHandle {
    handle: HANDLE,
}

unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn terminate_process_by_pid(pid: u32, label: &str) {
    use std::ptr::null_mut;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle == null_mut() {
        tracing::warn!(
            "WorkerProcess::{label}: OpenProcess(PROCESS_TERMINATE) failed for pid {pid}: {}",
            std::io::Error::last_os_error()
        );
        return;
    }
    let rc = unsafe { TerminateProcess(handle, 1) };
    if rc == 0 {
        tracing::warn!(
            "WorkerProcess::{label}: TerminateProcess failed for pid {pid}: {}",
            std::io::Error::last_os_error()
        );
    }
    unsafe { CloseHandle(handle) };
}

fn terminate_job_tree(job: &Arc<JobHandle>, pid: u32, label: &str) {
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    let rc = unsafe { TerminateJobObject(job.handle, 1) };
    if rc == 0 {
        tracing::warn!(
            "WorkerProcess::{label}: TerminateJobObject failed for pid {pid}: {}",
            std::io::Error::last_os_error()
        );
        terminate_process_by_pid(pid, label);
    }
}

struct WorkerInner {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
    _job: Arc<JobHandle>,
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
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
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

fn create_job_for_child(child: &std::process::Child) -> Result<Arc<JobHandle>, String> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    let job = unsafe { CreateJobObjectW(null_mut(), null_mut()) };
    if job == std::ptr::null_mut() {
        return Err(format!(
            "CreateJobObjectW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let rc = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if rc == 0 {
        unsafe { CloseHandle(job) };
        return Err(format!(
            "SetInformationJobObject failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let process_handle = child.as_raw_handle() as HANDLE;
    let rc = unsafe { AssignProcessToJobObject(job, process_handle) };
    if rc == 0 {
        unsafe { CloseHandle(job) };
        return Err(format!(
            "AssignProcessToJobObject failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(Arc::new(JobHandle { handle: job }))
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

        let job = create_job_for_child(&child)?;

        let pid = child.id();
        active_pid.store(pid, Ordering::Relaxed);

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
                _job: job,
            })),
        })
    }

    pub(crate) async fn shutdown(&mut self, pid_slot: &AtomicU32) {
        let inner = Arc::clone(&self.inner);
        let result = tokio::task::spawn_blocking(move || {
            let mut inner = inner.lock().unwrap();
            let pid = inner.child.id();
            drop(inner.child.stdin.take());
            tracing::info!(
                "WorkerProcess::shutdown: sent EOF to pid {pid}; waiting up to {:?} for graceful exit",
                ROOF_KNOCK_TIMEOUT
            );
            match wait_for_process_exit(&mut inner.child, ROOF_KNOCK_TIMEOUT) {
                Ok(true) => {
                    tracing::info!(
                        "WorkerProcess::shutdown: pid {pid} exited during grace period"
                    );
                    Ok(())
                }
                Ok(false) => {
                    tracing::warn!(
                        "WorkerProcess::shutdown: pid {pid} did not exit during grace period; hard-killing tree"
                    );
                    terminate_job_tree(&inner._job, pid, "shutdown");
                    let _ = inner
                        .child
                        .wait()
                        .map_err(|e| format!("Failed to reap worker after kill: {e}"))?;
                    Ok(())
                }
                Err(e) => Err(e),
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
