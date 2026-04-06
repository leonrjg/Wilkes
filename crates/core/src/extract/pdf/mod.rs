mod backend;
mod mupdf;

use std::path::Path;

use backend::PdfBackend;
use mupdf::MuPdfBackend;

use crate::types::ExtractedContent;

use super::ContentExtractor;

pub struct PdfExtractor {
    backend: Box<dyn PdfBackend>,
}

impl PdfExtractor {
    pub fn new() -> Self {
        Self {
            backend: Box::new(MuPdfBackend),
        }
    }
}

impl Default for PdfExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentExtractor for PdfExtractor {
    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false)
    }

    fn extract(&self, path: &Path) -> anyhow::Result<ExtractedContent> {
        self.backend.extract(path)
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
