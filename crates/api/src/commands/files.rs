use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use wilkes_core::types::{ByteRange, FileEntry, FileType, PreviewData};

use super::preview::detect_language;

pub async fn list_files(root: PathBuf, supported_extensions: Vec<String>) -> anyhow::Result<Vec<FileEntry>> {
    tokio::task::spawn_blocking(move || {
        let mut entries = Vec::new();
        for result in WalkBuilder::new(&root).build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().map(|t: std::fs::FileType| t.is_file()).unwrap_or(false) {
                let path = entry.path().to_path_buf();
                let Some(file_type) = FileType::detect(&path, &supported_extensions) else {
                    continue;
                };
                let size_bytes = entry.metadata().ok().map(|m| m.len()).unwrap_or(0);
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
