mod backend;
/// Which files this backend is allowed to read, and on what terms.
pub mod format;
/// The MuPDF reading of a document. `pub(crate)` for the two functions that
/// find a picture again in the file it was extracted from — see
/// [`crate::figure`], which is the only caller outside this module.
pub(crate) mod mupdf;
mod sanitize;
/// A book, rendered to pages the reader can draw.
pub mod surrogate;
/// Formulas and ruled tables the page draws rather than embeds.
///
/// `pub` for the three steps a probe must run rather than reimplement —
/// [`typeset::render_page`], the detector's own rasterization;
/// [`typeset::regions`] with the [`typeset::PageSurvey`] it claims against and
/// the [`typeset::TypesetRegion`]s it yields, which is what decides the crops a
/// recognizer is actually handed; and [`typeset::render`], which is those
/// crops' own pixels. Everything else stays `pub(super)`: a probe that drew
/// its own page, or claimed words by its own rule, would be measuring a
/// pipeline nobody runs.
pub mod typeset;

use std::path::Path;
use std::sync::Arc;

use backend::LayoutBackend;
use mupdf::MuPdfBackend;

use crate::extract::image::ImageAnalyzer;
use crate::types::{DeclaredOutline, ExtractedContent};

use super::ContentExtractor;

pub struct DocumentExtractor {
    backend: Box<dyn LayoutBackend>,
    /// The analyzer this extractor was built with, named in the extraction
    /// recipe. Empty when there is none, which is itself a recipe: a reading
    /// produced without a recognizer is a different reading, and mixing the
    /// two in one index would be exactly the drift the recipe exists to stop.
    analyzer_identity: String,
    /// The same analyzer the backend enriches with, held here so a caller
    /// that has finished with images can release it. A second handle to one
    /// analyzer, not a second analyzer: the backend does the enriching and
    /// this says when the recognizer behind it may be let go.
    analyzer: Option<Arc<dyn ImageAnalyzer>>,
}

impl DocumentExtractor {
    /// A PDF extractor that reads native text only. Native images are still
    /// found, digested and counted — what is absent is the enrichment, and
    /// the diagnostics say so.
    pub fn new() -> Self {
        Self {
            backend: Box::new(MuPdfBackend::default()),
            analyzer_identity: String::new(),
            analyzer: None,
        }
    }

    /// A PDF extractor that enriches native images with the given analyzer.
    pub fn with_image_analyzer(analyzer: Arc<dyn ImageAnalyzer>) -> Self {
        // The routing joins the analyzer in the recipe. Which areas of a page
        // are handed to the recognizer is as much a determinant of the bytes
        // as which recognizer reads them, and a change to it has to re-read
        // the library the same way a change of model does.
        let analyzer_identity = format!("{}+{}", typeset::ROUTING_VERSION, analyzer.identity());
        Self {
            backend: Box::new(MuPdfBackend::new(Some(Arc::clone(&analyzer)))),
            analyzer_identity,
            analyzer: Some(analyzer),
        }
    }
}

impl Default for DocumentExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentExtractor for DocumentExtractor {
    fn image_analyzer_identity(&self) -> &str {
        &self.analyzer_identity
    }

    fn release_image_analyzer(&self) {
        if let Some(analyzer) = &self.analyzer {
            analyzer.release();
        }
    }

    /// The closed list in [`format`], never `fz_recognize_document`.
    ///
    /// MuPDF recognizes far more than this — including `.txt`, `.html`,
    /// `.xml`, `.zip` and `.log`, every one of which Wilkes already reads as
    /// plain text with exact line and column origins. See that module for what
    /// admitting them would cost.
    fn can_handle(&self, path: &Path, _mime: Option<&str>) -> bool {
        format::PagedFormat::for_path(path).is_some()
    }

    /// This reading's contribution to the extraction recipe.
    ///
    /// Per format, not per backend: a change to how EPUBs are laid out must
    /// re-extract EPUBs and leave PDFs alone, and one identity covering both
    /// could do neither.
    fn recipe_identity(&self, path: &Path) -> Option<String> {
        format::PagedFormat::for_path(path).map(|format| format.recipe_identity())
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
        let extractor = DocumentExtractor::default();

        assert!(extractor.can_handle(Path::new("test.pdf"), None));
        assert!(extractor.can_handle(Path::new("TEST.PDF"), None));

        assert!(!extractor.can_handle(Path::new("test.txt"), None));
        assert!(!extractor.can_handle(Path::new("test"), None));
    }
}
