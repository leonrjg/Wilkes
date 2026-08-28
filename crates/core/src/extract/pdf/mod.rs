mod backend;
mod mupdf;
mod sanitize;

use std::path::Path;
use std::sync::Arc;

use backend::PdfBackend;
use mupdf::MuPdfBackend;

use crate::extract::image::ImageAnalyzer;
use crate::types::{DeclaredOutline, ExtractedContent};

use super::ContentExtractor;

pub struct PdfExtractor {
    backend: Box<dyn PdfBackend>,
    /// The analyzer this extractor was built with, named in the extraction
    /// recipe. Empty when there is none, which is itself a recipe: a reading
    /// produced without a recognizer is a different reading, and mixing the
    /// two in one index would be exactly the drift the recipe exists to stop.
    analyzer_identity: String,
}

impl PdfExtractor {
    /// A PDF extractor that reads native text only. Native images are still
    /// found, digested and counted — what is absent is the enrichment, and
    /// the diagnostics say so.
    pub fn new() -> Self {
        Self {
            backend: Box::new(MuPdfBackend::default()),
            analyzer_identity: String::new(),
        }
    }

    /// A PDF extractor that enriches native images with the given analyzer.
    pub fn with_image_analyzer(analyzer: Arc<dyn ImageAnalyzer>) -> Self {
        let analyzer_identity = analyzer.identity();
        Self {
            backend: Box::new(MuPdfBackend::new(Some(analyzer))),
            analyzer_identity,
        }
    }

}

impl Default for PdfExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentExtractor for PdfExtractor {
    fn image_analyzer_identity(&self) -> &str {
        &self.analyzer_identity
    }

    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
    }

    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        self.backend.extract(path)
    }

    fn outline(&self, path: &Path) -> anyhow::Result<DeclaredOutline> {
        self.backend.outline(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pdf_extractor_can_handle() {
        let extractor = PdfExtractor::default();

        assert!(extractor.can_handle(Path::new("test.pdf"), None));
        assert!(extractor.can_handle(Path::new("TEST.PDF"), None));

        assert!(!extractor.can_handle(Path::new("test.txt"), None));
        assert!(!extractor.can_handle(Path::new("test"), None));
    }
}
