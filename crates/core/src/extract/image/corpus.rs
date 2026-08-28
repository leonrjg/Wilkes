//! The bounded corpus FIGURE.md's step 9 calls for, and the PDF builder it
//! needs.
//!
//! What is checked here is everything Wilkes owns: discovery, the technical
//! limits, deduplication against native glyphs, the admission rule, the
//! diagnostics, the serialization, the cache, and the search and chunking
//! paths that consume the result. The cases are built rather than collected,
//! so each one states in its own source what it is a case *of*.
//!
//! What is not checked here is the recognizer's accuracy — character error on
//! clean, low-resolution, dark-background and rotated-label figures. That is a
//! property of the weights, it needs the weights, and it is
//! [`super::paddleocr_vl::evaluate`]'s job. A test double stands in for the
//! model here precisely so that the contract around it is tested without
//! pretending to have measured it.

#![cfg(test)]

use std::sync::Arc;

use crate::extract::pdf::PdfExtractor;
use crate::extract::ContentExtractor;
use crate::types::{
    ExtractedContent, ImageAnalysisStatus, ImageOcrRegion, OcrAdmission, Point, TextProvenance,
};

use super::describe::FigureDescriber;
use super::ocr::{OcrEngine, SpottedRegion};
use super::NativeImageAnalyzer;

// ── A PDF with pictures in it ────────────────────────────────────────────────

/// One image drawn on a page: its pixels, and the rectangle it occupies in PDF
/// user space (origin bottom-left, as a content stream sees it).
pub(super) struct ImageSpec {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub x: f32,
    pub y: f32,
    pub draw_width: f32,
    pub draw_height: f32,
}

impl ImageSpec {
    /// A gradient, which is a picture with no text in it.
    pub(super) fn gradient(width: u32, height: u32) -> Self {
        let rgb = (0..width * height)
            .flat_map(|index| {
                let value = (index % 251) as u8;
                [value, 255 - value, value / 2]
            })
            .collect();
        Self {
            width,
            height,
            rgb,
            x: 20.0,
            y: 100.0,
            draw_width: 160.0,
            draw_height: 80.0,
        }
    }

    /// The same picture every time: a logo repeats byte for byte, which is
    /// what makes it one cache entry across a library.
    pub(super) fn logo() -> Self {
        Self {
            width: 8,
            height: 8,
            rgb: (0..8 * 8).flat_map(|index| [index as u8, 0, 128]).collect(),
            x: 20.0,
            y: 260.0,
            draw_width: 24.0,
            draw_height: 24.0,
        }
    }

    pub(super) fn at(mut self, x: f32, y: f32, draw_width: f32, draw_height: f32) -> Self {
        self.x = x;
        self.y = y;
        self.draw_width = draw_width;
        self.draw_height = draw_height;
        self
    }
}

/// One page: lines of text at user-space positions, and images.
#[derive(Default)]
pub(super) struct PageSpec {
    pub text: Vec<(f32, f32, String)>,
    pub images: Vec<ImageSpec>,
}

impl PageSpec {
    pub(super) fn with_text(mut self, x: f32, y: f32, text: &str) -> Self {
        self.text.push((x, y, text.to_string()));
        self
    }

    pub(super) fn with_image(mut self, image: ImageSpec) -> Self {
        self.images.push(image);
        self
    }
}

