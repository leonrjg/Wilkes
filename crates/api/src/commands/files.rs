use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::WalkBuilder;
use wilkes_core::types::{
    ByteRange, FileEntry, FileListResponse, FileType, OmittedFileEntry, OmittedFileReason,
    PreviewData,
};

use super::preview::detect_language;

pub async fn list_files(
    root: PathBuf,
    supported_extensions: Vec<String>,
    max_file_size: u64,
) -> anyhow::Result<FileListResponse> {
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        let mut omitted = Vec::new();
        for result in WalkBuilder::new(&root).build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry
                .file_type()
                .map(|t: std::fs::FileType| t.is_file())
                .unwrap_or(false)
            {
                let path = entry.path().to_path_buf();
                let extension = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();

                // File size filter
                let meta = entry.metadata().ok();
                let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let created_at_ms = meta
                    .as_ref()
                    .and_then(|m| m.created().ok())
                    .and_then(system_time_ms);
                let modified_at_ms = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(system_time_ms);
                let file_type = FileType::detect(&path, &supported_extensions);

                if file_type.is_none() {
                    omitted.push(OmittedFileEntry {
                        file: FileEntry {
                            path,
                            size_bytes,
                            file_type: FileType::PlainText,
                            extension,
                            created_at_ms,
                            modified_at_ms,
                            title: None,
                            author: None,
                            publication_date: None,
                            citation_count: None,
                            metadata_conflicts: Default::default(),
                        },
                        reason: OmittedFileReason::UnsupportedExtension,
                    });
                    continue;
                }
                let file_type = file_type.expect("checked is_some above");

                if max_file_size > 0 && size_bytes > max_file_size {
                    omitted.push(OmittedFileEntry {
                        file: FileEntry {
                            path,
                            size_bytes,
                            file_type,
                            extension,
                            created_at_ms,
                            modified_at_ms,
                            title: None,
                            author: None,
                            publication_date: None,
                            citation_count: None,
                            metadata_conflicts: Default::default(),
                        },
                        reason: OmittedFileReason::TooLarge,
                    });
                    continue;
                }

                files.push(FileEntry {
                    path,
                    size_bytes,
                    file_type,
                    extension,
                    created_at_ms,
                    modified_at_ms,
                    title: None,
                    author: None,
                    publication_date: None,
                    citation_count: None,
                    metadata_conflicts: Default::default(),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        omitted.sort_by(|a, b| a.file.path.cmp(&b.file.path));
        Ok(FileListResponse { files, omitted })
    })
    .await?
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    let millis = time.duration_since(UNIX_EPOCH).ok()?.as_millis();
    i64::try_from(millis).ok()
}

pub async fn open_file(
    path: PathBuf,
    _supported_extensions: Vec<String>,
) -> anyhow::Result<PreviewData> {
    match viewer_file_type(&path) {
        FileType::Pdf => Ok(PreviewData::Pdf {
            page: 1,
            highlight_bbox: None,
        }),
        FileType::PlainText => {
            let content =
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|err| match err.kind() {
                        std::io::ErrorKind::InvalidData => {
                            anyhow::anyhow!(
                                "Cannot preview non-UTF-8 text file: {}",
                                path.display()
                            )
                        }
                        _ => anyhow::Error::from(err),
                    })?;
            let language = detect_language(&path);
            Ok(PreviewData::Text {
                content,
                language,
                highlight_line: 0,
                highlight_range: ByteRange { start: 0, end: 0 },
            })
        }
    }
}

fn viewer_file_type(path: &Path) -> FileType {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("pdf"))
    {
        Some(true) => FileType::Pdf,
        _ => FileType::PlainText,
    }
}

