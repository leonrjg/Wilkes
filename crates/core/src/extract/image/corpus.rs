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
use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use super::NativeImageAnalyzer;

// ── A capture of what a real document actually draws ─────────────────────────

/// An [`OcrEngine`] that transcribes nothing and keeps the pixels it was
/// given.
///
/// The way to get a real document's native images out of extraction without
/// adding an accessor that only a test would use: extraction already hands
/// every decoded image to the analyzer, and this is an analyzer.
#[derive(Default)]
pub(super) struct ImageCapture {
    pub images: std::sync::Mutex<Vec<image::RgbImage>>,
}

impl OcrEngine for Arc<ImageCapture> {
    fn identity(&self) -> String {
        "capture".to_string()
    }

    fn admission_threshold(&self) -> f32 {
        1.0
    }

    fn spot_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<ImageRecognition>> {
        self.images
            .lock()
            .expect("capture lock")
            .extend(images.iter().cloned());
        Ok(vec![ImageRecognition::default(); images.len()])
    }
}

// ── The accuracy corpus ──────────────────────────────────────────────────────

/// The conditions FIGURE.md's step 9 lists, as figures a recognizer can be
/// measured on.
///
/// Built, not collected, for the same reason as the contract cases above:
/// each one states in its own source what it is a case *of*, and the ground
/// truth is what was drawn rather than what someone later read off a scan.
/// The figures are real rasterizations — MuPDF draws the page and the pixels
/// are what it drew — so the recognizer meets real glyphs and real
/// anti-aliasing, at a resolution each case chooses.
///
/// This is the corpus for [`super::paddleocr_vl::evaluate`]. It is not run by
/// the suite: it needs 1.9 GB of weights.
///
/// The rendering scales are chosen against what dominates the recognizer's
/// cost, which is the spotting task's pixel envelope and not the figure. Any
/// figure large enough to fill it costs the same, and on a CPU that is tens of
/// minutes each — so the built cases are rendered small enough that a sweep
/// over eight conditions and two checkpoints finishes, and what a real
/// full-size figure costs is measured on the real one instead. These cases
/// exist to vary the *condition*; the sample supplies the cost.
#[allow(dead_code)] // Reached only from the ignored evaluation test.
pub(super) fn accuracy_corpus() -> Vec<super::paddleocr_vl::EvaluationCase> {
    use super::paddleocr_vl::{EvaluationCase, ExpectedRegion};

    /// The labels of the sample figure, at the positions the diagram puts
    /// them: a source on the left, a hub in the middle, a store below it, and
    /// the expert's knowledge entering from the right.
    const DIAGRAM: &[(f32, f32, &str)] = &[
        (12.0, 240.0, "Non-expert"),
        (12.0, 200.0, "User interface"),
        (75.0, 160.0, "Inference engine"),
        (75.0, 110.0, "Knowledge base"),
        (85.0, 60.0, "Expert knowledge"),
    ];

    let diagram_page = || {
        DIAGRAM
            .iter()
            .fold(PageSpec::default(), |page, (x, y, text)| {
                page.with_text(*x, *y, text)
            })
    };

    let case = |name: &str, page: PageSpec, scale: f32, labels: &[(f32, f32, &str)]| {
        let pdf = build_pdf(vec![page]);
        EvaluationCase {
            name: name.to_string(),
            image: render_page(&pdf, 0, scale),
            expected: labels
                .iter()
                .map(|(x, y, text)| ExpectedRegion {
                    text: (*text).to_string(),
                    centre: Some(label_centre(*x, *y, text)),
                })
                .collect(),
        }
    };

    vec![
        // The baseline: what the recognizer does when nothing is against it.
        case("clean-diagram", diagram_page(), CASE_SCALE, DIAGRAM),
        // A figure exported at screen resolution, which is most of them.
        case("low-resolution", diagram_page(), LOW_SCALE, DIAGRAM),
        // Slide-deck artwork is rarely drawn on white.
        case(
            "coloured-background",
            diagram_page().with_background((0.82, 0.89, 0.97)),
            CASE_SCALE,
            DIAGRAM,
        ),
        // Inverted contrast, which a binarizing recognizer fails outright and
        // a VLM should not notice at all.
        case(
            "dark-background",
            DIAGRAM
                .iter()
                .fold(PageSpec::default(), |page, (x, y, text)| {
                    page.with_text(*x, *y, text)
                        .with_text_colour((0.97, 0.97, 0.97))
                })
                .with_background((0.09, 0.09, 0.12)),
            CASE_SCALE,
            DIAGRAM,
        ),
        // Axis captions and side labels are turned; the quads come back
        // turned with them, which is why the geometry is a quadrilateral and
        // not a rectangle.
        EvaluationCase {
            name: "rotated-labels".to_string(),
            image: render_page(
                &build_pdf(vec![DIAGRAM.iter().fold(
                    PageSpec::default(),
                    |page, (x, y, text)| {
                        let (x, y) = (*x + 20.0, *y - 40.0);
                        // Turned a quarter turn, a label runs up the page, so
                        // it is the height it has to fit inside.
                        assert!(
                            y + text.chars().count() as f32 * LABEL_POINTS * AVERAGE_ADVANCE_EM
                                <= PAGE_HEIGHT_POINTS,
                            "rotated {text:?} at y={y} runs off the page"
                        );
                        page.with_rotated_text(x, y, 90.0, text)
                    },
                )]),
                0,
                CASE_SCALE,
            ),
            expected: DIAGRAM
                .iter()
                .map(|(_, _, text)| ExpectedRegion {
                    text: (*text).to_string(),
                    centre: None,
                })
                .collect(),
        },
        // Nothing to transcribe. Every region emitted here is a false one,
        // and this is the case that says so.
        EvaluationCase {
            name: "no-text".to_string(),
            image: render_page(
                &build_pdf(vec![PageSpec::default().with_image(
                    ImageSpec::gradient(64, 64).at(20.0, 100.0, 160.0, 120.0),
                )]),
                0,
                CASE_SCALE,
            ),
            expected: Vec::new(),
        },
        // Characters outside ASCII, which is where a byte-indexing
        // transcription pipeline breaks.
        case(
            "unicode-labels",
            UNICODE_DIAGRAM
                .iter()
                .fold(PageSpec::default(), |page, (x, y, text)| {
                    page.with_text(*x, *y, text)
                }),
            CASE_SCALE,
            UNICODE_DIAGRAM,
        ),
    ]
}