/// Assemble a PDF. Uncompressed image streams and a base-14 font, because the
/// point is to control exactly which blocks MuPDF will report.
pub(super) fn build_pdf(pages: Vec<PageSpec>) -> Vec<u8> {
    const PAGE_WIDTH: f32 = 200.0;
    const PAGE_HEIGHT: f32 = 300.0;

    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1: catalog, 2: page tree, 3: font. Page and content objects follow.
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(Vec::new()); // page tree, patched below
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());

    let mut page_ids = Vec::new();
    for page in &pages {
        let mut content = Vec::new();
        for (x, y, text) in &page.text {
            let escaped = text.replace('\\', r"\\").replace('(', r"\(").replace(')', r"\)");
            content.extend_from_slice(
                format!("BT /F1 12 Tf {x} {y} Td ({escaped}) Tj ET\n").as_bytes(),
            );
        }
        let mut xobjects = String::new();
        for (index, image) in page.images.iter().enumerate() {
            objects.push(
                [
                    format!(
                        "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
                         /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",
                        image.width,
                        image.height,
                        image.rgb.len()
                    )
                    .into_bytes(),
                    image.rgb.clone(),
                    b"\nendstream".to_vec(),
                ]
                .concat(),
            );
            let id = objects.len();
            xobjects.push_str(&format!("/Im{index} {id} 0 R "));
            content.extend_from_slice(
                format!(
                    "q {} 0 0 {} {} {} cm /Im{index} Do Q\n",
                    image.draw_width, image.draw_height, image.x, image.y
                )
                .as_bytes(),
            );
        }

        objects.push(
            [
                format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                content,
                b"\nendstream".to_vec(),
            ]
            .concat(),
        );
        let content_id = objects.len();

        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_WIDTH} {PAGE_HEIGHT}] \
                 /Contents {content_id} 0 R /Resources << /Font << /F1 3 0 R >> \
                 /XObject << {xobjects}>> >> >>"
            )
            .into_bytes(),
        );
        page_ids.push(objects.len());
    }

    let kids: String = page_ids
        .iter()
        .map(|id| format!("{id} 0 R "))
        .collect::<String>();
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.trim_end(),
        page_ids.len()
    )
    .into_bytes();

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(object);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

// ── A recognizer that is not a model ─────────────────────────────────────────

/// What the double should do for one image, in the order images are found.
#[derive(Clone)]
pub(super) enum Script {
    /// Regions as the model would emit them, in fractions of the image.
    Spots(Vec<(&'static str, f32, [f32; 8])>),
    /// A recognition failure. The reading keeps its native text and the result
    /// says it is partial.
    Fails(&'static str),
}

pub(super) struct ScriptedOcr {
    script: Vec<Script>,
    seen: std::sync::Mutex<usize>,
    threshold: f32,
}

impl ScriptedOcr {
    pub(super) fn new(script: Vec<Script>) -> Self {
        Self {
            script,
            seen: std::sync::Mutex::new(0),
            threshold: 0.6,
        }
    }
}

impl OcrEngine for ScriptedOcr {
    fn identity(&self) -> String {
        "scripted-recognizer-v1".to_string()
    }

    fn admission_threshold(&self) -> f32 {
        self.threshold
    }

    fn spot(&self, _image: &image::RgbImage) -> anyhow::Result<Vec<SpottedRegion>> {
        let mut seen = self.seen.lock().expect("the script's lock");
        let index = *seen;
        *seen += 1;
        match self.script.get(index) {
            None => Ok(Vec::new()),
            Some(Script::Fails(reason)) => anyhow::bail!("{reason}"),
            Some(Script::Spots(spots)) => Ok(spots
                .iter()
                .map(|(text, confidence, quad)| SpottedRegion {
                    text: (*text).to_string(),
                    confidence: *confidence,
                    quad: [
                        Point { x: quad[0], y: quad[1] },
                        Point { x: quad[2], y: quad[3] },
                        Point { x: quad[4], y: quad[5] },
                        Point { x: quad[6], y: quad[7] },
                    ],
                })
                .collect()),
        }
    }
}

/// A describer that always fails, for the failure case.
struct FailingDescriber;

impl FigureDescriber for FailingDescriber {
    fn identity(&self) -> String {
        "failing-describer-v1".to_string()
    }
    fn describe(
        &self,
        _image: &image::RgbImage,
        _ocr: &[ImageOcrRegion],
    ) -> anyhow::Result<crate::types::ImageDescription> {
        anyhow::bail!("the describer is unreachable")
    }
}

// ── Running one case ─────────────────────────────────────────────────────────

/// Build one describer, or none. A factory rather than a value because each
/// of the two readings below needs its own.
type Describer = fn() -> Box<dyn FigureDescriber>;

fn extract(
    pages: Vec<PageSpec>,
    script: Vec<Script>,
    describer: Option<Describer>,
) -> (ExtractedContent, crate::types::ExtractionDiagnostics) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("case.pdf");
    std::fs::write(&path, build_pdf(pages)).expect("the fixture is written");

    // Two readings, each with its own double: the script is consumed in the
    // order images are met, so a reused engine would answer the second
    // reading with whatever the first left over.
    let content = PdfExtractor::with_image_analyzer(Arc::new(NativeImageAnalyzer::new(
        Box::new(ScriptedOcr::new(script.clone())),
        describer.map(|build| build()),
    )))
    .extract(&path)
    .expect("the fixture extracts");
    let diagnostics = PdfExtractor::with_image_analyzer(Arc::new(NativeImageAnalyzer::new(
        Box::new(ScriptedOcr::new(script)),
        describer.map(|build| build()),
    )))
    .outline(&path)
    .expect("the fixture's outline reads")
    .diagnostics;
    (content, diagnostics)
}

/// A quadrilateral covering the middle of an image.
const MIDDLE: [f32; 8] = [0.25, 0.25, 0.75, 0.25, 0.75, 0.75, 0.25, 0.75];

// ── The corpus ───────────────────────────────────────────────────────────────

/// A diagram whose labels the recognizer reads: they reach the reading, in the
/// order the model emitted them, at the image's position.
#[test]
fn a_diagram_with_labels_is_transcribed_into_the_reading() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))
            .with_text(20.0, 60.0, "Figure 3: Components of an Expert System")],
        vec![Script::Spots(vec![
            ("Non-expert", 0.95, MIDDLE),
            ("User interface", 0.93, MIDDLE),
            ("Inference engine", 0.91, MIDDLE),
            ("Knowledge base", 0.90, MIDDLE),
            ("Expert knowledge", 0.88, MIDDLE),
        ])],
        None,
    );

    assert!(content.text.contains(
        "Image embedded text: Non-expert; User interface; Inference engine; \
         Knowledge base; Expert knowledge."
    ));
    assert_eq!(diagnostics.native_images_found, 1);
    assert_eq!(diagnostics.native_images_analyzed, 1);
    assert_eq!(diagnostics.ocr_regions_accepted, 5);
    assert_eq!(diagnostics.images_description_not_configured, 1);
}

