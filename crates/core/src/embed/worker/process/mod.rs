use std::path::Path;
use std::time::Duration;

use super::ipc::{WorkerEvent, WorkerRequest};
use super::manager::WorkerPaths;
use super::python_env::setup_python_env;
use crate::types::EmbeddingEngine;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::WorkerProcess;
#[cfg(windows)]
pub(crate) use windows::WorkerProcess;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProcessCommandPlan {
    WorkerBin {
        worker_bin: std::path::PathBuf,
    },
    Sbert {
        python: std::path::PathBuf,
        package_dir: std::path::PathBuf,
        data_dir: std::path::PathBuf,
        cache_root: std::path::PathBuf,
        xdg_cache_root: std::path::PathBuf,
    },
}

#[derive(Debug)]
pub(super) enum ProtocolReadOutcome {
    Emit(WorkerEvent),
    IgnoreNonProtocolLine,
    ClosedStdout,
    ReadError(String),
}

pub(super) const ROOF_KNOCK_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg_attr(not(windows), allow(dead_code))]
#[cfg(any(test, windows))]
pub(super) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(unix)]
pub(super) fn pid_is_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        true
    } else {
        let errno = std::io::Error::last_os_error().raw_os_error();
        errno == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
pub(super) fn pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: i32, label: &str) {
    let rc = unsafe { libc::kill(pid as i32, signal) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("WorkerProcess::{label}: failed to signal pid {pid}: {err}");
    }
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: i32, _label: &str) {}

pub(super) fn kill_after_timeout(pid: u32, timeout: Duration, label: &str) {
    if pid == 0 {
        return;
    }

    tracing::info!(
        "WorkerProcess::{label}: waiting up to {:?} for pid {pid} to exit after EOF",
        timeout
    );

    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !pid_is_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if pid_is_alive(pid) {
        tracing::warn!(
            "WorkerProcess::{label}: pid {pid} ignored EOF grace period, sending SIGKILL"
        );
        #[cfg(unix)]
        send_signal(pid, libc::SIGKILL, label);
    }
}

pub(super) async fn build_command_plan(
    paths: &WorkerPaths,
    req: &WorkerRequest,
) -> Result<ProcessCommandPlan, String> {
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
            ProcessCommandPlan::Sbert {
                python,
                package_dir: paths.python_package_dir.clone(),
                data_dir: paths.data_dir.clone(),
                cache_root,
                xdg_cache_root,
            }
        }
        _ => ProcessCommandPlan::WorkerBin {
            worker_bin: paths.worker_bin.clone(),
        },
    })
}

#[cfg(not(windows))]
pub(super) type ProcessCommand = tokio::process::Command;
#[cfg(windows)]
pub(super) type ProcessCommand = std::process::Command;

pub(super) fn apply_command_plan(plan: &ProcessCommandPlan) -> ProcessCommand {
    let mut command = match plan {
        ProcessCommandPlan::WorkerBin { worker_bin } => ProcessCommand::new(worker_bin),
        ProcessCommandPlan::Sbert {
            python,
            package_dir,
            data_dir,
            cache_root,
            xdg_cache_root,
        } => {
            let mut command = ProcessCommand::new(python);
            command.env("PYTORCH_ENABLE_MPS_FALLBACK", "1");
            command.env("PYTHONPATH", package_dir);
            command.env("HOME", data_dir);
            command.env("XDG_CACHE_HOME", xdg_cache_root);
            command.env("HF_HOME", cache_root);
            command.env("HF_HUB_CACHE", cache_root.join("hub"));
            command.env("HF_ASSETS_CACHE", cache_root.join("assets"));
            command.env("HF_XET_CACHE", cache_root.join("xet"));
            command.env("HF_HUB_DISABLE_XET", "1");
            command.arg("-m");
            command.arg("wilkes_python_worker");
            command
        }
    };
    suppress_windows_console(&mut command);
    command
}

#[cfg(windows)]
fn suppress_windows_console(command: &mut ProcessCommand) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn suppress_windows_console(_command: &mut ProcessCommand) {}