/// 240x360, which the spotting task doubles to 480x720 — a real figure at a
/// resolution a document plausibly carries, and roughly a fifth of what a
/// full-envelope figure costs to recognize.
const CASE_SCALE: f32 = 1.2;

/// The degraded end: 120x180, where 12-point labels are around seven pixels
/// tall.
const LOW_SCALE: f32 = 0.6;

const UNICODE_DIAGRAM: &[(f32, f32, &str)] = &[
    (12.0, 240.0, "Système expert"),
    (12.0, 200.0, "Größe"),
    (75.0, 160.0, "Conocimiento"),
    (75.0, 110.0, "Naïve Bayes"),
    (12.0, 60.0, "Fähigkeit"),
];

/// Half the em width Helvetica averages over mixed-case text, close enough to
/// place a label and to catch one that would not fit on the page.
const AVERAGE_ADVANCE_EM: f32 = 0.5;
const LABEL_POINTS: f32 = 12.0;
const PAGE_WIDTH_POINTS: f32 = 200.0;
const PAGE_HEIGHT_POINTS: f32 = 300.0;

/// Where a drawn label sits, as a fraction of the rendered image.
///
/// Estimated from the layout rather than measured off the raster: Helvetica's
/// advance widths average close to 0.5 em over mixed-case text and its cap
/// height is 0.717 em, which places the centre within a few percent of the
/// image. That is the precision this measurement has, and it is enough for
/// what it is asked — whether a region landed on its own label or on another
/// part of the figure.
fn label_centre(x: f32, y: f32, text: &str) -> Point {
    let width = text.chars().count() as f32 * LABEL_POINTS * AVERAGE_ADVANCE_EM;
    // A label the page clips is ground truth the figure does not contain, and
    // the recognizer would be scored down for reading what is actually there.
    // This cost a whole evaluation run once: `Expert knowledge` was laid out
    // past the right edge and every checkpoint "truncated" it to `Expert kno`.
    assert!(
        x + width <= PAGE_WIDTH_POINTS,
        "{text:?} at x={x} runs off a {PAGE_WIDTH_POINTS}pt page"
    );
    Point {
        x: (x + width / 2.0) / PAGE_WIDTH_POINTS,
        // PDF user space counts up from the bottom; an image counts down from
        // the top.
        y: (PAGE_HEIGHT_POINTS - (y + LABEL_POINTS * 0.717 / 2.0)) / PAGE_HEIGHT_POINTS,
    }
}

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

