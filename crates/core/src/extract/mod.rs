pub mod pdf;

use crate::types::ExtractedContent;
use std::path::Path;

pub trait ContentExtractor: Send + Sync {
    /// Returns true if this extractor can handle the given file.
    fn can_handle(&self, path: &Path, mime: Option<&str>) -> bool;

    /// Extract searchable text and a source map from the file.
    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent>;
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
    }

    #[test]
    fn test_extractor_registry() {
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(MockExtractor));

        assert!(registry.find(Path::new("test.mock"), None).is_some());
        assert!(registry.find(Path::new("test.txt"), None).is_none());
    }
}