pub(super) fn parse_worker_stdout_line(line: &str) -> ProtocolReadOutcome {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ProtocolReadOutcome::IgnoreNonProtocolLine;
    }

    match serde_json::from_str::<WorkerEvent>(trimmed) {
        Ok(event) => ProtocolReadOutcome::Emit(event),
        Err(e) => ProtocolReadOutcome::ReadError(format!(
            "Ignoring non-protocol worker stdout line: {e}, raw line: {trimmed}"
        )),
    }
}

#[allow(dead_code)]
fn _assert_path(_: &Path) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        proc.send_request(&req_json, &reply_tx).await.unwrap();

        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Embeddings(v) => assert_eq!(v, vec![vec![1.0, 2.0]]),
            other => panic!("expected embeddings, got {other:?}"),
        }
        assert!(matches!(reply_rx.recv().await.unwrap(), WorkerEvent::Done));

        proc.shutdown(&active_pid).await;
        assert_eq!(active_pid.load(std::sync::atomic::Ordering::Relaxed), 0);
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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        let res = proc.send_request(&req_json, &reply_tx).await;
        assert!(res.is_err());
        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Error(msg) => assert!(msg.contains("closed stdout")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_worker_process_send_request_write_failure() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");
        write_executable(&worker_bin, "#!/bin/sh\nexit 0\n");

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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        let res = proc.send_request(&req_json, &reply_tx).await;
        assert!(res.is_err());
        assert!(matches!(
            reply_rx.recv().await.unwrap(),
            WorkerEvent::Error(_)
        ));
    }

    #[tokio::test]
    async fn test_worker_process_send_request_read_error() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");
        write_executable(
            &worker_bin,
            r#"#!/bin/sh
read req
printf '\377\n'
exit 0
"#,
        );

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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        let res = proc.send_request(&req_json, &reply_tx).await;
        assert!(res.is_err());
        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Error(msg) => assert!(msg.contains("Failed to read")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_worker_process_shutdown_kills_hung_process() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");
        write_executable(&worker_bin, "#!/bin/sh\nsleep 5\nexit 0\n");

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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        proc.shutdown(&active_pid).await;
        assert_eq!(active_pid.load(std::sync::atomic::Ordering::Relaxed), 0);
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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        proc.send_request(&req_json, &reply_tx).await.unwrap();

        match reply_rx.recv().await.unwrap() {
            WorkerEvent::Embeddings(v) => assert_eq!(v, vec![vec![1.0, 2.0]]),
            other => panic!("expected embeddings, got {other:?}"),
        }
        assert!(matches!(reply_rx.recv().await.unwrap(), WorkerEvent::Done));
    }

    #[test]
    fn test_parse_worker_stdout_line_variants() {
        match parse_worker_stdout_line("") {
            ProtocolReadOutcome::IgnoreNonProtocolLine => {}
            other => panic!("expected ignore, got {other:?}"),
        }

        match parse_worker_stdout_line(r#"{"Done":null}"#) {
            ProtocolReadOutcome::Emit(WorkerEvent::Done) => {}
            other => panic!("expected done event, got {other:?}"),
        }

        match parse_worker_stdout_line("not json") {
            ProtocolReadOutcome::ReadError(message) => {
                assert!(message.contains("Ignoring non-protocol worker stdout line"));
            }
            other => panic!("expected read error, got {other:?}"),
        }
    }

    #[test]
    fn test_roof_knock_pid_zero() {
        kill_after_timeout(0, Duration::from_millis(1), "test");
    }

    #[tokio::test]
    async fn test_worker_process_send_request_read_error_variant() {
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");

        write_executable(
            &worker_bin,
            r#"#!/bin/sh
read req
echo 'not json'
echo '"Done"'
exit 0
"#,
        );

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
        let mut proc = WorkerProcess::spawn(&paths, &req, &active_pid)
            .await
            .unwrap();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(8);
        let req_json = serde_json::to_string(&req).unwrap();
        proc.send_request(&req_json, &reply_tx).await.unwrap();

        assert!(matches!(reply_rx.recv().await.unwrap(), WorkerEvent::Done));
    }
}