/// A picture with no text in it contributes nothing to the reading. That it
/// was looked at, and found to hold nothing, is in the diagnostics — which is
/// the difference between this and an image nobody analyzed.
#[test]
fn an_image_with_no_text_leaves_the_reading_alone() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Prose either side")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Spots(Vec::new())],
        None,
    );

    assert!(!content.text.contains("Image embedded text:"));
    assert!(content.images[0].reading_range.is_none());
    assert_eq!(diagnostics.native_images_analyzed, 1);
    assert_eq!(diagnostics.ocr_regions_accepted, 0);
    assert_eq!(content.images[0].status, ImageAnalysisStatus::Complete);
}

/// A repeated logo goes through analysis like anything else — no semantic
/// suppression was added — and is recorded each time it is met.
#[test]
fn a_repeated_logo_is_analyzed_every_time_and_recorded_every_time() {
    let page = || PageSpec::default().with_image(ImageSpec::logo());
    let (content, diagnostics) = extract(
        vec![page(), page(), page()],
        vec![
            Script::Spots(Vec::new()),
            Script::Spots(Vec::new()),
            Script::Spots(Vec::new()),
        ],
        None,
    );

    assert_eq!(diagnostics.native_images_found, 3);
    assert_eq!(diagnostics.native_images_analyzed, 3);
    assert!(!content.text.contains("Image embedded text:"));
    // The same pixels every time, so one annotation addresses all three.
    let digests: std::collections::HashSet<&str> = content
        .images
        .iter()
        .map(|image| image.image_sha256.as_str())
        .collect();
    assert_eq!(digests.len(), 1, "a repeated logo is one set of pixels");
}

/// A region the recognizer was unsure of stays out of the reading, and stays
/// visible on the image and in the counts. A label missing from the text is
/// answerable.
#[test]
fn a_low_confidence_region_is_rejected_and_still_recorded() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default().with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Spots(vec![
            ("Legible", 0.9, MIDDLE),
            ("smudged", 0.2, MIDDLE),
        ])],
        None,
    );

    assert!(content.text.contains("Image embedded text: Legible."));
    assert!(!content.text.contains("smudged"));
    assert_eq!(diagnostics.ocr_regions_accepted, 1);
    assert_eq!(diagnostics.ocr_regions_rejected_low_confidence, 1);
    let rejected = content.images[0]
        .ocr_regions
        .iter()
        .find(|region| region.text == "smudged")
        .expect("the rejected region is kept on the image");
    assert_eq!(rejected.admission, OcrAdmission::RejectedLowConfidence);
}

