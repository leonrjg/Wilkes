use std::path::{Component, Path, PathBuf};

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

/// The longest a single path component may be, in bytes.
///
/// `NAME_MAX` is 255 on every filesystem Wilkes writes to — APFS and HFS+,
/// ext4, NTFS — and the kernel counts *bytes*, not characters. A name that
/// crosses it is not truncated by the filesystem: the write fails outright,
/// with `ENAMETOOLONG` (errno 63 on macOS), after the bytes have already been
/// fetched. That is what a shadow-library filename does — title, authors,
/// journal, doi, hash and provenance in one segment, percent-encoded on top —
/// so the limit is enforced where the name is chosen rather than discovered
/// where the file is written.
pub const MAX_FILE_NAME_BYTES: usize = 255;

/// Below this, keeping the extension costs more than it is worth: a name that
/// is all suffix and no stem names nothing.
const MIN_STEM_BYTES: usize = 16;

/// One file name the operating system will accept.
///
/// Replaces what a name may not contain — control characters, the path
/// separators, and the set Windows reserves — and then holds the result to
/// [`MAX_FILE_NAME_BYTES`]. Both halves are the same question: whether the
/// name can be written at all. Answering only the first is what produced a
/// download that failed after fetching every byte of a 12 MB paper.
///
/// Errors rather than inventing a name when nothing usable is left, because a
/// caller that asked to save something under a name has no use for a different
/// one it never chose.
pub fn sanitize_file_name(raw: &str) -> Result<String, String> {
    let mut safe = String::new();
    for character in raw.trim().chars() {
        if character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            safe.push('_');
        } else {
            safe.push(character);
        }
    }
    // `..` is a traversal wherever it is read as a whole component, and a name
    // is joined to a directory by everything that calls this.
    while safe.contains("..") {
        safe = safe.replace("..", "_");
    }
    let safe = truncate_file_name(safe.trim(), MAX_FILE_NAME_BYTES);
    if safe.is_empty() {
        return Err("file name is empty after sanitizing".to_string());
    }
    Ok(safe)
}

/// Trims a name to `limit` bytes, keeping its extension.
///
/// The extension is kept because it is what tells the rest of Wilkes — the
/// importer, the extractor, the viewer — what the file is; the stem is only
/// what it is called. Characters are taken whole: a budget counted in bytes
/// and spent in glyphs is the only way to stay under a byte limit without
/// cutting through a multi-byte character.
fn truncate_file_name(name: &str, limit: usize) -> String {
    if name.len() <= limit {
        return name.to_string();
    }
    let suffix = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{extension}"))
        .filter(|suffix| suffix.len() + MIN_STEM_BYTES <= limit)
        .unwrap_or_default();
    let stem = name.strip_suffix(suffix.as_str()).unwrap_or(name);
    let budget = limit - suffix.len();
    let mut kept = String::with_capacity(budget);
    for character in stem.chars() {
        if kept.len() + character.len_utf8() > budget {
            break;
        }
        kept.push(character);
    }
    format!("{}{suffix}", kept.trim_end())
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

/// The interpreter `WILKES_PYTHON` names, when one is. Environments are then
/// built on it alone, instead of on a Python the app chooses and installs.
pub fn python_override() -> Option<PathBuf> {
    std::env::var_os("WILKES_PYTHON")
        .filter(|python| !python.is_empty())
        .map(PathBuf::from)
}

/// The `uv` that builds Python environments, shipped beside the executable.
/// `WILKES_UV` points at another one, for running outside a packaged build.
pub fn resolve_uv() -> anyhow::Result<PathBuf> {
    if let Some(uv) = std::env::var_os("WILKES_UV").filter(|uv| !uv.is_empty()) {
        return Ok(PathBuf::from(uv));
    }
    let name = format!("uv{}", std::env::consts::EXE_SUFFIX);
    Ok(std::env::current_exe()?.with_file_name(name))
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
    use tempfile::tempdir;

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
    fn test_resolve_python_package_dir_not_found() {
        let result = resolve_python_package_dir();
        assert!(result.is_err());
    }

    #[test]
    fn a_name_the_filesystem_would_refuse_is_cut_to_fit_and_keeps_its_extension() {
        // The live case: a shadow-library name carrying title, authors,
        // journal, date, doi and hash in one segment. The write failed with
        // errno 63 after the whole PDF had been fetched.
        let long = format!("{}.pdf", "a".repeat(400));
        let safe = sanitize_file_name(&long).unwrap();
        assert!(safe.len() <= MAX_FILE_NAME_BYTES, "{}", safe.len());
        assert!(safe.ends_with(".pdf"));
    }

    #[test]
    fn the_limit_is_bytes_and_a_glyph_is_never_cut_through() {
        // 200 accented characters are 400 bytes: a cap counted in characters
        // passes this name straight to a filesystem that refuses it.
        let long = format!("{}.pdf", "é".repeat(200));
        let safe = sanitize_file_name(&long).unwrap();
        assert!(safe.len() <= MAX_FILE_NAME_BYTES, "{}", safe.len());
        assert!(safe.chars().all(|c| c == 'é' || ".pdf".contains(c)));
        assert!(safe.ends_with(".pdf"));
    }

    #[test]
    fn a_name_that_fits_is_left_exactly_as_it_is() {
        assert_eq!(
            sanitize_file_name("Wilkes 1992 — Artificial Intelligence.pdf").unwrap(),
            "Wilkes 1992 — Artificial Intelligence.pdf"
        );
    }

    #[test]
    fn separators_and_traversal_do_not_survive() {
        assert_eq!(sanitize_file_name("../a/b.pdf").unwrap(), "__a_b.pdf");
        assert_eq!(
            sanitize_file_name("Graph: Networks.PDF").unwrap(),
            "Graph_ Networks.PDF"
        );
    }

    #[test]
    fn a_name_with_nothing_left_in_it_is_refused_rather_than_invented() {
        assert!(sanitize_file_name("   ").is_err());
    }

    #[test]
    fn an_extension_too_long_to_be_worth_keeping_is_not_kept() {
        // No stem would be left under it, so the name is simply cut to fit.
        let name = format!("a.{}", "x".repeat(400));
        let safe = sanitize_file_name(&name).unwrap();
        assert!(safe.len() <= MAX_FILE_NAME_BYTES, "{}", safe.len());
        assert!(safe.starts_with("a.xxx"));
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