pub async fn rename_file(path: PathBuf, new_name: String) -> anyhow::Result<PathBuf> {
    let new_name = new_name.trim();
    validate_new_file_name(new_name)?;

    let metadata = tokio::fs::metadata(&path).await?;
    if !metadata.is_file() {
        anyhow::bail!("Can only rename files");
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("File has no containing directory"))?;
    let target = parent.join(new_name);

    if tokio::fs::try_exists(&target).await? {
        anyhow::bail!("A file or folder with that name already exists");
    }

    tokio::fs::rename(&path, &target).await?;
    Ok(target)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileImportMode {
    Move,
    Copy,
}

pub async fn import_files_into_root(
    paths: Vec<PathBuf>,
    root: PathBuf,
    supported_extensions: Vec<String>,
    mode: FileImportMode,
) -> anyhow::Result<Vec<PathBuf>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let root = tokio::fs::canonicalize(&root)
        .await
        .map_err(|err| anyhow::anyhow!("Root directory not found: {} ({err})", root.display()))?;
    let root_meta = tokio::fs::metadata(&root).await?;
    if !root_meta.is_dir() {
        anyhow::bail!("Root is not a directory: {}", root.display());
    }

    let mut imports = Vec::with_capacity(paths.len());
    let mut targets = HashSet::with_capacity(paths.len());
    for path in paths {
        let source = tokio::fs::canonicalize(&path).await.map_err(|err| {
            anyhow::anyhow!("File to import not found: {} ({err})", path.display())
        })?;
        let metadata = tokio::fs::metadata(&source).await?;
        if !metadata.is_file() {
            anyhow::bail!("Can only import files: {}", source.display());
        }
        if FileType::detect(&source, &supported_extensions).is_none() {
            anyhow::bail!("File type is not supported: {}", source.display());
        }
        let file_name = source.file_name().ok_or_else(|| {
            anyhow::anyhow!("File to import has no file name: {}", source.display())
        })?;
        let target = root.join(file_name);
        if source == target {
            anyhow::bail!("File is already in the current root: {}", source.display());
        }
        if tokio::fs::try_exists(&target).await? {
            anyhow::bail!(
                "A file or folder with that name already exists: {}",
                target.display()
            );
        }
        if !targets.insert(target.clone()) {
            anyhow::bail!(
                "Multiple files would import as the same name: {}",
                target.display()
            );
        }
        imports.push((source, target));
    }

    let mut imported = Vec::with_capacity(imports.len());
    for (source, target) in imports {
        match mode {
            FileImportMode::Move => tokio::fs::rename(&source, &target).await?,
            FileImportMode::Copy => {
                tokio::fs::copy(&source, &target).await?;
            }
        }
        imported.push(target);
    }

    Ok(imported)
}

pub async fn move_files_into_root(
    paths: Vec<PathBuf>,
    root: PathBuf,
    supported_extensions: Vec<String>,
) -> anyhow::Result<Vec<PathBuf>> {
    import_files_into_root(paths, root, supported_extensions, FileImportMode::Move).await
}