/// One line of drawn text: where it sits, what it says, how it is turned, and
/// what colour it is drawn in.
pub(super) struct TextSpec {
    pub x: f32,
    pub y: f32,
    pub text: String,
    /// Anticlockwise, in degrees, about the line's own origin.
    pub rotation: f32,
    pub rgb: (f32, f32, f32),
}

/// One page: lines of text at user-space positions, and images.
#[derive(Default)]
pub(super) struct PageSpec {
    pub text: Vec<TextSpec>,
    pub images: Vec<ImageSpec>,
    /// Painted over the whole page before anything else. `None` leaves the
    /// page as the viewer's own white.
    pub background: Option<(f32, f32, f32)>,
}

impl PageSpec {
    pub(super) fn with_text(mut self, x: f32, y: f32, text: &str) -> Self {
        self.text.push(TextSpec {
            x,
            y,
            text: text.to_string(),
            rotation: 0.0,
            rgb: (0.0, 0.0, 0.0),
        });
        self
    }

    /// A label turned on the page, as a diagram turns its axis captions.
    pub(super) fn with_rotated_text(mut self, x: f32, y: f32, degrees: f32, text: &str) -> Self {
        self.text.push(TextSpec {
            x,
            y,
            text: text.to_string(),
            rotation: degrees,
            rgb: (0.0, 0.0, 0.0),
        });
        self
    }

    pub(super) fn with_text_colour(mut self, rgb: (f32, f32, f32)) -> Self {
        if let Some(last) = self.text.last_mut() {
            last.rgb = rgb;
        }
        self
    }

    pub(super) fn with_background(mut self, rgb: (f32, f32, f32)) -> Self {
        self.background = Some(rgb);
        self
    }

    pub(super) fn with_image(mut self, image: ImageSpec) -> Self {
        self.images.push(image);
        self
    }
}

/// Rasterize one page of a built PDF, as a scanner or a figure export would.
///
/// This is how the accuracy corpus gets typeset text without a font
/// rasterizer of its own: MuPDF already draws pages, and drawing one is the
/// most faithful way to produce the thing a recognizer meets — real glyphs,
/// real anti-aliasing, at a resolution the case chooses.
pub(super) fn render_page(pdf: &[u8], page: usize, scale: f32) -> image::RgbImage {
    let document = mupdf::Document::from_bytes(pdf, "pdf").expect("the built PDF opens");
    let page = document.load_page(page as i32).expect("the page loads");
    let pixmap = page
        .to_pixmap(
            &mupdf::Matrix::new_scale(scale, scale),
            &mupdf::Colorspace::device_rgb(),
            false,
            true,
        )
        .expect("the page rasterizes");
    let (width, height) = (pixmap.width(), pixmap.height());
    image::RgbImage::from_raw(width, height, pixmap.samples().to_vec())
        .expect("a device-RGB pixmap is three bytes a pixel")
}

