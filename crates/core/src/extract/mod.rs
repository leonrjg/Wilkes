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

    /// The image analyzer this extractor enriches native images with, or an
    /// empty string when it does not enrich them at all.
    ///
    /// Defaulted rather than required: for a format with no embedded images
    /// there is nothing to declare, and an empty answer is the true one.
    fn image_analyzer_identity(&self) -> &str {
        ""
    }

    /// Let go of whatever this extractor's image analyzer keeps resident.
    ///
    /// The caller is a pass that has finished with images and is about to
    /// spend as long again on something else — the index build, which reads
    /// every figure before it embeds anything. Extraction still works
    /// afterwards; a later read reloads.
    ///
    /// Defaulted for the same reason as the identity above: a format with no
    /// embedded images holds no analyzer, and having nothing to release is
    /// the true answer rather than an omission.
    fn release_image_analyzer(&self) {}

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

/// The extractors Wilkes reads documents with, and the only place that says
/// what they are.
///
/// Every consumer that *produces* a rendition — indexing, watcher updates,
/// MCP reads, summaries, export — goes through a registry built here, with
/// the process's one configured analyzer ([`image::configure`]). Assembling a
/// registry at a call site is how two consumers come to disagree about what a
/// document says: one enriches its images and one does not, and both write
/// their answer into the same index under recipes that differ. There is no
/// parameter here for the same reason — a caller that could pass a different
/// analyzer would eventually be one that did.
///
/// Exact search is not one of them, and reads with
/// [`exact_search_registry`] instead; the two callers are separated there
/// rather than here, so this stays the only place that names the configured
/// analyzer.
pub fn production_registry() -> ExtractorRegistry {
    let mut registry = ExtractorRegistry::new();
    registry.register(match image::configured() {
        Some(analyzer) => Box::new(pdf::PdfExtractor::with_image_analyzer(analyzer)),
        None => Box::new(pdf::PdfExtractor::new()),
    });
    registry
}

/// The extractors exact search reads a PDF with when the index cannot serve
/// it: the page's own glyphs, and no enrichment, whatever the process's
/// configured analyzer is.
///
/// **Why not [`production_registry`].** Enriching a page means detecting its
/// layout and recognizing the areas that detection marks out — a document's
/// worth of inference, minutes for a textbook. Exact search asked for that
/// once per document it could not serve from the index, from inside the
/// `rayon` loop over the corpus in [`crate::search::grep`], and so a search
/// over a library whose index was narrower than the search's scope became an
/// enrichment pass wearing a search's clothes: the log filled with
/// PP-DocLayoutV2, and the search did not return.
///
/// That loop is also the shape the worker invariant forbids: a host loop that
/// submits per item is a loop that starts a worker per item, and a cancel
/// kills exactly one of them. The unit here cannot be moved into the worker —
/// the loop is over the corpus, not over one document's pages — so the only
/// way to hold the invariant is for a search to submit nothing at all.
///
/// **What it costs.** A PDF the index does not hold is searched for the text
/// it typesets, not for text that exists only inside its pictures. That is
/// the reading [`pdf::PdfExtractor::new`] documents, the count of files that
/// took it is `live_pdf_fallbacks` in the search outcome, and the way to
/// search a picture's text is to index the file — which is what the index is
/// for.
pub fn exact_search_registry() -> ExtractorRegistry {
    let mut registry = ExtractorRegistry::new();
    registry.register(Box::new(pdf::PdfExtractor::new()));
    registry
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
    image_analyzer_identity: String,
}

