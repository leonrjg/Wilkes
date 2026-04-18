use std::path::PathBuf;

use wilkes_core::metadata::pdf::PdfMetadataExtractor;
use wilkes_core::metadata::text::TextMetadataExtractor;
use wilkes_core::metadata::MetadataExtractorRegistry;
use wilkes_core::types::DocumentMetadata;

pub async fn get_file_metadata(
    path: PathBuf,
    supported_extensions: Vec<String>,
) -> anyhow::Result<DocumentMetadata> {
    tokio::task::spawn_blocking(move || {
        let registry = build_registry(supported_extensions);

        match registry.extract_for(&path, None) {
            Ok(metadata) => Ok(metadata),
            Err(_) => Ok(DocumentMetadata::default()),
        }
    })
    .await?
}

fn build_registry(supported_extensions: Vec<String>) -> MetadataExtractorRegistry {
    let mut registry = MetadataExtractorRegistry::new();
    registry.register(Box::new(PdfMetadataExtractor));
    registry.register(Box::new(TextMetadataExtractor::new(supported_extensions)));
    registry
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn test_get_file_metadata_returns_empty_for_unsupported_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        fs::write(&path, "hello").unwrap();

        let metadata = get_file_metadata(path, vec![]).await.unwrap();
        assert_eq!(metadata, DocumentMetadata::default());
    }

    #[tokio::test]
    async fn test_get_file_metadata_does_not_fail_for_invalid_pdf() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.pdf");
        fs::write(&path, "not a pdf").unwrap();

        let metadata = get_file_metadata(path, vec!["pdf".into()]).await.unwrap();
        assert_eq!(metadata, DocumentMetadata::default());
    }

    #[tokio::test]
    async fn test_get_file_metadata_extracts_doi_from_text_when_supported() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("notes.md");
        fs::write(&path, "doi:10.1000/xyz123").unwrap();

        let metadata = get_file_metadata(path, vec!["md".into()]).await.unwrap();
        assert_eq!(metadata.doi.as_deref(), Some("10.1000/xyz123"));
    }
}