/// Some PDFs set a diagram's labels as real glyphs over the picture. Those
/// glyphs are the document's own; a transcription of them is not a second
/// occurrence of the text, and does not become one in the reading.
#[test]
fn native_glyphs_drawn_over_an_image_are_not_transcribed_twice() {
    // The image occupies user space x 20..180, y 100..180, which is page-space
    // y 120..200; the label is drawn inside it.
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_image(ImageSpec::gradient(64, 32).at(20.0, 100.0, 160.0, 80.0))
            .with_text(40.0, 140.0, "Knowledge base")],
        vec![Script::Spots(vec![
            ("Knowledge base", 0.95, MIDDLE),
            ("Inference engine", 0.95, MIDDLE),
        ])],
        None,
    );

    assert_eq!(
        content.text.matches("Knowledge base").count(),
        1,
        "the label appears once, as the document's own glyphs: {:?}",
        content.text
    );
    assert!(content.text.contains("Image embedded text: Inference engine."));
    assert_eq!(diagnostics.ocr_regions_deduplicated_against_native_text, 1);
    assert_eq!(diagnostics.ocr_regions_accepted, 1);
}

/// A label the page draws elsewhere is not inside this image, so it is not a
/// duplicate of anything and the transcription stands.
#[test]
fn the_same_words_outside_the_image_do_not_deduplicate_it() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_image(ImageSpec::gradient(64, 32).at(20.0, 100.0, 160.0, 60.0))
            .with_text(20.0, 250.0, "Knowledge base is defined below")],
        vec![Script::Spots(vec![("Knowledge base", 0.95, MIDDLE)])],
        None,
    );

    assert_eq!(diagnostics.ocr_regions_deduplicated_against_native_text, 0);
    assert_eq!(diagnostics.ocr_regions_accepted, 1);
    assert!(content.text.contains("Image embedded text: Knowledge base."));
}

/// A recognition failure leaves the document's own text intact and reports a
/// partial result. It is never a second engine's turn, and never a complete
/// analysis that happened to find nothing.
#[test]
fn a_recognition_failure_is_partial_rather_than_empty() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "The prose survives")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Fails("decoder error")],
        None,
    );

    assert!(content.text.contains("The prose survives"));
    assert!(!content.text.contains("Image embedded text:"));
    assert_eq!(diagnostics.images_ocr_failed, 1);
    assert_eq!(diagnostics.images_ocr_succeeded, 0);
    let ImageAnalysisStatus::Partial { failures } = &content.images[0].status else {
        panic!("expected a partial analysis, got {:?}", content.images[0].status);
    };
    assert!(failures[0].contains("decoder error"), "{failures:?}");
}

/// A description that fails leaves the transcription in place and says the
/// analysis was partial. The two stages are separate facts.
#[test]
fn a_description_failure_keeps_the_transcription() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default().with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Spots(vec![("Expert system", 0.9, MIDDLE)])],
        Some(|| Box::new(FailingDescriber)),
    );

    assert!(content.text.contains("Image embedded text: Expert system."));
    assert!(!content.text.contains("Image description:"));
    assert_eq!(diagnostics.images_ocr_succeeded, 1);
    assert_eq!(diagnostics.images_description_failed, 1);
    assert_eq!(diagnostics.images_description_not_configured, 0);
    assert!(matches!(
        content.images[0].status,
        ImageAnalysisStatus::Partial { .. }
    ));
}

/// A degenerate image is rejected by a technical limit, before any decode, and
/// the rejection is reported with its reason rather than counted as an image
/// with nothing in it.
#[test]
fn a_degenerate_image_is_skipped_by_a_technical_limit() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default().with_image(ImageSpec {
            width: 1,
            height: 1,
            rgb: vec![255, 0, 0],
            x: 20.0,
            y: 100.0,
            draw_width: 40.0,
            draw_height: 40.0,
        })],
        vec![Script::Spots(vec![("never asked", 0.99, MIDDLE)])],
        None,
    );

    assert_eq!(diagnostics.native_images_found, 1);
    assert_eq!(diagnostics.native_images_analyzed, 0);
    assert_eq!(diagnostics.native_images_skipped_technical_limit, 1);
    assert!(!content.text.contains("never asked"));
    let ImageAnalysisStatus::SkippedTechnicalLimit { reason } = &content.images[0].status else {
        panic!("expected a technical limit, got {:?}", content.images[0].status);
    };
    assert!(reason.contains("degenerate"), "{reason}");
}

