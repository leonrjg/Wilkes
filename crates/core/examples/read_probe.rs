//! The whole reading path with the real models: detector, formula reader, page
//! reader, admission, supersession, and the areas a reading surface is handed.
//!
//! `probe_reading` answers "was this routed" with a stub. This answers "what
//! does the document say afterwards", which is the only question that settles
//! whether a case is fixed.
//!
//!     PROBE_MODEL_DIR=… cargo run --release --example read_probe -- <pdf> [needle…]
//!
//! The models are loaded in this process, which the application itself is
//! forbidden from doing — see the "no inference in the host process" invariant
//! in `AGENTS.md`. It does not bind here: the whole point of a probe is to put
//! a model in front of a document with nothing in between, this process exists
//! for that and nothing else, and Ctrl-C is the forced kill the invariant asks
//! for. Routing a probe through the worker protocol would mean measuring the
//! protocol.
//!
//! What it does *not* do is invent a configuration. The detector and both
//! recognizers are loaded through `dispatch`'s own loaders and
//! `dispatch::recognizer_layout`, so the readers and thread counts here are
//! the ones the application runs; a probe with its own numbers beside them
//! would be timing a pipeline nobody has.

use std::sync::Arc;

use wilkes_core::extract::image::dispatch::{self, RecognitionEngine, RecognizerRole};
use wilkes_core::extract::image::ocr::{ImageRecognition, OcrEngine};
use wilkes_core::extract::image::serialize::{reading_regions, superseded_areas};
use wilkes_core::extract::image::{
    doclayout, granite_docling, table_structure, texify, NativeImageAnalyzer,
};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ContentExtractor;
use wilkes_core::types::{ImageScope, OcrAdmission};

/// A page reader that reads nothing, for the configuration in which typeset
/// tables and charts are not recognized at all and the page's own words stay.
///
/// Probe-only. It exists so the *cost* of that configuration can be measured
/// on the same pipeline rather than subtracted from another run's total: the
/// detector still runs, the crops are still rendered, and only the page
/// reader's call is free.
struct NoPageReader;

impl OcrEngine for NoPageReader {
    fn identity(&self) -> String {
        "probe-no-page-reader".to_string()
    }

    fn admission_threshold(&self) -> f32 {
        1.0
    }

    fn spot_batch(&self, images: &[image::RgbImage]) -> anyhow::Result<Vec<ImageRecognition>> {
        Ok(images.iter().map(|_| ImageRecognition::default()).collect())
    }
}

