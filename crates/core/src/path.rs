use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Checks if `path` is contained within `base`.
///
/// This implementation uses canonicalization to resolve symlinks and '..' components,
/// ensuring that path traversal attacks are prevented even with complex paths.
///
/// If either path cannot be canonicalized (e.g. they don't exist), it returns false.
pub fn is_under(path: &Path, base: &Path) -> bool {
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let canonical_base = match base.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    canonical_path.starts_with(canonical_base)
}

/// Normalizes a path by resolving '..' and '.' components without hitting the disk.
/// Note: This does NOT resolve symlinks. It's useful for checking paths that
/// might not exist yet, or as a secondary check.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = path.components().peekable();
    let mut ret = if let Some(c @ Component::Prefix(..)) = components.peek() {
        let c = *c;
        components.next();
        PathBuf::from(c.as_os_str())
    } else {
        PathBuf::new()
    };

    for component in components {
        match component {
            Component::Prefix(..) => unreachable!(),
            Component::RootDir => {
                ret.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                ret.pop();
            }
            Component::Normal(c) => {
                ret.push(c);
            }
        }
    }
    ret
}

/// Resolves the Python 3 interpreter path.
///
/// It checks:
/// 1. `WILKES_PYTHON` environment variable.
/// 2. Common bundled paths relative to the current executable.
/// 3. The system PATH for `python3` or `python`.
fn python_probe_args() -> [&'static str; 2] {
    ["-c", "import sys; print(sys.executable)"]
}

#[cfg(windows)]
fn suppress_windows_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn suppress_windows_console(_command: &mut Command) {}

fn python_candidate_is_usable(path: &Path) -> bool {
    let mut command = Command::new(path);
    suppress_windows_console(&mut command);
    command
        .args(python_probe_args())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn resolve_python() -> anyhow::Result<PathBuf> {
    let mut attempted = Vec::new();

    // 1. Env override
    if let Ok(s) = std::env::var("WILKES_PYTHON") {
        if !s.is_empty() {
            let p = PathBuf::from(s);
            if p.exists() && python_candidate_is_usable(&p) {
                return Ok(p);
            }
            attempted.push(p);
        }
    }

    // 2. Bundled paths
    let exe = std::env::current_exe()?;
    let bundled = if cfg!(target_os = "macos") {
        exe.parent().and_then(|p| p.parent()).map(|p| {
            p.join("Resources")
                .join("python")
                .join("bin")
                .join("python3")
        })
    } else if cfg!(target_os = "windows") {
        exe.parent().map(|p| p.join("python").join("python.exe"))
    } else {
        // Linux / Docker
        exe.parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("lib").join("python").join("bin").join("python3"))
    };

    if let Some(ref p) = bundled {
        if p.exists() && python_candidate_is_usable(p) {
            return Ok(p.clone());
        }
        attempted.push(p.clone());
    }

    // 3. System PATH
    #[cfg(target_os = "windows")]
    let system_names = ["python", "py", "python3"];

    #[cfg(not(target_os = "windows"))]
    let system_names = ["python3", "python"];

    for name in system_names {
        if let Ok(p) = which::which(name) {
            if python_candidate_is_usable(&p) {
                return Ok(p);
            }
            attempted.push(p);
        }
    }

    let mut msg = "Python interpreter not found. Tried:\n".to_string();
    for p in attempted {
        msg.push_str(&format!("- {}\n", p.display()));
    }
    #[cfg(target_os = "windows")]
    msg.push_str("- system PATH (python, py, python3)\n");
    #[cfg(not(target_os = "windows"))]
    msg.push_str("- system PATH (python3, python)\n");
    anyhow::bail!("{}", msg);
}

