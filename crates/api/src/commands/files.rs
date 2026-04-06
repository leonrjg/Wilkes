use std::path::PathBuf;

use ignore::WalkBuilder;
use wilkes_core::types::{ByteRange, FileEntry, FileType, PreviewData};

use super::preview::detect_language;

pub async fn list_files(
    root: PathBuf,
    supported_extensions: Vec<String>,
    max_file_size: u64,
) -> anyhow::Result<Vec<FileEntry>> {
    tokio::task::spawn_blocking(move || {
        let mut entries = Vec::new();
        for result in WalkBuilder::new(&root).build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().map(|t: std::fs::FileType| t.is_file()).unwrap_or(false) {
                let path = entry.path().to_path_buf();

                // File size filter
                let meta = entry.metadata().ok();
                let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                if max_file_size > 0 && size_bytes > max_file_size {
                    continue;
                }

                let Some(file_type) = FileType::detect(&path, &supported_extensions) else {
                    continue;
                };
                let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                entries.push(FileEntry { path, size_bytes, file_type, extension });
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    })
    .await?
}

pub async fn open_file(path: PathBuf, supported_extensions: Vec<String>) -> anyhow::Result<PreviewData> {
    match FileType::detect(&path, &supported_extensions) {
        Some(FileType::Pdf) => Ok(PreviewData::Pdf { page: 1, highlight_bbox: None }),
        Some(FileType::PlainText) => {
            let content = tokio::fs::read_to_string(&path).await?;
            let language = detect_language(&path);
            Ok(PreviewData::Text {
                content,
                language,
                highlight_line: 0,
                highlight_range: ByteRange { start: 0, end: 0 },
            })
        }
        None => anyhow::bail!("unsupported file type: {}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[tokio::test]
    async fn test_list_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("test.txt"), "hello").unwrap();
        fs::write(root.join("test.pdf"), "pdf content").unwrap();
        fs::write(root.join("test.exe"), "executable").unwrap();

        let extensions = vec!["txt".to_string(), "pdf".to_string()];
        let files = list_files(root.to_path_buf(), extensions, 0).await.unwrap();

        assert_eq!(files.len(), 2);
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

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path.file_name().unwrap(), "small.txt");
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
    async fn test_open_file_unsupported() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.exe");
        fs::write(&path, "binary").unwrap();

        let extensions = vec!["txt".to_string()];
        let result = open_file(path, extensions).await;
        assert!(result.is_err());
    }
}