/// Transcriptions are arbitrary Unicode. Nothing here slices one by a byte
/// offset, and the reading carries what the recognizer read.
#[test]
fn unicode_transcriptions_survive_intact() {
    let (content, _) = extract(
        vec![PageSpec::default().with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Spots(vec![
            ("Größe: 50 %", 0.9, MIDDLE),
            ("知識ベース", 0.9, MIDDLE),
            ("Ω → π", 0.9, MIDDLE),
        ])],
        None,
    );

    assert!(content.text.contains("Größe: 50 %"), "{:?}", content.text);
    assert!(content.text.contains("知識ベース"));
    assert!(content.text.contains("Ω → π"));
}

/// The acceptance criterion, through the machinery exact search actually
/// uses: a transcribed label is findable in the reading, and the position it
/// resolves to is the region's own polygon rather than the whole image.
#[test]
fn exact_search_finds_a_transcribed_label_at_its_own_polygon() {
    use grep_matcher::Matcher;

    let (content, _) = extract(
        vec![PageSpec::default().with_image(
            ImageSpec::gradient(64, 32).at(20.0, 100.0, 160.0, 80.0),
        )],
        vec![Script::Spots(vec![
            ("Expert knowledge", 0.9, [0.1, 0.1, 0.4, 0.1, 0.4, 0.3, 0.1, 0.3]),
            ("Knowledge base", 0.9, [0.6, 0.6, 0.9, 0.6, 0.9, 0.8, 0.6, 0.8]),
        ])],
        None,
    );

    let projection = crate::search::pdf_projection::PdfSearchProjection::new(&content.text);
    let matcher = crate::search::pdf_projection::literal_matcher("Expert knowledge", false)
        .expect("the matcher builds");
    let found = matcher
        .find(projection.as_bytes())
        .expect("the search runs")
        .expect("exact search finds the transcribed label");
    let range = projection
        .raw_range(crate::types::ByteRange {
            start: found.start(),
            end: found.end(),
        })
        .expect("the match maps back into the reading");

    let origin = content
        .source_map
        .resolve_range(range.clone())
        .expect("the match resolves to a page position");
    let crate::types::SourceOrigin::PdfPage { page, bbox: Some(bbox) } = origin else {
        panic!("expected a page locator, got {origin:?}");
    };
    assert_eq!(page, 1);

    let image = &content.images[0];
    let region = image
        .ocr_regions
        .iter()
        .find(|region| region.text == "Expert knowledge")
        .expect("the region is on the image");
    let hull = super::serialize::polygon_hull(&region.page_polygon, &image.bbox);
    assert!(
        (bbox.x - hull.x).abs() < 0.01 && (bbox.width - hull.width).abs() < 0.01,
        "resolved to {bbox:?}, the region's polygon is {hull:?}"
    );
    assert!(
        hull.width < image.bbox.width * 0.9,
        "the region should be smaller than the whole image: {hull:?} in {:?}",
        image.bbox
    );

    // And the bytes say what they are.
    let segment = content
        .source_map
        .segments
        .iter()
        .find(|segment| segment.text_range.start == range.start)
        .expect("the label has its own segment");
    let TextProvenance::ImageOcr { image_id, confidence } = &segment.provenance else {
        panic!("expected transcription provenance, got {:?}", segment.provenance);
    };
    assert_eq!(image_id, &image.id);
    assert!(confidence.is_some(), "a transcribed region carries its signal");
}

