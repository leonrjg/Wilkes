pub mod image;
pub mod outline;
pub mod pdf;

use crate::types::{DeclaredOutline, ExtractedContent};
use std::path::Path;

pub trait ContentExtractor: Send + Sync {
    /// Returns true if this extractor can handle the given file.
    fn can_handle(&self, path: &Path, mime: Option<&str>) -> bool;

    /// Extract searchable text and a source map from the file.
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent>;

    /// The document's declared table of contents, empty when it declares none,
    /// with each entry anchored in the reading `extract` produces.
    ///
    /// Required rather than defaulted: a format whose outline nobody
    /// implemented would otherwise report "this document has no structure",
    /// which is a different claim and one a consumer cannot tell from the
    /// truth. Callers ask for the outline *without* wanting the text (the
    /// chunk export already holds it), which is why this is its own method —
    /// but an anchored entry is an offset into the reading, so an
    /// implementation of a paginated format has to produce that reading to
    /// answer, and costs accordingly.
    fn outline(&self, path: &Path) -> anyhow::Result<DeclaredOutline>;
}

/// The declared outline of one file, dispatched exactly as extraction is: the
/// registry's extractor where there is one, the plain-text reading where there
/// is not (`SemanticIndex::extract_content` makes the same two choices, and the
/// two must not disagree about what a file is).
pub fn document_outline(
    path: &Path,
    extractors: &ExtractorRegistry,
) -> anyhow::Result<DeclaredOutline> {
    match extractors.find(path, None) {
        Some(extractor) => extractor.outline(path),
        None => {
            let text = std::fs::read_to_string(path)?;
            Ok(DeclaredOutline {
                entries: outline::markdown_outline(&text),
                // A text file is not paginated, has no margin columns and no
                // running heads: there is nothing here for sanitation to
                // decide, and reporting zeros says exactly that.
                diagnostics: Default::default(),
            })
        }
    }
}

pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn ContentExtractor>>,
}

impl ExtractorRegistry {
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }

    pub fn register(&mut self, extractor: Box<dyn ContentExtractor>) {
        self.extractors.push(extractor);
    }

    /// Returns the first extractor that can handle the file, or None.
    /// Priority: registration order (register more specific extractors first).
    pub fn find(&self, path: &Path, mime: Option<&str>) -> Option<&dyn ContentExtractor> {
        self.extractors
            .iter()
            .find(|e| e.can_handle(path, mime))
            .map(|e| e.as_ref())
    }
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockExtractor;
    impl ContentExtractor for MockExtractor {
        fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("mock")
        }
        fn extract(&self, _path: &Path) -> anyhow::Result<ExtractedContent> {
            anyhow::bail!("mock")
        }
        fn outline(&self, _path: &Path) -> anyhow::Result<DeclaredOutline> {
            Ok(DeclaredOutline::default())
        }
    }

    #[test]
    fn test_extractor_registry() {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(MockExtractor));

        assert!(registry.find(Path::new("test.mock"), None).is_some());
        assert!(registry.find(Path::new("test.txt"), None).is_none());
    }

    struct MimeExtractor;
    impl ContentExtractor for MimeExtractor {
        fn can_handle(&self, _path: &Path, mime: Option<&str>) -> bool {
            mime == Some("text/plain")
        }
        fn extract(&self, _path: &Path) -> anyhow::Result<ExtractedContent> {
            anyhow::bail!("mime")
        }
        fn outline(&self, _path: &Path) -> anyhow::Result<DeclaredOutline> {
            Ok(DeclaredOutline::default())
        }
    }

    #[test]
    fn test_extractor_registry_priority_and_mime() {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(MockExtractor));
        registry.register(Box::new(MimeExtractor));

        // Extension match
        assert!(registry.find(Path::new("test.mock"), None).is_some());

        // MIME match
        assert!(registry
            .find(Path::new("test.txt"), Some("text/plain"))
            .is_some());

        // No match
        assert!(registry
            .find(Path::new("test.txt"), Some("image/png"))
            .is_none());
    }

    #[test]
    fn test_extractor_registry_default() {
        let registry = ExtractorRegistry::default();
        assert!(registry.extractors.is_empty());
    }
}
