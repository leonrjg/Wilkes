use std::path::{Path, PathBuf, Component};

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
        let c = c.clone();
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
pub fn resolve_python() -> anyhow::Result<PathBuf> {
    let mut attempted = Vec::new();

    // 1. Env override
    if let Ok(s) = std::env::var("WILKES_PYTHON") {
        if !s.is_empty() {
            let p = PathBuf::from(s);
            if p.exists() {
                return Ok(p);
            }
            attempted.push(p);
        }
    }

    // 2. Bundled paths
    let exe = std::env::current_exe()?;
    let bundled = if cfg!(target_os = "macos") {
        exe.parent().and_then(|p| p.parent())
            .map(|p| p.join("Resources").join("python").join("bin").join("python3"))
    } else if cfg!(target_os = "windows") {
        exe.parent().map(|p| p.join("python").join("python.exe"))
    } else {
        // Linux / Docker
        exe.parent().and_then(|p| p.parent())
            .map(|p| p.join("lib").join("python").join("bin").join("python3"))
    };

    if let Some(ref p) = bundled {
        if p.exists() {
            return Ok(p.clone());
        }
        attempted.push(p.clone());
    }

    // 3. System PATH
    for name in &["python3", "python"] {
        if let Ok(p) = which::which(name) {
            return Ok(p);
        }
    }

    let mut msg = "Python interpreter not found. Tried:\n".to_string();
    for p in attempted {
        msg.push_str(&format!("- {}\n", p.display()));
    }
    msg.push_str("- system PATH (python3, python)\n");
    anyhow::bail!("{}", msg);
}

/// Resolves the Python worker package directory.
pub fn resolve_python_package_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let resource_dir = if cfg!(target_os = "macos") {
        exe.parent().and_then(|p| p.parent()).map(|p| p.join("Resources"))
    } else {
        exe.parent().map(|p| p.to_path_buf())
    }.ok_or_else(|| anyhow::anyhow!("Cannot determine resource directory"))?;

    let candidates = [
        resource_dir.clone(), 
        resource_dir.join("_up_").join("worker"),
        resource_dir.join("worker")
    ];
    
    candidates.into_iter()
        .find(|p| p.join("wilkes_python_worker").is_dir())
        .ok_or_else(|| anyhow::anyhow!(
            "Python worker package not found in {}", resource_dir.display()
        ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize_path(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize_path(Path::new("a/b/c/../..")), PathBuf::from("a"));
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
    }

    #[test]
    fn test_resolve_python_package_dir_not_found() {
        let result = resolve_python_package_dir();
        assert!(result.is_err());
    }
}
