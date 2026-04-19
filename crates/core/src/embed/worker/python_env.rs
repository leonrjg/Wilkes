use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::manager::WorkerPaths;

const MINIMUM_SBERT_PYTHON: PythonVersion = PythonVersion {
    major: 3,
    minor: 9,
    patch: 0,
};

#[cfg_attr(not(windows), allow(dead_code))]
#[cfg(any(test, windows))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn windows_creation_flags() -> u32 {
    CREATE_NO_WINDOW
}

#[cfg_attr(not(windows), allow(dead_code))]
#[cfg(not(windows))]
fn windows_creation_flags() -> u32 {
    0
}

#[cfg(windows)]
fn suppress_windows_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(windows_creation_flags());
}

#[cfg(not(windows))]
fn suppress_windows_console(_command: &mut Command) {}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PythonVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl PythonVersion {
    fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let mut parts = trimmed.split('.');

        let major = parts
            .next()
            .ok_or_else(|| format!("missing major version in '{trimmed}'"))?
            .parse::<u32>()
            .map_err(|e| format!("invalid major version in '{trimmed}': {e}"))?;
        let minor = parts
            .next()
            .ok_or_else(|| format!("missing minor version in '{trimmed}'"))?
            .parse::<u32>()
            .map_err(|e| format!("invalid minor version in '{trimmed}': {e}"))?;
        let patch = parts
            .next()
            .unwrap_or("0")
            .parse::<u32>()
            .map_err(|e| format!("invalid patch version in '{trimmed}': {e}"))?;

        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn short(&self) -> String {
        format!("{}.{}", self.major, self.minor)
    }
}

