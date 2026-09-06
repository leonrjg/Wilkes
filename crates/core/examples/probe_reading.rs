//! Ask what produced a given run of a document's reading.
//!
//!     cargo run --release --example probe_reading -- <pdf> <needle> [page]
//!
//! Reports every occurrence of the needle, the page and provenance of the
//! bytes, and — when a page is named — every typeset region on that page.
//! No recognizer is loaded: the analyzer here only records what it was asked
//! to read, so the routing is exercised and nothing is transcribed.
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use wilkes_core::extract::image::{AnalysisContext, DiscoveredImage, ImageAnalyzer};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ContentExtractor;
use wilkes_core::types::{
    ExtractedImage, ExtractionDiagnostics, RegionOrigin, SourceOrigin, TextProvenance,
};

struct Recorder {
    routed: Mutex<Vec<(String, u32, u32, u32)>>,
    /// The real detector, loaded from the installation's model directory.
    detector: Option<Box<wilkes_core::extract::image::doclayout::DocLayout>>,
    /// When set, every typeset region comes back with one admitted formula.
    /// Not a transcription — a stand-in, so the question "would this line have
    /// been replaced by a recognizer's answer" can be asked without running
    /// one. What it proves is *routing*, which is what is in doubt.
    supersede: bool,
}

impl ImageAnalyzer for Recorder {
    fn layout(&self) -> Option<&dyn wilkes_core::extract::image::LayoutModel> {
        self.detector
            .as_ref()
            .map(|d| d.as_ref() as &dyn wilkes_core::extract::image::LayoutModel)
    }
    fn identity(&self) -> String {
        "probe-reading-v1".to_string()
    }
    fn reads_embedded_images(&self) -> bool {
        false
    }

    /// Every kind, as before this configuration could switch a reader
    /// off: this double routes what it is given.
    fn reads_typeset_kind(&self, _: wilkes_core::types::RegionKind) -> bool {
        true
    }
    fn analyze(
        &self,
        _images: &mut [ExtractedImage],
        discovered: &[DiscoveredImage],
        _context: &AnalysisContext,
        _diagnostics: &mut ExtractionDiagnostics,
    ) {
        let mut routed = self.routed.lock().expect("the record's lock");
        for (image, found) in _images.iter_mut().zip(discovered) {
            if found.origin != RegionOrigin::Typeset {
                continue;
            }
            let (w, h) = found
                .decoded
                .as_ref()
                .map_or((0, 0), |d| (d.pixels.width(), d.pixels.height()));
            routed.push((found.id.clone(), found.page, w, h));
            if self.supersede {
                image.ocr_regions = vec![wilkes_core::types::ImageOcrRegion {
                    kind: wilkes_core::types::RegionKind::Formula,
                    text: format!("ROUTED<{}>", found.id),
                    confidence: 0.99,
                    polygon_within_image: vec![wilkes_core::types::Point { x: 0.0, y: 0.0 }],
                    page_polygon: vec![wilkes_core::types::Point {
                        x: image.bbox.x,
                        y: image.bbox.y,
                    }],
                    admission: wilkes_core::types::OcrAdmission::Accepted,
                }];
            }
        }
    }
    fn release(&self) {}
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("usage: <pdf> <needle> [page]"))?,
    );
    let needle = args.next().ok_or_else(|| anyhow::anyhow!("no needle"))?;
    let page: Option<u32> = args.next().and_then(|p| p.parse().ok());

    let supersede = std::env::var_os("PROBE_SUPERSEDE").is_some();
    let model_dir = std::env::var_os("PROBE_MODEL_DIR").map(PathBuf::from);
    let detector = match &model_dir {
        Some(dir) => Some(Box::new(
            wilkes_core::extract::image::doclayout::DocLayout::load(dir, 4)?,
        )),
        None => None,
    };
    let recorder = Arc::new(Recorder {
        routed: Mutex::new(Vec::new()),
        supersede,
        detector,
    });
    if supersede {
        println!("(every typeset region stands in for its lines)");
    }
    let content = PdfExtractor::with_image_analyzer(recorder.clone()).extract(&path)?;

    let routed = recorder.routed.lock().expect("the record's lock").clone();
    println!(
        "{} typeset region(s) routed in the whole document",
        routed.len()
    );
    if let Some(page) = page {
        let here: Vec<_> = routed.iter().filter(|r| r.1 == page).collect();
        println!("on page {page}: {} region(s)", here.len());
        for (id, _, w, h) in here {
            println!("   {id}  {w}x{h} px");
        }
    }

    println!("\n── occurrences of {needle:?} ──");
    let mut at = 0usize;
    let mut hits = 0usize;
    while let Some(found) = content.text[at..].find(&needle) {
        let start = at + found;
        at = start + needle.len();
        hits += 1;
        if hits > 12 {
            continue;
        }
        let segment = content
            .source_map
            .segments
            .iter()
            .find(|s| s.text_range.start <= start && start < s.text_range.end);
        let (page, provenance) = match segment {
            Some(segment) => {
                let page = match &segment.origin {
                    SourceOrigin::PdfPage { page, .. } => Some(*page),
                    _ => None,
                };
                (page, format!("{:?}", segment.provenance))
            }
            None => (None, "NO SEGMENT".to_string()),
        };
        let lo = content.text[..start]
            .char_indices()
            .rev()
            .nth(70)
            .map_or(0, |(i, _)| i);
        let hi = content.text[at..]
            .char_indices()
            .nth(70)
            .map_or(content.text.len(), |(i, _)| at + i);
        let context = content.text[lo..hi].replace('\n', "⏎");
        println!(
            "\n#{hits} at byte {start}, page {}, provenance {}",
            page.map_or("?".to_string(), |p| p.to_string()),
            if provenance.starts_with("ImageOcr") {
                provenance
            } else {
                provenance
            }
        );
        println!("   …{context}…");
    }
    println!("\n{hits} occurrence(s) in total");

    // And the balance for the whole document, so one hit is not read as the
    // whole picture.
    let mut native = 0usize;
    let mut ocr = 0usize;
    for segment in &content.source_map.segments {
        let len = segment.text_range.end - segment.text_range.start;
        match segment.provenance {
            TextProvenance::ImageOcr { .. } => ocr += len,
            _ => native += len,
        }
    }
    println!("reading: {native} bytes native, {ocr} bytes from a recognizer");
    Ok(())
}
