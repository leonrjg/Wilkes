//! Count what a root would hand a recognizer, under each image scope.
//!
//! Runs the real extraction path — the same typeset survey, the same budget,
//! the same technical limits — with a counting analyzer in place of a
//! recognizer. Nothing is transcribed and no model is loaded, so this is the
//! question "how much work is there" asked without doing the work.
//!
//!     cargo run --release --example count_ocr_targets -- <root>
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wilkes_core::extract::image::{AnalysisContext, DiscoveredImage, ImageAnalyzer, NativeImage};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ContentExtractor;
use wilkes_core::types::{ExtractedImage, ExtractionDiagnostics, RegionOrigin};

#[derive(Default, Clone, Copy)]
struct Tally {
    /// Rasters the PDF embeds that a recognizer would actually be handed:
    /// decoded, so past the degenerate and decode limits.
    embedded_readable: u32,
    /// Rasters a technical limit or a decode failure took out. Never handed to
    /// a recognizer under either scope.
    embedded_rejected: u32,
    /// Areas the page typesets as mathematics or as a ruled table. All of
    /// them: there is no per-document cap.
    typeset: u32,
    pages: u32,
}

struct Counter {
    /// The real detector, when COUNT_MODEL_DIR names an installation. Without
    /// it nothing a page typesets is marked out, which is also what a runtime
    /// with no detector installed would do.
    detector: Option<std::sync::Arc<wilkes_core::extract::image::doclayout::DocLayout>>,
    tally: Mutex<Tally>,
    /// Megapixels per region, by origin. A recognizer's encode cost scales
    /// with the tiles an image is cut into, so this is what says whether
    /// "fewer calls" is also "less work".
    areas: Mutex<(Vec<f64>, Vec<f64>)>,
}

impl ImageAnalyzer for Counter {
    fn layout(&self) -> Option<&dyn wilkes_core::extract::image::LayoutModel> {
        self.detector
            .as_ref()
            .map(|d| d.as_ref() as &dyn wilkes_core::extract::image::LayoutModel)
    }
    fn identity(&self) -> String {
        // A distinct identity, so this never collides with a real reading's
        // annotation cache.
        "count-ocr-targets-v1".to_string()
    }

    // True, so the backend decodes the embedded rasters and the technical
    // limits get their say. That is the "before" number.
    fn reads_embedded_images(&self) -> bool {
        true
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
        let mut tally = self.tally.lock().expect("the tally's lock");
        let mut areas = self.areas.lock().expect("the areas' lock");
        for found in discovered {
            let readable = found.decoded.is_some();
            if let Some(decoded) = &found.decoded {
                let mp = f64::from(decoded.pixels.width()) * f64::from(decoded.pixels.height())
                    / 1_000_000.0;
                match found.origin {
                    RegionOrigin::Embedded => areas.0.push(mp),
                    RegionOrigin::Typeset => areas.1.push(mp),
                }
            }
            match (found.origin, readable) {
                (RegionOrigin::Embedded, true) => tally.embedded_readable += 1,
                (RegionOrigin::Embedded, false) => tally.embedded_rejected += 1,
                (RegionOrigin::Typeset, _) => tally.typeset += 1,
            }
        }
    }

    fn release(&self) {}
}

