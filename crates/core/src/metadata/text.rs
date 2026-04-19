use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::types::{DocumentMetadata, FileType};

use super::doi::find_doi;
use super::FileMetadataExtractor;

const TEXT_HEAD_BYTES: usize = 64 * 1024;

pub struct TextMetadataExtractor {
    supported_extensions: Vec<String>,
}

impl TextMetadataExtractor {
    pub fn new(supported_extensions: Vec<String>) -> Self {
        Self {
            supported_extensions,
        }
    }
}

impl FileMetadataExtractor for TextMetadataExtractor {
    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        matches!(
            FileType::detect(path, &self.supported_extensions),
            Some(FileType::PlainText)
        )
    }

    fn extract_metadata(&self, path: &Path) -> anyhow::Result<DocumentMetadata> {
        let mut file = File::open(path)?;
        let mut buffer = vec![0_u8; TEXT_HEAD_BYTES];
        let len = file.read(&mut buffer)?;
        buffer.truncate(len);

        let head = String::from_utf8_lossy(&buffer);

        Ok(DocumentMetadata {
            title: None,
            author: None,
            doi: find_doi(&head),
            created_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_text_metadata_extractor_finds_doi_in_text_head() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("paper.md");
        fs::write(&path, "# Title\nDOI: 10.1000/xyz123\n").unwrap();

        let metadata = TextMetadataExtractor::new(vec!["md".into(), "txt".into()])
            .extract_metadata(&path)
            .unwrap();

        assert_eq!(metadata.doi.as_deref(), Some("10.1000/xyz123"));
        assert_eq!(metadata.title, None);
        assert_eq!(metadata.author, None);
    }

    #[test]
    fn test_text_metadata_extractor_returns_empty_when_no_doi_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("paper.txt");
        fs::write(&path, "hello").unwrap();

        let metadata = TextMetadataExtractor::new(vec!["txt".into()])
            .extract_metadata(&path)
            .unwrap();

        assert_eq!(metadata, DocumentMetadata::default());
    }
}