/// The acceptance criterion the embedder rests on: the enrichment reaches the
/// passages the existing embedder is given, in one piece, with no second
/// embedding path — and the chunks still rebuild the reading byte for byte.
#[test]
fn the_enrichment_reaches_the_embedder_as_one_passage() {
    struct Fixed;
    impl FigureDescriber for Fixed {
        fn identity(&self) -> String {
            "fixed-describer-v1".to_string()
        }
        fn describe(
            &self,
            _image: &image::RgbImage,
            _ocr: &[ImageOcrRegion],
        ) -> anyhow::Result<crate::types::ImageDescription> {
            Ok(crate::types::ImageDescription {
                description: "Expert knowledge feeds a knowledge base that an inference \
                              engine consults."
                    .to_string(),
            })
        }
    }

    let (content, _) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "An expert system has three components.")
            .with_image(ImageSpec::gradient(64, 32))
            .with_text(20.0, 60.0, "Figure 3: Components of an Expert System")],
        vec![Script::Spots(vec![
            ("Knowledge base", 0.9, MIDDLE),
            ("Expert knowledge", 0.9, MIDDLE),
        ])],
        Some(|| Box::new(Fixed)),
    );

    let chunks = crate::embed::index::chunk::chunk_content(
        &content,
        std::path::PathBuf::from("case.pdf"),
        600,
        128,
    );
    let passage = chunks
        .iter()
        .find(|chunk| chunk.text.contains("Image embedded text:"))
        .expect("the enrichment is in a passage the embedder receives");
    assert!(
        passage.text.contains("Image description: Expert knowledge feeds"),
        "transcription and description reach the embedder together: {:?}",
        passage.text
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.text.contains("Image embedded text:"))
            .count(),
        1,
        "the block is one passage"
    );

    crate::embed::index::chunk::ensure_chunks_reconstruct(
        &content.text,
        chunks
            .iter()
            .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
    )
    .expect("the chunks rebuild the reading");
}

/// Several figures in one document each land at their own picture, and the
/// counts add up over the whole document.
#[test]
fn a_document_of_figures_counts_and_places_every_one() {
    let (content, diagnostics) = extract(
        vec![
            PageSpec::default()
                .with_text(20.0, 250.0, "First page prose")
                .with_image(ImageSpec::gradient(64, 32)),
            PageSpec::default()
                .with_image(ImageSpec::logo())
                .with_text(20.0, 200.0, "Second page prose")
                .with_image(ImageSpec::gradient(48, 24).at(20.0, 60.0, 120.0, 60.0)),
        ],
        vec![
            Script::Spots(vec![("Alpha", 0.9, MIDDLE)]),
            Script::Spots(Vec::new()),
            Script::Spots(vec![("Beta", 0.9, MIDDLE)]),
        ],
        None,
    );

    assert_eq!(diagnostics.native_images_found, 3);
    assert_eq!(diagnostics.native_images_analyzed, 3);
    assert_eq!(diagnostics.ocr_regions_accepted, 2);

    let alpha = content.text.find("Alpha").expect("the first figure");
    let second_page = content.text.find("Second page prose").expect("page two");
    let beta = content.text.find("Beta").expect("the second figure");
    assert!(alpha < second_page && second_page < beta, "{:?}", content.text);

    assert_eq!(content.images[0].page, 1);
    assert_eq!(content.images[1].page, 2);
    assert_eq!(content.images[2].page, 2);
    assert!(content.images[1].reading_range.is_none(), "the logo says nothing");
}

/// Analysis is versioned extraction, so it happens once. The second reading of
/// the same document asks the cache, not the model.
#[test]
fn a_second_reading_takes_its_annotation_from_the_cache() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("cached.pdf");
    std::fs::write(
        &path,
        build_pdf(vec![PageSpec::default().with_image(ImageSpec::gradient(64, 32))]),
    )
    .expect("the fixture is written");

    let analyzer = |script| {
        Arc::new(
            NativeImageAnalyzer::new(Box::new(ScriptedOcr::new(script)), None).with_cache(
                super::cache::AnnotationCache::open(dir.path()).expect("the cache opens"),
            ),
        )
    };

    let first = PdfExtractor::with_image_analyzer(analyzer(vec![Script::Spots(vec![(
        "Knowledge base",
        0.9,
        MIDDLE,
    )])]));
    assert!(first
        .extract(&path)
        .expect("extracts")
        .text
        .contains("Image embedded text: Knowledge base."));

    // A recognizer that would fail if it were asked. It is not asked.
    let second =
        PdfExtractor::with_image_analyzer(analyzer(vec![Script::Fails("must not be called")]));
    let content = second.extract(&path).expect("extracts");
    assert!(
        content.text.contains("Image embedded text: Knowledge base."),
        "the cached annotation was not used: {:?}",
        content.text
    );
    assert_eq!(content.images[0].status, ImageAnalysisStatus::Complete);
}