fn pdfs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: count_ocr_targets <root>"))?;

    let detector = match std::env::var_os("COUNT_MODEL_DIR").map(PathBuf::from) {
        Some(dir) => Some(std::sync::Arc::new(
            wilkes_core::extract::image::doclayout::DocLayout::load(&dir, 4)?,
        )),
        None => None,
    };
    let files = pdfs(&root);
    eprintln!("{} PDF(s) under {}", files.len(), root.display());

    let mut per_file: BTreeMap<String, Tally> = BTreeMap::new();
    let mut total = Tally::default();
    let mut embedded_mp: Vec<f64> = Vec::new();
    let mut typeset_mp: Vec<f64> = Vec::new();
    let mut peak_document = (String::new(), 0.0f64);
    let mut peak_typeset = (String::new(), 0.0f64);

    for path in &files {
        let counter = Arc::new(Counter {
            detector: detector.clone(),
            tally: Mutex::new(Tally::default()),
            areas: Mutex::new((Vec::new(), Vec::new())),
        });
        let extractor = PdfExtractor::with_image_analyzer(counter.clone());
        let started = std::time::Instant::now();
        // `outline` reads the document exactly as `extract` does and hands back
        // the diagnostics, which is where the typeset budget reports itself.
        let outline = match extractor.outline(path) {
            Ok(outline) => outline,
            Err(error) => {
                eprintln!("  {}: FAILED: {error:#}", path.display());
                continue;
            }
        };
        {
            let areas = counter.areas.lock().expect("the areas' lock");
            // Everything discovered is held decoded until analysis finishes,
            // so one document's total is its peak resident artwork.
            let name = || {
                path.strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string()
            };
            let resident: f64 = areas.0.iter().chain(areas.1.iter()).sum();
            if resident > peak_document.1 {
                peak_document = (name(), resident);
            }
            // What the same document holds under `typeset_only`, where the
            // embedded rasters are never decoded.
            let typeset_resident: f64 = areas.1.iter().sum();
            if typeset_resident > peak_typeset.1 {
                peak_typeset = (name(), typeset_resident);
            }
            embedded_mp.extend(areas.0.iter().copied());
            typeset_mp.extend(areas.1.iter().copied());
        }
        let mut tally = *counter.tally.lock().expect("the tally's lock");
        tally.pages = outline.diagnostics.pages;

        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        eprintln!(
            "  {name}: {} pages, {} embedded readable, {} embedded rejected, \
             {} typeset in {:?}",
            tally.pages,
            tally.embedded_readable,
            tally.embedded_rejected,
            tally.typeset,
            started.elapsed()
        );
        total.pages += tally.pages;
        total.embedded_readable += tally.embedded_readable;
        total.embedded_rejected += tally.embedded_rejected;
        total.typeset += tally.typeset;
        per_file.insert(name, tally);
    }

    let before = total.embedded_readable + total.typeset;
    println!("\n── {} ──", root.display());
    println!("files                       {}", per_file.len());
    println!("pages                       {}", total.pages);
    println!();
    println!("typeset_and_embedded        {before}   (what the old default passed)");
    println!("  embedded rasters          {}", total.embedded_readable);
    println!("  typeset regions           {}", total.typeset);
    println!(
        "typeset_only                {}   (what the new default passes)",
        total.typeset
    );
    println!();
    println!("embedded, rejected by limit {}", total.embedded_rejected);
    if before > 0 {
        println!(
            "\nreduction                   {:.1}%  ({} fewer recognizer calls)",
            100.0 * (before - total.typeset) as f64 / before as f64,
            before - total.typeset
        );
    }
    let summarize = |name: &str, mut v: Vec<f64>| {
        if v.is_empty() {
            println!("{name:<12} none");
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN areas"));
        let sum: f64 = v.iter().sum();
        println!(
            "{name:<12} n={:<5} total {:.1} MP   median {:.3} MP   mean {:.3} MP   max {:.2} MP",
            v.len(),
            sum,
            v[v.len() / 2],
            sum / v.len() as f64,
            v[v.len() - 1]
        );
        sum
    };
    println!("\n── pixels handed to the recognizer ──");
    let e = summarize("embedded", embedded_mp);
    let t = summarize("typeset", typeset_mp);
    if e + t > 0.0 {
        println!(
            "\ntypeset is {:.1}% of the pixels and {:.1}% of the calls",
            100.0 * t / (e + t),
            100.0 * total.typeset as f64 / (total.typeset + total.embedded_readable) as f64
        );
    }
    println!("\n── peak resident artwork, one document ──");
    println!(
        "typeset_and_embedded        {:.1} MP ({:.2} GB of RGB8) — {}",
        peak_document.1,
        peak_document.1 * 3.0 / 1024.0,
        peak_document.0
    );
    println!(
        "typeset_only                {:.1} MP ({:.2} GB of RGB8) — {}",
        peak_typeset.1,
        peak_typeset.1 * 3.0 / 1024.0,
        peak_typeset.0
    );
    let _ = std::mem::size_of::<NativeImage>();
    Ok(())
}