/// Which reader the areas that are not formulas go to, for this run.
fn page_reader(dir: &std::path::Path) -> anyhow::Result<(Box<dyn OcrEngine>, &'static str)> {
    match std::env::var("PROBE_PAGE_READER")
        .unwrap_or_else(|_| "granite".to_string())
        .as_str()
    {
        "granite" => Ok((
            dispatch::load_recognizer_local(
                RecognitionEngine::Onnx,
                granite_docling::MODEL_ID,
                dir,
                "cpu",
            )?,
            "granite-docling-258M",
        )),
        "none" => Ok((Box::new(NoPageReader), "none")),
        #[cfg(all(feature = "recognize-vision", target_os = "macos"))]
        "vision" => Ok((
            dispatch::load_recognizer_local(
                RecognitionEngine::Vision,
                wilkes_core::extract::image::vision::MODEL_ID,
                dir,
                "cpu",
            )?,
            "apple-vision",
        )),
        other => anyhow::bail!("PROBE_PAGE_READER={other} is not a reader this build has"),
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wilkes_core=info".into()),
        )
        .with_thread_ids(true)
        .init();

    let mut args = std::env::args().skip(1);
    let pdf = args.next().expect("usage: <pdf> [needle…]");
    let needles: Vec<String> = args.collect();
    let dir = std::path::PathBuf::from(std::env::var("PROBE_MODEL_DIR").unwrap_or_else(|_| {
        format!(
            "{}/Library/Application Support/app.wilkes/models",
            std::env::var("HOME").unwrap_or_default()
        )
    }));

    // Production's own layout, asked for rather than repeated.
    let page_layout = dispatch::recognizer_layout(RecognizerRole::Page, "cpu");
    let formula_layout = dispatch::recognizer_layout(RecognizerRole::Formula, "cpu");
    eprintln!(
        "page reader {}x{} · formula reader {}x{} · detector {} thread(s)",
        page_layout.0,
        page_layout.1,
        formula_layout.0,
        formula_layout.1,
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1)
    );
    let (reader, reader_name) = page_reader(&dir)?;
    eprintln!("page reader for this run: {reader_name}");
    let analyzer = NativeImageAnalyzer::new(
        reader,
        None,
        ImageScope::TypesetOnly,
        Some(dispatch::load_layout_detector_local(
            doclayout::MODEL_ID,
            &dir,
        )?),
    )
    .with_formula_reader(dispatch::load_recognizer_local(
        RecognitionEngine::Onnx,
        texify::MODEL_ID,
        &dir,
        "cpu",
    )?);
    // The table reader, attached the way production attaches it: the same
    // loader, the same layout, and the fill and admission that follow are the
    // application's own. `PROBE_TABLE_READER=none` leaves it off, so the run
    // this replaced — every typeset table through the page reader — can be
    // measured on the same pipeline rather than recalled from another one.
    let table_reader = std::env::var("PROBE_TABLE_READER").unwrap_or_else(|_| "slanet".to_string());
    let analyzer = match table_reader.as_str() {
        "none" => analyzer,
        "slanet" => analyzer.with_table_reader(dispatch::load_table_structure_local(
            table_structure::MODEL_ID,
            &dir,
        )?),
        other => anyhow::bail!("PROBE_TABLE_READER={other} is not a table reader this build has"),
    };
    eprintln!("table reader for this run: {table_reader}");

    let started = std::time::Instant::now();
    let content = PdfExtractor::with_image_analyzer(Arc::new(analyzer))
        .extract(std::path::Path::new(&pdf))?;
    println!(
        "\nread in {:?}, {} bytes",
        started.elapsed(),
        content.text.len()
    );

    let regions = reading_regions(&content);
    let areas = superseded_areas(&content.text, &regions);
    println!(
        "{} reading regions, {} superseded areas",
        regions.len(),
        areas.len()
    );

    // What the recognizer was actually spent on, page by page. A book's cost
    // is not its mean: a handful of derivation pages carry thirty crops each
    // and most pages carry none, so the shape is the answer and an average
    // hides it.
    let pages = content.metadata.page_count.unwrap_or(0) as usize;
    let mut per_page = vec![0usize; pages.max(1)];
    let mut crops = 0usize;
    for image in &content.images {
        if image.origin != wilkes_core::types::RegionOrigin::Typeset {
            continue;
        }
        crops += 1;
        if let Some(slot) = per_page.get_mut(image.page as usize - 1) {
            *slot += 1;
        }
    }
    let mut sorted = per_page.clone();
    sorted.sort_unstable();
    let at =
        |q: f64| sorted[(((sorted.len() as f64 - 1.0) * q).round() as usize).min(sorted.len() - 1)];
    println!(
        "{pages} page(s), {crops} typeset crop(s), {} page(s) with none\n\
         crops a page: min {} · median {} · p90 {} · max {} · mean {:.1}",
        per_page.iter().filter(|n| **n == 0).count(),
        sorted.first().copied().unwrap_or(0),
        at(0.5),
        at(0.9),
        sorted.last().copied().unwrap_or(0),
        crops as f64 / pages.max(1) as f64,
    );
    println!(
        "wall {:.1} min over {pages} page(s) — {:.0} ms a page",
        started.elapsed().as_secs_f64() / 60.0,
        started.elapsed().as_secs_f64() * 1000.0 / pages.max(1) as f64,
    );

    // What became of every region either recognizer produced, by the kind it
    // was read as and the verdict admission reached. The crop's own class is
    // not here — an `ExtractedImage` does not carry it — and is joined from
    // the `typeset crop <id>` line the discovery stage logs, by id.
    let mut by_kind: std::collections::BTreeMap<String, [usize; 9]> = Default::default();
    let slot = |admission: OcrAdmission| match admission {
        OcrAdmission::Accepted => 0,
        OcrAdmission::RejectedLowConfidence => 1,
        OcrAdmission::DeduplicatedAgainstNativeText => 2,
        OcrAdmission::RejectedInvalidLatex => 3,
        OcrAdmission::RejectedMalformedTable => 4,
        OcrAdmission::RejectedTruncated => 5,
        OcrAdmission::RejectedEmptyHeaderRow => 6,
        OcrAdmission::RejectedUnassignedWords => 7,
        OcrAdmission::RejectedSparseTable => 8,
    };
    for image in &content.images {
        for region in &image.ocr_regions {
            by_kind.entry(format!("{:?}", region.kind)).or_default()[slot(region.admission)] += 1;
        }
    }
    println!(
        "\nregions read, by kind → accepted · low confidence · deduplicated · invalid latex \
         · malformed table · truncated · empty header row · unassigned words · sparse"
    );
    for (kind, counts) in &by_kind {
        println!(
            "  {kind:<8} {} · {} · {} · {} · {} · {} · {} · {} · {}",
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4],
            counts[5],
            counts[6],
            counts[7],
            counts[8],
        );
    }

    // One line per crop, so a call's cost can be attributed to the crop that
    // paid it: the id joins to the discovery log, the position in this list to
    // the batch item the recognizer reported.
    if let Ok(out) = std::env::var("PROBE_CROPS") {
        let mut report = String::new();
        for image in &content.images {
            if image.origin != wilkes_core::types::RegionOrigin::Typeset {
                continue;
            }
            report.push_str(&format!(
                "{} page {} {}x{} px bbox {:.1},{:.1} {:.1}x{:.1} pt — {} region(s)\n",
                image.id,
                image.page,
                image.pixel_width,
                image.pixel_height,
                image.bbox.x,
                image.bbox.y,
                image.bbox.width,
                image.bbox.height,
                image.ocr_regions.len(),
            ));
            for region in &image.ocr_regions {
                report.push_str(&format!(
                    "    {:?} {:?} conf {:.3}\n",
                    region.kind, region.admission, region.confidence
                ));
                for line in region.text.lines().take(4) {
                    report.push_str(&format!("      | {line}\n"));
                }
            }
        }
        std::fs::write(&out, report)?;
        println!("wrote the per-crop report to {out}");
    }

    if let Ok(out) = std::env::var("PROBE_TEXT") {
        std::fs::write(&out, &content.text)?;
        std::fs::write(
            format!("{out}.areas.json"),
            serde_json::to_vec_pretty(&areas)?,
        )?;
        println!("wrote the reading to {out}");
    }

    for needle in &needles {
        println!("\n── lines containing {needle:?} ──");
        let mut seen = 0;
        for line in content
            .text
            .lines()
            .filter(|line| line.contains(needle.as_str()))
        {
            println!("  {line}");
            seen += 1;
            if seen >= 12 {
                break;
            }
        }
        if seen == 0 {
            println!("  (none)");
        }
    }
    Ok(())
}
