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