impl ExtractorRegistry {
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
            image_analyzer_identity: String::new(),
        }
    }

    pub fn register(&mut self, extractor: Box<dyn ContentExtractor>) {
        self.image_analyzer_identity = self
            .image_analyzer_identity
            .clone()
            .max(extractor.image_analyzer_identity().to_string());
        self.extractors.push(extractor);
    }

    /// The image analyzer this registry's extractors enrich with, or empty
    /// when none of them does.
    ///
    /// Part of the extraction recipe, which is why it lives on the registry
    /// rather than being asked of an extractor per call: the recipe describes
    /// what *this* configuration produces, and a registry is the unit a
    /// configuration is expressed in.
    pub fn image_analyzer_identity(&self) -> &str {
        &self.image_analyzer_identity
    }

    /// Returns the first extractor that can handle the file, or None.
    /// Priority: registration order (register more specific extractors first).
    pub fn find(&self, path: &Path, mime: Option<&str>) -> Option<&dyn ContentExtractor> {
        self.extractors
            .iter()
            .find(|e| e.can_handle(path, mime))
            .map(|e| e.as_ref())
    }

    /// Tell every extractor that enriches images that the images are done for
    /// now, so the recognizer behind them can be let go.
    ///
    /// On the registry rather than reached for through
    /// [`crate::extract::image::configured`], because a caller holding a
    /// registry is reading documents *with that registry*, and releasing some
    /// other process-wide analyzer would be releasing something else.
    pub fn release_image_analyzers(&self) {
        for extractor in &self.extractors {
            extractor.release_image_analyzer();
        }
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

    /// The registry reports the analyzer its extractors enrich with, because
    /// the extraction recipe is derived from it. A registry with no analyzer
    /// declares nothing, which is what keeps a runtime that never had one
    /// from re-extracting its whole library.
    #[test]
    fn the_registry_reports_the_analyzer_its_extractors_enrich_with() {
        // Reads the process-wide analyzer, so it takes the same guard as the
        // tests that move it.
        let _serialized = image::CONFIGURED_ANALYZER_TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let plain = production_registry();
        assert_eq!(plain.image_analyzer_identity(), "");

        struct Named;
        impl image::ImageAnalyzer for Named {
            fn layout(&self) -> Option<&dyn image::LayoutModel> {
                None
            }
            fn release(&self) {}
            // A test analyzer reads whatever it is handed; the scope is the
            // configuration's decision and these fixtures are not testing it.
            fn reads_embedded_images(&self) -> bool {
                true
            }
            fn identity(&self) -> String {
                "named-analyzer-v1".to_string()
            }
            fn analyze(
                &self,
                _images: &mut [crate::types::ExtractedImage],
                _discovered: &[image::DiscoveredImage],
                _context: &image::AnalysisContext,
                _diagnostics: &mut crate::types::ExtractionDiagnostics,
            ) {
            }
        }

        let mut enriched = ExtractorRegistry::new();
        enriched.register(Box::new(pdf::PdfExtractor::with_image_analyzer(
            std::sync::Arc::new(Named),
        )));
        // The analyzer, and the routing that decides which areas of a page
        // reach it: both determine the bytes, so both are in the recipe.
        let identity = enriched.image_analyzer_identity();
        assert!(identity.contains("named-analyzer-v1"), "{identity}");
        assert!(identity.contains("typeset-routing"), "{identity}");
    }

    /// A search reads renditions; it does not produce them. With an analyzer
    /// configured the two registries have to disagree — the production one
    /// enriches, the search one reads glyphs — and the difference is only
    /// visible while an analyzer is configured, which is why this test
    /// installs one.
    ///
    /// The regression it stands against: exact search built its live PDF read
    /// from `production_registry`, so a search over documents the index could
    /// not serve ran layout detection and recognition once per document, from
    /// inside the loop over the corpus.
    #[test]
    fn exact_search_reads_glyphs_while_production_enriches() {
        struct Enriching;
        impl image::ImageAnalyzer for Enriching {
            fn layout(&self) -> Option<&dyn image::LayoutModel> {
                None
            }
            fn release(&self) {}
            fn reads_embedded_images(&self) -> bool {
                true
            }
            fn identity(&self) -> String {
                "enriching-analyzer-v1".to_string()
            }
            fn analyze(
                &self,
                _images: &mut [crate::types::ExtractedImage],
                _discovered: &[image::DiscoveredImage],
                _context: &image::AnalysisContext,
                _diagnostics: &mut crate::types::ExtractionDiagnostics,
            ) {
            }
        }

        let _serialized = image::CONFIGURED_ANALYZER_TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let restore = image::configured();
        image::configure(Some(std::sync::Arc::new(Enriching)));

        let production = production_registry().image_analyzer_identity().to_string();
        let search = exact_search_registry().image_analyzer_identity().to_string();

        image::configure(restore);

        assert!(
            production.contains("enriching-analyzer-v1"),
            "the production registry must enrich with the configured analyzer: {production}"
        );
        assert_eq!(
            search, "",
            "an exact search must not reach the configured analyzer"
        );
    }

    /// Installing an analyzer changes the recipe, which is what forces
    /// re-extraction and re-embedding; a runtime without one keeps the
    /// identity it already had, so nothing re-extracts for a field that was
    /// merely added.
    #[test]
    fn the_analyzer_is_part_of_the_extraction_recipe() {
        use crate::embed::ExtractionRecipe;

        struct Named(&'static str);
        impl image::ImageAnalyzer for Named {
            fn layout(&self) -> Option<&dyn image::LayoutModel> {
                None
            }
            fn release(&self) {}
            // A test analyzer reads whatever it is handed; the scope is the
            // configuration's decision and these fixtures are not testing it.
            fn reads_embedded_images(&self) -> bool {
                true
            }
            fn identity(&self) -> String {
                self.0.to_string()
            }
            fn analyze(
                &self,
                _images: &mut [crate::types::ExtractedImage],
                _discovered: &[image::DiscoveredImage],
                _context: &image::AnalysisContext,
                _diagnostics: &mut crate::types::ExtractionDiagnostics,
            ) {
            }
        }
        let registry = |analyzer: Option<&'static str>| {
            let mut registry = ExtractorRegistry::new();
            registry.register(match analyzer {
                Some(name) => Box::new(pdf::PdfExtractor::with_image_analyzer(std::sync::Arc::new(
                    Named(name),
                ))) as Box<dyn ContentExtractor>,
                None => Box::new(pdf::PdfExtractor::new()),
            });
            registry
        };
        let recipe = |analyzer| {
            ExtractionRecipe::for_path(Path::new("doc.pdf"), &registry(analyzer), 600, 128).id()
        };

        assert_ne!(recipe(Some("recognizer-v1")), recipe(Some("recognizer-v2")));
        assert_ne!(recipe(Some("recognizer-v1")), recipe(None));
        assert_eq!(recipe(None), {
            let mut plain = ExtractionRecipe::new(600, 128);
            plain.selected_extractor = "pdf-mupdf-v1".to_string();
            plain.id()
        });
    }

    #[test]
    fn test_extractor_registry_default() {
        let registry = ExtractorRegistry::default();
        assert!(registry.extractors.is_empty());
    }
}