fn validate_new_file_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("File name cannot be empty");
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        anyhow::bail!("File name cannot contain path separators");
    }
    if name == "." || name == ".." {
        anyhow::bail!("Invalid file name");
    }
    if !matches!(
        PathBuf::from(name).components().next(),
        Some(Component::Normal(_))
    ) {
        anyhow::bail!("Invalid file name");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_list_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("test.txt"), "hello").unwrap();
        fs::write(root.join("test.pdf"), "pdf content").unwrap();
        fs::write(root.join("test.exe"), "executable").unwrap();

        let extensions = vec!["txt".to_string(), "pdf".to_string()];
        let files = list_files(root.to_path_buf(), extensions, 0).await.unwrap();

        assert_eq!(files.files.len(), 2);
        assert_eq!(files.omitted.len(), 1);
        assert!(files
            .files
            .iter()
            .all(|entry| entry.modified_at_ms.is_some()));
        assert_eq!(
            files.omitted[0].reason,
            OmittedFileReason::UnsupportedExtension
        );
    }

    #[tokio::test]
    async fn test_list_files_size_filter() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("small.txt"), "ok").unwrap();
        fs::write(root.join("large.txt"), "this is much larger").unwrap();

        let extensions = vec!["txt".to_string()];
        // Filter to 5 bytes
        let files = list_files(root.to_path_buf(), extensions, 5).await.unwrap();

        assert_eq!(files.files.len(), 1);
        assert_eq!(files.files[0].path.file_name().unwrap(), "small.txt");
        assert_eq!(files.omitted.len(), 1);
        assert_eq!(files.omitted[0].file.path.file_name().unwrap(), "large.txt");
        assert_eq!(files.omitted[0].reason, OmittedFileReason::TooLarge);
    }

    #[tokio::test]
    async fn test_open_file_text() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();

        let extensions = vec!["txt".to_string()];
        let preview = open_file(path, extensions).await.unwrap();

        match preview {
            PreviewData::Text { content, .. } => assert_eq!(content, "hello world"),
            _ => panic!("Expected Text preview"),
        }
    }

    #[tokio::test]
    async fn test_open_file_unsupported_extension_as_text() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.log");
        fs::write(&path, "plain text").unwrap();

        let extensions = vec!["txt".to_string()];
        let preview = open_file(path, extensions).await.unwrap();

        match preview {
            PreviewData::Text { content, .. } => assert_eq!(content, "plain text"),
            _ => panic!("Expected Text preview"),
        }
    }

    #[tokio::test]
    async fn test_open_file_non_utf8_text_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.bin");
        fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();

        let extensions = vec!["txt".to_string()];
        let result = open_file(path, extensions).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Cannot preview non-UTF-8 text file"));
    }

    #[tokio::test]
    async fn test_open_file_pdf() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.pdf");
        fs::write(&path, "fake pdf").unwrap();

        let extensions = vec!["pdf".to_string()];
        let preview = open_file(path, extensions).await.unwrap();

        match preview {
            PreviewData::Pdf { page, .. } => assert_eq!(page, 1),
            _ => panic!("Expected Pdf preview"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_list_files_skips_walk_errors() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let root = dir.path();
        let bad_dir = root.join("nope");
        fs::create_dir(&bad_dir).unwrap();
        fs::write(root.join("ok.txt"), "hello").unwrap();

        let mut perms = fs::metadata(&bad_dir).unwrap().permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&bad_dir, perms).unwrap();

        let extensions = vec!["txt".to_string()];
        let files = list_files(root.to_path_buf(), extensions, 0).await.unwrap();

        assert!(files
            .files
            .iter()
            .any(|entry| entry.path.ends_with("ok.txt")));

        let mut perms = fs::metadata(&bad_dir).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bad_dir, perms).unwrap();
    }

    #[tokio::test]
    async fn test_rename_file_renames_within_parent_directory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.txt");
        fs::write(&path, "hello").unwrap();

        let renamed = rename_file(path.clone(), "new.txt".into()).await.unwrap();

        assert_eq!(renamed, dir.path().join("new.txt"));
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(renamed).unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_rename_file_rejects_path_names_and_existing_targets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.txt");
        fs::write(&path, "hello").unwrap();
        fs::write(dir.path().join("taken.txt"), "existing").unwrap();

        let err = rename_file(path.clone(), "../new.txt".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("path separators"));

        let err = rename_file(path, "taken.txt".into()).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_move_files_into_root_moves_supported_files() {
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("paper.pdf");
        fs::write(&source, "pdf").unwrap();

        let imported = move_files_into_root(
            vec![source.clone()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap();

        let target = root_dir.path().join("paper.pdf");
        assert_eq!(imported, vec![target.canonicalize().unwrap()]);
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(target).unwrap(), "pdf");
    }

    #[tokio::test]
    async fn test_import_files_into_root_copies_supported_files() {
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("paper.pdf");
        fs::write(&source, "pdf").unwrap();

        let imported = import_files_into_root(
            vec![source.clone()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
            FileImportMode::Copy,
        )
        .await
        .unwrap();

        let target = root_dir.path().join("paper.pdf");
        assert_eq!(imported, vec![target.canonicalize().unwrap()]);
        assert_eq!(fs::read_to_string(&source).unwrap(), "pdf");
        assert_eq!(fs::read_to_string(target).unwrap(), "pdf");
    }

    #[tokio::test]
    async fn test_move_files_into_root_rejects_unsupported_files() {
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("program.exe");
        fs::write(&source, "binary").unwrap();

        let err = move_files_into_root(
            vec![source.clone()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("not supported"));
        assert!(source.exists());
    }

    #[tokio::test]
    async fn test_move_files_into_root_rejects_existing_target() {
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source = source_dir.path().join("paper.pdf");
        fs::write(&source, "new").unwrap();
        fs::write(root_dir.path().join("paper.pdf"), "existing").unwrap();

        let err = move_files_into_root(
            vec![source.clone()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("already exists"));
        assert!(source.exists());
        assert_eq!(
            fs::read_to_string(root_dir.path().join("paper.pdf")).unwrap(),
            "existing"
        );
    }

    #[tokio::test]
    async fn test_move_files_into_root_rejects_duplicate_import_names_before_moving() {
        let source_dir_1 = tempdir().unwrap();
        let source_dir_2 = tempdir().unwrap();
        let root_dir = tempdir().unwrap();
        let source_1 = source_dir_1.path().join("paper.pdf");
        let source_2 = source_dir_2.path().join("paper.pdf");
        fs::write(&source_1, "one").unwrap();
        fs::write(&source_2, "two").unwrap();

        let err = move_files_into_root(
            vec![source_1.clone(), source_2.clone()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("same name"));
        assert!(source_1.exists());
        assert!(source_2.exists());
        assert!(!root_dir.path().join("paper.pdf").exists());
    }

    #[tokio::test]
    async fn test_move_files_into_root_rejects_directories_and_missing_roots() {
        let source_dir = tempdir().unwrap();
        let root_dir = tempdir().unwrap();

        let err = move_files_into_root(
            vec![source_dir.path().to_path_buf()],
            root_dir.path().to_path_buf(),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Can only import files"));

        let source = source_dir.path().join("paper.pdf");
        fs::write(&source, "pdf").unwrap();
        let err = move_files_into_root(
            vec![source],
            root_dir.path().join("missing"),
            vec!["pdf".to_string()],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Root directory not found"));
    }
}