/// Resolves the Python worker package directory.
pub fn resolve_python_package_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().unwrap_or(std::path::Path::new(""));

    #[cfg(target_os = "macos")]
    let mut candidates = vec![
        exe_dir.to_path_buf(),
        exe_dir.join("_up_").join("worker"),
        exe_dir.join("worker"),
    ];

    #[cfg(not(target_os = "macos"))]
    let candidates = vec![
        exe_dir.to_path_buf(),
        exe_dir.join("_up_").join("worker"),
        exe_dir.join("worker"),
    ];

    // In a macOS .app bundle the resources sit at ../Resources relative to the exe.
    // Checked first since it's the production layout; dev falls through to exe_dir above.
    #[cfg(target_os = "macos")]
    if let Some(p) = exe_dir.parent() {
        candidates.insert(0, p.join("Resources"));
    }

    candidates
        .into_iter()
        .find(|p| p.join("wilkes_python_worker").is_dir())
        .ok_or_else(|| anyhow::anyhow!("Python worker package not found (exe: {})", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tempfile::tempdir;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_fake_python(dir: &Path) -> PathBuf {
        write_named_fake_python(dir, "fake-python", true)
    }

    fn write_named_fake_python(dir: &Path, stem: &str, success: bool) -> PathBuf {
        #[cfg(windows)]
        let path = dir.join(format!("{stem}.cmd"));
        #[cfg(not(windows))]
        let path = dir.join(stem);

        #[cfg(windows)]
        {
            let body = if success {
                "@echo off\r\nexit /b 0\r\n"
            } else {
                "@echo off\r\nexit /b 1\r\n"
            };
            std::fs::write(&path, body).unwrap();
        }

        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;

            let body = if success {
                "#!/bin/sh\nexit 0\n"
            } else {
                "#!/bin/sh\nexit 1\n"
            };
            std::fs::write(&path, body).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }

        path
    }

    fn with_temp_env_var<T>(key: &str, value: Option<OsString>, f: impl FnOnce() -> T) -> T {
        let previous = std::env::var_os(key);
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        let result = f();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        result
    }

    #[test]
    fn test_normalize_path_more() {
        assert_eq!(
            normalize_path(Path::new("a/b/c/../../d")),
            PathBuf::from("a/d")
        );
        assert_eq!(
            normalize_path(Path::new("/a/b/../../c/d")),
            PathBuf::from("/c/d")
        );
        assert_eq!(normalize_path(Path::new("///a//b")), PathBuf::from("/a/b"));

        // Parent components at the start
        // normalize_path pops if it's ParentDir.
        // If it was empty, it stays empty.
        assert_eq!(normalize_path(Path::new("../a")), PathBuf::from("a"));
        assert_eq!(normalize_path(Path::new("/../a")), PathBuf::from("/a"));
        assert_eq!(normalize_path(Path::new(".")), PathBuf::from(""));
        assert_eq!(normalize_path(Path::new("./a/./b")), PathBuf::from("a/b"));
    }

    #[test]
    fn test_is_under_non_existent() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let non_existent = base.join("ghost");
        assert!(!is_under(&non_existent, base));
    }

    #[test]
    fn test_resolve_python_no_env_no_bundled() {
        // Clear environment and simulate no bundled python
        std::env::remove_var("WILKES_PYTHON");
        // We can't easily simulate "no system python" without breaking everything
        // but we can check it doesn't crash.
        let _ = resolve_python();
    }

    #[test]
    fn test_is_under_symlinks() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("base");
        std::fs::create_dir_all(&base).unwrap();

        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&sub, &link).unwrap();

        #[cfg(unix)]
        assert!(is_under(&link, &base));
    }

    #[test]
    fn test_is_under() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let sub = base.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(is_under(&sub, base));
        assert!(is_under(base, base));

        let outside = Path::new("/tmp/some_other_dir_12345");
        assert!(!is_under(outside, base));

        // Non-existent
        assert!(!is_under(&base.join("nonexistent"), base));
    }

    #[test]
    fn test_resolve_python_invalid_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("WILKES_PYTHON", "/tmp/nonexistent_python_12345");
        let result = resolve_python();
        // It might still succeed if it falls back to system path,
        // but we want to check that it didn't use the invalid env var immediately.
        if let Ok(p) = result {
            assert_ne!(p, PathBuf::from("/tmp/nonexistent_python_12345"));
        }
        std::env::remove_var("WILKES_PYTHON");
    }

    #[test]
    fn test_resolve_python_package_dir_not_found() {
        let result = resolve_python_package_dir();
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_python_with_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let python_path = write_fake_python(dir.path());
        std::env::set_var("WILKES_PYTHON", python_path.to_str().unwrap());

        let result = resolve_python();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), python_path);

        std::env::remove_var("WILKES_PYTHON");
    }

    #[test]
    fn test_python_candidate_is_usable_detects_failures() {
        let dir = tempdir().unwrap();
        let ok = write_named_fake_python(dir.path(), "python-ok", true);
        let bad = write_named_fake_python(dir.path(), "python-bad", false);

        assert!(python_candidate_is_usable(&ok));
        assert!(!python_candidate_is_usable(&bad));
    }

    #[test]
    fn test_resolve_python_skips_broken_path_candidate() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let broken = write_named_fake_python(dir.path(), "python3", false);
        let working = write_named_fake_python(dir.path(), "python", true);
        let path = std::env::join_paths([dir.path()]).unwrap();

        let resolved = with_temp_env_var("WILKES_PYTHON", None, || {
            with_temp_env_var("PATH", Some(path), || resolve_python())
        })
        .unwrap();

        assert_eq!(resolved, working);
        assert_ne!(resolved, broken);
    }

    #[test]
    fn test_is_under_traversal() {
        let base = Path::new("/a/b");
        let path = Path::new("/a/b/../c");
        assert!(!is_under(path, base));
    }

    #[test]
    fn test_is_under_base_non_existent() {
        let dir = tempdir().unwrap();
        let base = dir.path().join("ghost_base");
        let path = dir.path().join("some_file");
        std::fs::write(&path, "data").unwrap();
        assert!(!is_under(&path, &base));
    }
}