/// Assemble a PDF. Uncompressed image streams and a base-14 font, because the
/// point is to control exactly which blocks MuPDF will report.
pub(super) fn build_pdf(pages: Vec<PageSpec>) -> Vec<u8> {
    const PAGE_WIDTH: f32 = PAGE_WIDTH_POINTS;
    const PAGE_HEIGHT: f32 = PAGE_HEIGHT_POINTS;

    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1: catalog, 2: page tree, 3: font. Page and content objects follow.
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(Vec::new()); // page tree, patched below
                              // WinAnsi, so a label's accented characters are the bytes written for them
                              // rather than whatever StandardEncoding happens to put at that code.
    objects.push(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );

    let mut page_ids = Vec::new();
    for page in &pages {
        let mut content = Vec::new();
        if let Some((red, green, blue)) = page.background {
            content.extend_from_slice(
                format!("{red} {green} {blue} rg 0 0 {PAGE_WIDTH} {PAGE_HEIGHT} re f\n").as_bytes(),
            );
        }
        for line in &page.text {
            // WinAnsi is the base-14 default, so a label may carry any Latin-1
            // character as its own byte; the escapes are the three the string
            // syntax reserves.
            let escaped = line
                .text
                .replace('\\', r"\\")
                .replace('(', r"\(")
                .replace(')', r"\)");
            let bytes: Vec<u8> = escaped
                .chars()
                .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
                .collect();
            let (radians, (red, green, blue)) = (line.rotation.to_radians(), line.rgb);
            let (cos, sin) = (radians.cos(), radians.sin());
            content.extend_from_slice(
                format!(
                    "BT /F1 12 Tf {red} {green} {blue} rg {cos} {sin} {} {cos} {} {} Tm (",
                    -sin, line.x, line.y
                )
                .as_bytes(),
            );
            content.extend_from_slice(&bytes);
            content.extend_from_slice(b") Tj ET\n");
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
    /// Prose regions as the model would emit them, in fractions of the image.
    Spots(Vec<(&'static str, f32, [f32; 8])>),
    /// Regions of any kind — what a recognizer that parses a document
    /// returns, rather than one that only transcribes.
    Reads(Vec<(RegionKind, &'static str, f32, [f32; 8])>),
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

    /// One script entry per image, and a scripted failure fails the whole
    /// batch — which is what a real recognizer does, since the batch is one
    /// request to one process.
    fn spot_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<ImageRecognition>> {
        let mut all = Vec::with_capacity(images.len());
        for _ in images {
            all.push(ImageRecognition::from_regions(self.spot_one()?));
        }
        Ok(all)
    }
}

impl ScriptedOcr {
    fn spot_one(&self) -> anyhow::Result<Vec<SpottedRegion>> {
        let mut seen = self.seen.lock().expect("the script's lock");
        let index = *seen;
        *seen += 1;
        match self.script.get(index) {
            None => Ok(Vec::new()),
            Some(Script::Fails(reason)) => anyhow::bail!("{reason}"),
            Some(Script::Reads(reads)) => Ok(reads
                .iter()
                .map(|(kind, text, confidence, quad)| SpottedRegion {
                    kind: *kind,
                    text: (*text).to_string(),
                    confidence: *confidence,
                    quad: [
                        Point {
                            x: quad[0],
                            y: quad[1],
                        },
                        Point {
                            x: quad[2],
                            y: quad[3],
                        },
                        Point {
                            x: quad[4],
                            y: quad[5],
                        },
                        Point {
                            x: quad[6],
                            y: quad[7],
                        },
                    ],
                })
                .collect()),
            Some(Script::Spots(spots)) => Ok(spots
                .iter()
                .map(|(text, confidence, quad)| SpottedRegion {
                    kind: RegionKind::Text,
                    text: (*text).to_string(),
                    confidence: *confidence,
                    quad: [
                        Point {
                            x: quad[0],
                            y: quad[1],
                        },
                        Point {
                            x: quad[2],
                            y: quad[3],
                        },
                        Point {
                            x: quad[4],
                            y: quad[5],
                        },
                        Point {
                            x: quad[6],
                            y: quad[7],
                        },
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
    // Almost every case below is about what a recognizer does with a picture,
    // which requires the pictures to reach it. The scope is its own case and
    // says so by naming itself.
    extract_scoped(
        pages,
        script,
        describer,
        crate::types::ImageScope::TypesetAndEmbedded,
    )
}

fn extract_scoped(
    pages: Vec<PageSpec>,
    script: Vec<Script>,
    describer: Option<Describer>,
    scope: crate::types::ImageScope,
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
        scope,
        // These fixtures are about what a recognizer does with a picture, not
        // about what marks pictures out.
        None,
    )))
    .extract(&path)
    .expect("the fixture extracts");
    let diagnostics = PdfExtractor::with_image_analyzer(Arc::new(NativeImageAnalyzer::new(
        Box::new(ScriptedOcr::new(script)),
        describer.map(|build| build()),
        scope,
        // These fixtures are about what a recognizer does with a picture, not
        // about what marks pictures out.
        None,
    )))
    .outline(&path)
    .expect("the fixture's outline reads")
    .diagnostics;
    (content, diagnostics)
}

/// A quadrilateral covering the middle of an image.
const MIDDLE: [f32; 8] = [0.25, 0.25, 0.75, 0.25, 0.75, 0.75, 0.25, 0.75];

// ── The corpus ───────────────────────────────────────────────────────────────

/// The default scope does not spend the recognizer on a picture the PDF
/// embeds. The page still draws it, the reading still knows it is there, and
/// nothing is claimed about what it contains.
///
/// The script fails on the first call, so a recognizer that ran at all would
/// leave `Partial { failures }` behind and this would not pass by accident.
#[test]
fn the_default_scope_withholds_an_embedded_raster_from_the_recognizer() {
    let (content, diagnostics) = extract_scoped(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Fails("the recognizer must not be called")],
        None,
        crate::types::ImageScope::TypesetOnly,
    );

    assert_eq!(content.images.len(), 1, "the page still draws one picture");
    let image = &content.images[0];
    let ImageAnalysisStatus::NotRead { reason } = &image.status else {
        panic!("expected NotRead, got {:?}", image.status);
    };
    assert!(
        reason.contains("typeset"),
        "the reason names the setting rather than a fault: {reason}"
    );

    // Never decoded, so there is no digest and no size. An empty digest here
    // is the truthful record of a picture nobody looked at, not a failure.
    assert!(image.image_sha256.is_empty(), "the pixels were not read");
    assert_eq!((image.pixel_width, image.pixel_height), (0, 0));
    assert!(image.ocr_regions.is_empty());
    assert!(image.reading_range.is_none());

    assert!(
        !content.text.contains("Image embedded text:"),
        "nothing was transcribed into the reading: {:?}",
        content.text
    );
    assert!(
        content.text.contains("Before the figure"),
        "and the page's own text is untouched: {:?}",
        content.text
    );

    // Found and counted, separately from what a limit rejected: a reader of
    // the diagnostics can tell a setting from a decoder problem.
    assert_eq!(diagnostics.native_images_found, 1);
    assert_eq!(diagnostics.native_images_not_read, 1);
    assert_eq!(diagnostics.native_images_analyzed, 0);
    assert_eq!(diagnostics.native_images_skipped_technical_limit, 0);
    assert_eq!(diagnostics.images_ocr_succeeded, 0);
    assert_eq!(diagnostics.images_ocr_failed, 0);
}

/// The same page under the other scope, so the test above is about the scope
/// and not about the fixture.
#[test]
fn the_wider_scope_reads_the_same_embedded_raster() {
    let (content, diagnostics) = extract_scoped(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Reads(vec![(
            RegionKind::Text,
            "Knowledge base",
            0.95,
            MIDDLE,
        )])],
        None,
        crate::types::ImageScope::TypesetAndEmbedded,
    );

    assert_eq!(content.images[0].status, ImageAnalysisStatus::Complete);
    assert!(
        content
            .text
            .contains("Image embedded text: Knowledge base."),
        "{:?}",
        content.text
    );
    assert_eq!(diagnostics.native_images_analyzed, 1);
    assert_eq!(diagnostics.native_images_not_read, 0);
}

/// A figure holding a formula, a table and a chart beside its labels: each
/// kind reaches the reading under its own label, so a reader and a language
/// model can tell a transcription from a reconstruction without consulting
/// metadata.
#[test]
fn every_recognized_kind_reaches_the_reading_under_its_own_label() {
    let table = "| Corpus | Recall |\n| --- | --- |\n| Reports | 0.91 |";
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Reads(vec![
            (RegionKind::Text, "Knowledge base", 0.95, MIDDLE),
            (
                RegionKind::Formula,
                "S(q,d) = \\Sigma_{i} w_{i}",
                0.90,
                MIDDLE,
            ),
            (RegionKind::Table, table, 0.88, MIDDLE),
            (RegionKind::Chart, table, 0.86, MIDDLE),
        ])],
        None,
    );

    assert!(content
        .text
        .contains("Image embedded text: Knowledge base."));
    assert!(content
        .text
        .contains("Image embedded formula: S(q,d) = \\Sigma_{i} w_{i}."));
    assert!(content
        .text
        .contains("Image embedded table:\n| Corpus | Recall |"));
    assert!(content
        .text
        .contains("Image transcribed chart:\n| Corpus | Recall |"));

    assert_eq!(diagnostics.regions_routed_text, 1);
    assert_eq!(diagnostics.regions_routed_formula, 1);
    assert_eq!(diagnostics.regions_routed_table, 1);
    assert_eq!(diagnostics.regions_routed_chart, 1);
    assert_eq!(diagnostics.formulas_accepted, 1);
    assert_eq!(diagnostics.tables_accepted, 1);
    assert_eq!(diagnostics.charts_accepted, 1);
}

/// A formula the decoder truncated is confident and invalid. It is refused,
/// the refusal is counted with its own reason, and the string never reaches
/// the reading.
#[test]
fn a_formula_that_does_not_parse_is_refused_with_its_reason() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Reads(vec![(
            RegionKind::Formula,
            "S(q,d) = \\frac{a}{b",
            0.99,
            MIDDLE,
        )])],
        None,
    );

    assert!(!content.text.contains("\\frac{a}{b"));
    assert!(!content.text.contains("Image embedded formula:"));
    assert_eq!(diagnostics.formulas_rejected_invalid_latex, 1);
    assert_eq!(diagnostics.formulas_accepted, 0);
    // Kept on the image, where a missing formula is answerable.
    let region = &content.images[0].ocr_regions[0];
    assert_eq!(
        region.admission,
        crate::types::OcrAdmission::RejectedInvalidLatex
    );
}

/// A ragged table is a failed recognition wearing the shape of a result. It
/// is refused on structure rather than on confidence, and a chart refused the
/// same way is counted as a chart.
#[test]
fn a_table_that_is_not_rectangular_is_refused_on_structure() {
    let (content, diagnostics) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Reads(vec![
            (
                RegionKind::Table,
                "| a | b |\n| --- | --- |\n| 1 |",
                0.99,
                MIDDLE,
            ),
            (RegionKind::Chart, "rising, roughly", 0.99, MIDDLE),
        ])],
        None,
    );

    assert!(!content.text.contains("Image embedded table:"));
    assert!(!content.text.contains("Image transcribed chart:"));
    assert_eq!(diagnostics.tables_rejected_malformed, 1);
    assert_eq!(diagnostics.charts_rejected_malformed, 1);
    assert_eq!(diagnostics.tables_accepted, 0);
    assert_eq!(diagnostics.charts_accepted, 0);
}

