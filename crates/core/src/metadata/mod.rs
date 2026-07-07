pub mod arxiv;
pub mod cache;
pub mod doi;
pub mod pdf;
pub mod text;

use std::path::Path;

use crate::types::DocumentMetadata;

/// Canonical document metadata fields stored in `file_metadata.key`.
///
/// Provider-specific contracts are adapted into these names at the cache
/// boundary. The initial vocabulary follows the Semantic Scholar paper fields
/// because it was the first external metadata provider, with file/Zotero fields
/// folded into the same key space where they overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataField {
    Title,
    Author,
    Doi,
    PublicationDate,
    PaperId,
    Year,
    Venue,
    CitationCount,
    ExternalIdsJson,
    CachedAtMs,
    /// Internal cache bookkeeping field. Stored alongside provider fields but
    /// not part of a provider contract.
    ExtractedAtMs,
}

impl MetadataField {
    pub const fn as_str(self) -> &'static str {
        match self {
            MetadataField::Title => "title",
            MetadataField::Author => "author",
            MetadataField::Doi => "doi",
            MetadataField::PublicationDate => "publication_date",
            MetadataField::PaperId => "paper_id",
            MetadataField::Year => "year",
            MetadataField::Venue => "venue",
            MetadataField::CitationCount => "citation_count",
            MetadataField::ExternalIdsJson => "external_ids_json",
            MetadataField::CachedAtMs => "cached_at_ms",
            MetadataField::ExtractedAtMs => "extracted_at_ms",
        }
    }
}

pub trait FileMetadataExtractor: Send + Sync {
    fn can_handle(&self, path: &Path, mime: Option<&str>) -> bool;
    fn extract_metadata(&self, path: &Path) -> anyhow::Result<DocumentMetadata>;
}

pub struct MetadataExtractorRegistry {
    extractors: Vec<Box<dyn FileMetadataExtractor>>,
}

impl MetadataExtractorRegistry {
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }

    pub fn register(&mut self, extractor: Box<dyn FileMetadataExtractor>) {
        self.extractors.push(extractor);
    }

    pub fn find(&self, path: &Path, mime: Option<&str>) -> Option<&dyn FileMetadataExtractor> {
        self.extractors
            .iter()
            .find(|e| e.can_handle(path, mime))
            .map(|e| e.as_ref())
    }

    pub fn extract_for(&self, path: &Path, mime: Option<&str>) -> anyhow::Result<DocumentMetadata> {
        match self.find(path, mime) {
            Some(extractor) => extractor.extract_metadata(path),
            None => Ok(DocumentMetadata::default()),
        }
    }
}

impl Default for MetadataExtractorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::text::TextMetadataExtractor;

    struct MockExtractor;

    impl FileMetadataExtractor for MockExtractor {
        fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
            path.extension().and_then(|ext| ext.to_str()) == Some("mock")
        }

        fn extract_metadata(&self, _path: &Path) -> anyhow::Result<DocumentMetadata> {
            Ok(DocumentMetadata {
                title: Some("Mock Title".into()),
                author: None,
                doi: None,
                created_at: None,
                ..DocumentMetadata::default()
            })
        }
    }

    #[test]
    fn test_registry_finds_matching_extractor() {
        let mut registry = MetadataExtractorRegistry::new();
        registry.register(Box::new(MockExtractor));

        let metadata = registry.extract_for(Path::new("paper.mock"), None).unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Mock Title"));
    }

    #[test]
    fn test_registry_returns_empty_metadata_when_unsupported() {
        let registry = MetadataExtractorRegistry::new();

        let metadata = registry.extract_for(Path::new("paper.txt"), None).unwrap();
        assert_eq!(metadata, DocumentMetadata::default());
    }

    #[test]
    fn test_registry_uses_registered_text_extractor() {
        let mut registry = MetadataExtractorRegistry::new();
        registry.register(Box::new(TextMetadataExtractor::new(vec!["txt".into()])));

        assert!(registry.find(Path::new("paper.txt"), None).is_some());
    }
}
