use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use wilkes_core::types::{ByteRange, FileEntry, FileType, PreviewData};

use super::preview::detect_language;

pub async fn list_files(root: PathBuf) -> anyhow::Result<Vec<FileEntry>> {
    tokio::task::spawn_blocking(move || {
        let mut entries = Vec::new();
        for result in WalkBuilder::new(&root).build() {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.file_type().map(|t: std::fs::FileType| t.is_file()).unwrap_or(false) {
                let path = entry.path().to_path_buf();
                let Some(file_type) = detect_file_type(&path) else {
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

pub async fn open_file(path: PathBuf) -> anyhow::Result<PreviewData> {
    match detect_file_type(&path) {
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

fn detect_file_type(path: &Path) -> Option<FileType> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext == "pdf" {
        return Some(FileType::Pdf);
    }
    const TEXT_EXTENSIONS: &[&str] = &[
        "txt", "md", "markdown", "rst", "rs", "py", "js", "ts", "jsx", "tsx",
        "json", "toml", "yaml", "yml", "xml", "html", "htm", "css", "scss",
        "sass", "less", "c", "cpp", "cc", "cxx", "h", "hpp", "java", "go",
        "rb", "sh", "bash", "zsh", "fish", "lua", "php", "swift", "kt",
        "cs", "r", "sql", "graphql", "gql", "proto", "ini", "cfg", "conf",
        "env", "gitignore", "lock", "log", "csv", "tsv", "jsonl",
    ];
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        Some(FileType::PlainText)
    } else {
        None
    }
}