impl std::fmt::Display for PythonVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
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
pub(crate) async fn run_setup_step(
    program: &Path,
    args: Vec<OsString>,
    label: &str,
) -> Result<(), String> {
    tracing::info!("[python-setup] {label}");
    let mut command = Command::new(program);
    suppress_windows_console(&mut command);
    let mut child = command
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
            while reader
                .read_line(&mut line)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {
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
            while reader
                .read_line(&mut line)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {
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

    let status = child
        .wait()
        .await
        .map_err(|e| format!("[python-setup] {label} wait failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "[python-setup] {label} failed (exit code {:?})",
            status.code()
        ));
    }

    Ok(())
}

async fn read_python_version(python_path: &Path) -> Result<PythonVersion, String> {
    let mut command = Command::new(python_path);
    suppress_windows_console(&mut command);
    let output = command
        .args([
            "-c",
            "import sys; print('.'.join(str(part) for part in sys.version_info[:3]))",
        ])
        .output()
        .await
        .map_err(|e| {
            format!(
                "[python-setup] Failed to query Python version from {}: {e}",
                python_path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit code {:?}", output.status.code())
        } else {
            stderr
        };
        return Err(format!(
            "[python-setup] Failed to query Python version from {}: {}",
            python_path.display(),
            detail
        ));
    }

    let version_text = String::from_utf8_lossy(&output.stdout);
    PythonVersion::parse(&version_text).map_err(|e| {
        format!(
            "[python-setup] Failed to parse Python version from {}: {}",
            python_path.display(),
            e
        )
    })
}

fn build_requirements_stamp(requirements: &str, python_version: PythonVersion) -> String {
    format!("# python={python_version}\n{requirements}")
}

/// Ensures the Python virtualenv exists and has the correct packages installed.
/// Returns the path to the venv's Python interpreter on success.
pub(crate) async fn setup_python_env(paths: &WorkerPaths) -> Result<PathBuf, String> {
    let python = venv_python(&paths.venv_dir);
    let stamp = paths.venv_dir.join(".requirements_installed");

    let current_requirements = std::fs::read_to_string(&paths.requirements_path)
        .map_err(|e| format!("[python-setup] Cannot read requirements.txt: {e}"))?;
    let python_version = read_python_version(&paths.python_path).await?;
    if python_version < MINIMUM_SBERT_PYTHON {
        return Err(format!(
            "[python-setup] SBERT worker requires Python {}+; found Python {} at {}",
            MINIMUM_SBERT_PYTHON.short(),
            python_version,
            paths.python_path.display()
        ));
    }
    let current_stamp = build_requirements_stamp(&current_requirements, python_version);

    if python.exists() && stamp.exists() {
        let installed = std::fs::read_to_string(&stamp).unwrap_or_default();
        if installed == current_stamp {
            tracing::info!("[python-setup] Virtualenv up to date, skipping setup.");
            return Ok(python);
        }
        tracing::info!("[python-setup] Requirements changed, reinstalling.");
    } else {
        tracing::info!(
            "[python-setup] Setting up Python environment in {}",
            paths.venv_dir.display()
        );
    }

    run_setup_step(
        &paths.python_path,
        vec![
            "-m".into(),
            "venv".into(),
            paths.venv_dir.as_os_str().to_owned(),
        ],
        "Create virtualenv",
    )
    .await?;

    run_setup_step(
        &python,
        vec!["-m".into(), "ensurepip".into(), "--upgrade".into()],
        "Ensure pip",
    )
    .await?;

    run_setup_step(
        &python,
        vec![
            "-m".into(),
            "pip".into(),
            "install".into(),
            "-r".into(),
            paths.requirements_path.as_os_str().to_owned(),
        ],
        "Install requirements",
    )
    .await?;

    if let Err(e) = std::fs::write(&stamp, &current_stamp) {
        tracing::warn!("[python-setup] Failed to write requirements stamp: {e}");
    }

    tracing::info!("[python-setup] Python environment ready.");
    Ok(python)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_windows_creation_flags_shape() {
        #[cfg(windows)]
        assert_eq!(windows_creation_flags(), CREATE_NO_WINDOW);

        #[cfg(not(windows))]
        assert_eq!(windows_creation_flags(), 0);
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn write_fake_python(python_path: &Path, version_file: &Path, venv_counter_file: &Path) {
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "-c" ]; then
    cat "{}"
    exit 0
fi

if [ "$1" = "-m" ] && [ "$2" = "venv" ]; then
    mkdir -p "$3/bin"
    cat > "$3/bin/python3" <<'EOF'
#!/bin/sh
exit 0
EOF
    chmod +x "$3/bin/python3"
    count=0
    if [ -f "{}" ]; then
        count=$(cat "{}")
    fi
    count=$((count + 1))
    printf "%s" "$count" > "{}"
fi

exit 0
"#,
            version_file.display(),
            venv_counter_file.display(),
            venv_counter_file.display(),
            venv_counter_file.display()
        );

        write_executable(python_path, &script);
    }

    #[cfg(unix)]
    fn write_fake_python_with_output(python_path: &Path, version: &str, worker_body: &str) {
        let script = r#"#!/bin/sh
if [ "$1" = "-c" ]; then
    printf '%s\n' "__VERSION__"
    exit 0
fi

if [ "$1" = "-m" ] && [ "$2" = "venv" ]; then
    mkdir -p "$3/bin"
    cat > "$3/bin/python3" <<'EOF'
#!/bin/sh
if [ "$1" = "-m" ] && [ "$2" = "wilkes_python_worker" ]; then
__WORKER__
fi
exit 0
EOF
    chmod +x "$3/bin/python3"
fi

exit 0
"#
        .replace("__VERSION__", version)
        .replace("__WORKER__", worker_body);

        write_executable(python_path, &script);
    }

    #[tokio::test]
    async fn test_setup_python_env_mock() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        let version_file = dir.path().join("python-version.txt");
        let venv_counter_file = dir.path().join("venv-count.txt");
        std::fs::write(&version_file, "3.9.6\n").unwrap();
        std::fs::write(&venv_counter_file, "0").unwrap();
        #[cfg(unix)]
        write_fake_python(&python_path, &version_file, &venv_counter_file);
        #[cfg(windows)]
        {
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

        let res = setup_python_env(&paths).await;
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(std::fs::read_to_string(&venv_counter_file).unwrap(), "1");

        let stamp =
            std::fs::read_to_string(paths.venv_dir.join(".requirements_installed")).unwrap();
        assert!(stamp.starts_with("# python=3.9.6\n"));
    }

    #[tokio::test]
    async fn test_setup_python_env_skips_when_stamp_matches() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        let version_file = dir.path().join("python-version.txt");
        let venv_counter_file = dir.path().join("venv-count.txt");
        std::fs::write(&version_file, "3.9.6\n").unwrap();
        std::fs::write(&venv_counter_file, "0").unwrap();
        #[cfg(unix)]
        write_fake_python(&python_path, &version_file, &venv_counter_file);
        #[cfg(windows)]
        {
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

        let venv_python_path = venv_python(&paths.venv_dir);
        if let Some(parent) = venv_python_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        #[cfg(unix)]
        write_executable(&venv_python_path, "#!/bin/sh\nexit 0\n");
        #[cfg(windows)]
        {
            std::fs::write(&venv_python_path, "@echo off\nexit 0").unwrap();
        }

        let stamp_contents = build_requirements_stamp(
            "torch\n",
            PythonVersion {
                major: 3,
                minor: 9,
                patch: 6,
            },
        );
        std::fs::write(paths.venv_dir.join(".requirements_installed"), stamp_contents).unwrap();

        let res = setup_python_env(&paths).await;
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(std::fs::read_to_string(&venv_counter_file).unwrap(), "0");
    }

    #[tokio::test]
    async fn test_setup_python_env_rejects_old_python() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        let version_file = dir.path().join("python-version.txt");
        let venv_counter_file = dir.path().join("venv-count.txt");
        std::fs::write(&version_file, "3.8.18\n").unwrap();
        std::fs::write(&venv_counter_file, "0").unwrap();
        #[cfg(unix)]
        write_fake_python(&python_path, &version_file, &venv_counter_file);
        #[cfg(windows)]
        {
            std::fs::write(&python_path, "@echo off\nexit 0").unwrap();
        }

        let requirements_path = dir.path().join("requirements.txt");
        std::fs::write(&requirements_path, "torch\n").unwrap();

        let paths = WorkerPaths {
            python_path,
            python_package_dir: dir.path().to_path_buf(),
            requirements_path,
            venv_dir: dir.path().join("venv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };

        let err = setup_python_env(&paths).await.unwrap_err();
        assert!(err.contains("requires Python 3.9+"));
        assert!(err.contains("found Python 3.8.18"));
        assert_eq!(std::fs::read_to_string(&venv_counter_file).unwrap(), "0");
    }

    #[tokio::test]
    async fn test_run_setup_step_fail() {
        let dir = tempdir().unwrap();
        let bad_path = dir.path().join("non_existent");
        let res = run_setup_step(&bad_path, vec![], "test").await;
        assert!(res.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_setup_step_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("echo.sh");
        write_executable(
            &script,
            r#"#!/bin/sh
echo stdout-line
echo stderr-line 1>&2
exit 0
"#,
        );

        run_setup_step(&script, vec![], "echo-step").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_run_setup_step_non_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fail.sh");
        write_executable(
            &script,
            r#"#!/bin/sh
echo failing
exit 7
"#,
        );

        let err = run_setup_step(&script, vec![], "fail-step")
            .await
            .unwrap_err();
        assert!(err.contains("exit code Some(7)"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_setup_python_env_rejects_bad_version_output() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        write_fake_python_with_output(&python_path, "not-a-version", "echo worker");

        let requirements_path = dir.path().join("requirements.txt");
        std::fs::write(&requirements_path, "torch\n").unwrap();

        let paths = WorkerPaths {
            python_path,
            python_package_dir: dir.path().to_path_buf(),
            requirements_path,
            venv_dir: dir.path().join("venv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };

        let err = setup_python_env(&paths).await.unwrap_err();
        assert!(err.contains("Failed to parse Python version"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_setup_python_env_version_query_failure() {
        let dir = tempfile::tempdir().unwrap();
        let python_path = dir.path().join("fake_python");
        write_executable(
            &python_path,
            r#"#!/bin/sh
echo version-query-error 1>&2
exit 2
"#,
        );

        let requirements_path = dir.path().join("requirements.txt");
        std::fs::write(&requirements_path, "torch\n").unwrap();

        let paths = WorkerPaths {
            python_path,
            python_package_dir: dir.path().to_path_buf(),
            requirements_path,
            venv_dir: dir.path().join("venv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };

        let err = setup_python_env(&paths).await.unwrap_err();
        assert!(err.contains("Failed to query Python version"));
        assert!(err.contains("version-query-error"));
    }
}