/// Every byte a recognized table contributes says it is a table, so a
/// consumer resolving provenance is told what it has without reading the
/// label out of the text.
#[test]
fn a_tables_bytes_carry_its_kind_in_their_provenance() {
    let table = "| Corpus | Recall |\n| --- | --- |\n| Reports | 0.91 |";
    let (content, _) = extract(
        vec![PageSpec::default()
            .with_text(20.0, 250.0, "Before the figure")
            .with_image(ImageSpec::gradient(64, 32))],
        vec![Script::Reads(vec![(RegionKind::Table, table, 0.9, MIDDLE)])],
        None,
    );

    let block = content.images[0]
        .reading_range
        .clone()
        .expect("the table reached the reading");
    let covering: Vec<&crate::types::SourceSegment> = content
        .source_map
        .segments
        .iter()
        .filter(|segment| {
            segment.text_range.start >= block.start && segment.text_range.end <= block.end
        })
        .collect();
    assert!(!covering.is_empty());
    assert!(covering.iter().all(|segment| matches!(
        segment.provenance,
        TextProvenance::ImageOcr {
            kind: RegionKind::Table,
            ..
        }
    )));
}

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
    assert!(content
        .text
        .contains("Image embedded text: Inference engine."));
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
    assert!(content
        .text
        .contains("Image embedded text: Knowledge base."));
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
        panic!(
            "expected a partial analysis, got {:?}",
            content.images[0].status
        );
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
        panic!(
            "expected a technical limit, got {:?}",
            content.images[0].status
        );
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
        vec![PageSpec::default()
            .with_image(ImageSpec::gradient(64, 32).at(20.0, 100.0, 160.0, 80.0))],
        vec![Script::Spots(vec![
            (
                "Expert knowledge",
                0.9,
                [0.1, 0.1, 0.4, 0.1, 0.4, 0.3, 0.1, 0.3],
            ),
            (
                "Knowledge base",
                0.9,
                [0.6, 0.6, 0.9, 0.6, 0.9, 0.8, 0.6, 0.8],
            ),
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
    let crate::types::SourceOrigin::PdfPage {
        page,
        bbox: Some(bbox),
    } = origin
    else {
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
    let TextProvenance::ImageOcr {
        image_id,
        confidence,
        ..
    } = &segment.provenance
    else {
        panic!(
            "expected transcription provenance, got {:?}",
            segment.provenance
        );
    };
    assert_eq!(image_id, &image.id);
    assert!(
        confidence.is_some(),
        "a transcribed region carries its signal"
    );
}

/// A label the figure sets on two lines is transcribed as two regions, and the
/// reading separates them with `; ` — so an exact search for the label as a
/// person reads it does not find it.
///
/// **This is a limitation, not a design.** It is pinned here because the
/// evaluation found it on the real sample document: PaddleOCR-VL's spotting
/// task emits one region per drawn line, every label in that diagram is set on
/// two lines inside its own shape, and the transcription — which is
/// character-perfect — reaches the reading as `User; interface`. A semicolon
/// is punctuation the figure does not contain, and FIGURE.md's acceptance
/// criterion "exact search for `Expert knowledge` finds the sample image"
/// therefore fails on the very figure it names.
///
/// Joining the two would take a geometric rule over the whole region set
/// rather than over neighbours: the recognizer reads this figure in row order,
/// so `User` and `interface` are not adjacent in emission — `Inference` and
/// `Knowledge` come between them. That is layout analysis, which this phase
/// does not do, so the behaviour is recorded rather than quietly patched.
#[test]
fn a_label_split_across_two_drawn_lines_is_two_regions_in_the_reading() {
    use grep_matcher::Matcher;

    let (content, _) = extract(
        vec![PageSpec::default().with_image(ImageSpec::gradient(64, 32))],
        // Stacked, as the sample's circles set their labels, and in the row
        // order the recognizer actually reads such a figure in.
        vec![Script::Spots(vec![
            ("User", 0.95, [0.1, 0.1, 0.4, 0.1, 0.4, 0.3, 0.1, 0.3]),
            ("Inference", 0.95, [0.6, 0.1, 0.9, 0.1, 0.9, 0.3, 0.6, 0.3]),
            ("interface", 0.95, [0.1, 0.4, 0.4, 0.4, 0.4, 0.6, 0.1, 0.6]),
            ("engine", 0.95, [0.6, 0.4, 0.9, 0.4, 0.9, 0.6, 0.6, 0.6]),
        ])],
        None,
    );

    assert!(
        content
            .text
            .contains("Image embedded text: User; Inference; interface; engine."),
        "each drawn line is its own region: {:?}",
        content.text
    );

    let projection = crate::search::pdf_projection::PdfSearchProjection::new(&content.text);
    let finds = |needle: &str| {
        crate::search::pdf_projection::literal_matcher(needle, false)
            .expect("the matcher builds")
            .find(projection.as_bytes())
            .expect("the search runs")
            .is_some()
    };
    assert!(finds("User"), "the line itself is findable");
    assert!(
        !finds("User interface"),
        "the label as a person reads it is not findable, and this is the gap"
    );
}

/// The acceptance criterion `get_document_text` rests on: a page-scoped read
/// of the document includes the enrichment, exactly once, and only on the page
/// that drew the picture.
///
/// A page-scoped read is the span between the first and last byte the source
/// map attributes to that page (`wilkes_agent::reader`), so what decides the
/// answer is where the enrichment's own segments say they are. That is what is
/// checked here: the tool itself is one registry lookup and this slice, and
/// the registry is checked in `extract::tests`.
#[test]
fn a_page_scoped_read_carries_that_pages_enrichment_and_no_others() {
    let (content, _) = extract(
        vec![
            PageSpec::default()
                .with_text(20.0, 250.0, "First page prose")
                .with_image(ImageSpec::gradient(64, 32)),
            PageSpec::default().with_text(20.0, 250.0, "Second page prose"),
        ],
        vec![
            Script::Spots(vec![("Inference engine", 0.95, MIDDLE)]),
            Script::Spots(Vec::new()),
        ],
        None,
    );

    let span = |page: u32| -> String {
        let ranges: Vec<_> = content
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                matches!(segment.origin, crate::types::SourceOrigin::PdfPage { page: p, .. } if p == page)
            })
            .map(|segment| segment.text_range.clone())
            .collect();
        assert!(!ranges.is_empty(), "page {page} has no segments at all");
        let start = ranges
            .iter()
            .map(|range| range.start)
            .min()
            .expect("a start");
        let end = ranges.iter().map(|range| range.end).max().expect("an end");
        content.text[start..end].to_string()
    };

    let first = span(1);
    assert_eq!(
        first
            .matches("Image embedded text: Inference engine")
            .count(),
        1,
        "the enrichment is in the reading of its own page, once: {first:?}"
    );
    assert!(first.contains("First page prose"), "{first:?}");
    assert!(
        !span(2).contains("Image embedded text:"),
        "a page that drew no picture reads none: {:?}",
        span(2)
    );
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
        passage
            .text
            .contains("Image description: Expert knowledge feeds"),
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

/// FIGURE.md's semantic acceptance criterion: *semantic search for "Where does
/// expert knowledge enter the system?" retrieves the image-enrichment passage
/// when a description is configured.*
///
/// Retrieval for real — the chunks are written to a `SemanticIndex` and the
/// question is asked of it — with a transparent embedder in place of a model:
/// a term-overlap vector, so a passage's rank is a fact about its words rather
/// than about weights this test would otherwise be silently measuring.
///
/// What that proves is the half Wilkes owns: the enrichment is chunked,
/// embedded through the one existing path, stored, and comes back first for a
/// question only the picture answers. What it does not prove is that a
/// particular real model ranks it first; no test can, and the shipped
/// embedder's quality is not this feature's claim.
#[test]
fn semantic_search_retrieves_the_enrichment_for_a_question_only_the_picture_answers() {
    use crate::embed::index::db::{PreparedFile, SemanticIndex};

    /// One dimension per term of interest. Cosine similarity then ranks by
    /// how much of the question a passage actually contains.
    const TERMS: &[&str] = &[
        "expert",
        "knowledge",
        "enter",
        "system",
        "inference",
        "engine",
        "base",
        "component",
        "figure",
        "chapter",
        "reasoning",
    ];
    fn embed(text: &str) -> Vec<f32> {
        let lowered = text.to_lowercase();
        let mut vector: Vec<f32> = TERMS
            .iter()
            .map(|term| lowered.matches(term).count() as f32)
            .collect();
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut vector {
                *value /= norm;
            }
        }
        vector.resize(TERMS.len(), 0.0);
        vector
    }

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
                description: "Expert knowledge enters from the right and fills the \
                              knowledge base, which the inference engine consults."
                    .to_string(),
            })
        }
    }

    let (content, _) = extract(
        vec![PageSpec::default()
            .with_text(
                20.0,
                260.0,
                "This chapter introduces reasoning under uncertainty.",
            )
            .with_image(ImageSpec::gradient(64, 32))
            .with_text(20.0, 60.0, "Figure 3: Components of an Expert System")],
        vec![Script::Spots(vec![
            ("Inference engine", 0.95, MIDDLE),
            ("Knowledge base", 0.95, MIDDLE),
            ("Expert knowledge", 0.95, MIDDLE),
        ])],
        Some(|| Box::new(Fixed)),
    );

    let dir = tempfile::tempdir().expect("a temporary index");
    let path = dir.path().join("case.pdf");
    std::fs::write(&path, b"%PDF-1.4\n").expect("a file to hang the chunks on");
    let mut index = SemanticIndex::create(
        dir.path(),
        "term-overlap",
        TERMS.len(),
        crate::types::EmbeddingEngine::SBERT,
        Some(dir.path()),
    )
    .expect("the index is created");

    let chunks = crate::embed::index::chunk::chunk_content(&content, path.clone(), 220, 0);
    assert!(chunks.len() > 1, "the document is more than one passage");
    index
        .write_file(PreparedFile {
            retained: Default::default(),
            full_text: content.text.clone(),
            path: path.clone(),
            chunks: chunks
                .iter()
                .map(|chunk| (chunk.clone(), embed(&chunk.text)))
                .collect(),
        })
        .expect("the passages are indexed");

    let found = index
        .query(&embed("Where does expert knowledge enter the system?"), 3)
        .expect("the index answers");
    let best = found.first().expect("the question retrieves something");
    assert!(
        best.chunk_text
            .contains("Image description: Expert knowledge enters"),
        "the enrichment should answer a question only the picture answers, got {:?}",
        found
            .iter()
            .map(|chunk| &chunk.chunk_text)
            .collect::<Vec<_>>()
    );
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
    assert!(
        alpha < second_page && second_page < beta,
        "{:?}",
        content.text
    );

    assert_eq!(content.images[0].page, 1);
    assert_eq!(content.images[1].page, 2);
    assert_eq!(content.images[2].page, 2);
    assert!(
        content.images[1].reading_range.is_none(),
        "the logo says nothing"
    );
}

/// Analysis is versioned extraction, so it happens once. The second reading of
/// the same document asks the cache, not the model.
#[test]
fn a_second_reading_takes_its_annotation_from_the_cache() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("cached.pdf");
    std::fs::write(
        &path,
        build_pdf(vec![
            PageSpec::default().with_image(ImageSpec::gradient(64, 32))
        ]),
    )
    .expect("the fixture is written");

    let analyzer = |script| {
        Arc::new(
            NativeImageAnalyzer::new(
                Box::new(ScriptedOcr::new(script)),
                None,
                crate::types::ImageScope::TypesetAndEmbedded,
                None,
            )
            .with_cache(super::cache::AnnotationCache::open(dir.path()).expect("the cache opens")),
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
        content
            .text
            .contains("Image embedded text: Knowledge base."),
        "the cached annotation was not used: {:?}",
        content.text
    );
    assert_eq!(content.images[0].status, ImageAnalysisStatus::Complete);
}
