//! What the production table path does to every table crop of one book, crop
//! by crop, with an overlay for each.
//!
//!     cargo run --release --example table_probe -- <pdf> \
//!         [--out DIR] [--threads 4,8] [--cache FILE] [--limit N]
//!
//! Extraction used to route every `table` and `chart` region PP-DocLayoutV2
//! marks out to granite-docling-258M, which reads the crop as a page and
//! answers in DocTags. On one 168-page textbook that was 56 crops, 411.7 s, and
//! 21 admitted tables: the other 35 calls came back as prose that admission
//! threw away against the page's own glyphs.
//!
//! Typeset tables now go to SLANet-plus instead, which answers the *grid*, and
//! the cells are filled from the page's own text layer by geometry rather than
//! transcribed. **A typeset table's structure comes from the structure model
//! and its text from the page; no model transcribes glyphs the page already
//! holds.**
//!
//! Everything this probe measures is the production code:
//! [`table_structure::SlanetPlus`] through `dispatch`'s own loader and layout,
//! [`table_structure::fill_from_page`] for the fill and the Markdown, and
//! [`ocr::place_and_admit`] for the verdict. There is no second decoder here
//! and no second admission rule — a probe with its own copy of either would be
//! measuring a pipeline nobody runs. What is this file's own is the report: the
//! per-crop table, the overlays, and the TSV.
//!
//! Loads its model in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context as _;
use image::{Rgb, RgbImage};
use mupdf::text_page::TextBlockType;
use mupdf::{Document, TextPageFlags};

use wilkes_core::extract::image::dispatch::{self, RecognizerRole};
use wilkes_core::extract::image::doclayout::{self, DocLayout, Recipe};
use wilkes_core::extract::image::ocr::{self, markdown_table_is_rectangular, SpottedRegion};
use wilkes_core::extract::image::table_structure::{self, TableGrid};
use wilkes_core::extract::image::{AnalysisContext, NativeTextOnPage};
use wilkes_core::extract::pdf::typeset::{self, PageSurvey, WordBox};
use wilkes_core::types::{
    BoundingBox, ImageTransform, OcrAdmission, Point, RegionKind, RegionOrigin,
};

// ---------------------------------------------------------------------------
// The page, surveyed
// ---------------------------------------------------------------------------

/// The page's whitespace-delimited words, twice over: the boxes
/// [`typeset::regions`] claims on, and the same words with their characters for
/// the fill.
///
/// Two shapes of one walk because the pipeline wants two: `typeset::regions`
/// takes a [`PageSurvey`] and the fill takes what the host's
/// [`AnalysisContext`] holds. Built here in one pass so the two cannot
/// disagree — which is exactly the disagreement `fill_from_page` raises on.
fn page_words(text_page: &mupdf::TextPage, page: u32) -> (PageSurvey, Vec<NativeTextOnPage>) {
    let mut out = Vec::new();
    let mut drawn = Vec::new();
    let mut native = Vec::new();
    for (block_index, block) in text_page.blocks().enumerate() {
        if block.r#type() == TextBlockType::Image {
            let bounds = block.bounds();
            drawn.push(BoundingBox {
                x: bounds.x0,
                y: bounds.y0,
                width: (bounds.x1 - bounds.x0).max(0.0),
                height: (bounds.y1 - bounds.y0).max(0.0),
            });
            continue;
        }
        for (line_index, line) in block.lines().enumerate() {
            let mut word = 0usize;
            let mut text = String::new();
            let mut bbox: Option<BoundingBox> = None;
            let mut flush =
                |word: &mut usize, text: &mut String, bbox: &mut Option<BoundingBox>| {
                    if text.is_empty() {
                        *bbox = None;
                        return;
                    }
                    if let Some(bbox) = bbox.take() {
                        out.push(WordBox {
                            block: block_index,
                            line: line_index,
                            word: *word,
                            bbox: bbox.clone(),
                        });
                        native.push(NativeTextOnPage {
                            page,
                            text: std::mem::take(text),
                            bbox,
                        });
                    }
                    text.clear();
                    *word += 1;
                };
            for ch in line.chars() {
                let Some(c) = ch.char() else { continue };
                if c.is_whitespace() {
                    flush(&mut word, &mut text, &mut bbox);
                    continue;
                }
                text.push(c);
                let q = ch.quad();
                let x0 = q.ul.x.min(q.ll.x);
                let y0 = q.ul.y.min(q.ur.y);
                let x1 = q.ur.x.max(q.lr.x);
                let y1 = q.ll.y.max(q.lr.y);
                if x1 > x0 && y1 > y0 {
                    let next = BoundingBox {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    };
                    bbox = Some(match &bbox {
                        Some(existing) => existing.merge(&next),
                        None => next,
                    });
                }
            }
            flush(&mut word, &mut text, &mut bbox);
        }
    }
    (PageSurvey { words: out, drawn }, native)
}

// ---------------------------------------------------------------------------
// The crops
// ---------------------------------------------------------------------------

/// One table or chart crop of the book, as extraction itself would produce it.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Crop {
    /// `p15-v0`: the id the production run's report writes, so a row here and
    /// a row there are the same crop.
    id: String,
    page: u32,
    kind: String,
    /// The region rectangle, in page points, before the render pads it.
    bbox: [f32; 4],
}

/// Detect every page and keep the regions that route to a reader.
///
/// Through [`typeset::regions`], not through `decode` alone: a detection whose
/// every word another detection already claimed is dropped there, and the 56
/// crops the production run paid for are the survivors of that stage. The
/// ordinal in the id is the region's index among its page's survivors, which
/// is exactly what `typeset::discover` numbers them by.
fn detect(pdf: &Path, model_dir: &Path, threads: usize) -> anyhow::Result<Vec<Crop>> {
    let detector = DocLayout::load(model_dir, threads).with_context(|| {
        format!(
            "the layout detector is not loadable from {}",
            model_dir.display()
        )
    })?;
    let document = Document::open(pdf)?;
    let pages = document.page_count()?;
    let recipe = Recipe::PRODUCTION;
    let side = DocLayout::input_side();

    let mut out = Vec::new();
    // What the detector *proposed* per class, beside what survived claiming.
    // The production log counts the first and the crop report counts the
    // second, and the difference is the whole answer to "how many chart crops
    // were there" — which is not the same number as "how many charts were
    // detected".
    let mut proposed: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut survived: BTreeMap<&'static str, usize> = BTreeMap::new();
    let started = Instant::now();
    for index in 0..pages {
        let page = document.load_page(index)?;
        let bounds = page.bounds()?;
        let page_box = BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
        };
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        let (survey, _) = page_words(&text_page, (index + 1) as u32);
        let rendered = typeset::render_page(&page, side)?;
        let passes = detector.passes(&rendered, &recipe)?;
        let found = doclayout::decode(&passes, &recipe);
        for region in &found {
            *proposed.entry(region.label).or_default() += 1;
        }
        let claimed = typeset::regions((index + 1) as u32, &page_box, &found, &survey);
        for region in &claimed {
            *survived
                .entry(match region.kind {
                    RegionKind::Text => "text",
                    RegionKind::Formula => "formula",
                    RegionKind::Table => "table",
                    RegionKind::Chart => "chart",
                    RegionKind::Code => "code",
                })
                .or_default() += 1;
        }
        for (ordinal, region) in claimed.iter().enumerate() {
            if !matches!(region.kind, RegionKind::Table | RegionKind::Chart) {
                continue;
            }
            out.push(Crop {
                id: format!("p{}-v{ordinal}", region.page),
                page: region.page,
                kind: format!("{:?}", region.kind),
                bbox: [
                    region.bbox.x,
                    region.bbox.y,
                    region.bbox.width,
                    region.bbox.height,
                ],
            });
        }
        if (index + 1) % 25 == 0 {
            eprintln!(
                "  detected {} of {pages} page(s), {} crop(s), {:.0}s",
                index + 1,
                out.len(),
                started.elapsed().as_secs_f64()
            );
        }
    }
    eprintln!(
        "detection: {pages} page(s) in {:.1}s, {} table/chart crop(s)",
        started.elapsed().as_secs_f64(),
        out.len()
    );
    // Said out loud rather than left to be inferred from a crop count: a class
    // the detector proposed and the claiming stage then dropped never became a
    // crop, and never cost a second of any reader.
    eprintln!("detector proposed, by class:");
    for (label, count) in &proposed {
        eprintln!("  {label:<20} {count}");
    }
    eprintln!("survived typeset::regions, by kind:");
    for (kind, count) in &survived {
        eprintln!("  {kind:<20} {count}");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The overlay
// ---------------------------------------------------------------------------

fn draw_box(canvas: &mut RgbImage, rect: (f32, f32, f32, f32), colour: Rgb<u8>, weight: i64) {
    let (w, h) = (canvas.width() as i64, canvas.height() as i64);
    let (x0, y0, x1, y1) = (
        rect.0.round() as i64,
        rect.1.round() as i64,
        rect.2.round() as i64,
        rect.3.round() as i64,
    );
    let mut put = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && x < w && y < h {
            canvas.put_pixel(x as u32, y as u32, colour);
        }
    };
    for t in 0..weight {
        for x in x0.min(x1)..=x0.max(x1) {
            put(x, y0 + t);
            put(x, y1 - t);
        }
        for y in y0.min(y1)..=y0.max(y1) {
            put(x0 + t, y);
            put(x1 - t, y);
        }
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

fn model_dir() -> PathBuf {
    std::env::var("PROBE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("a home directory");
            PathBuf::from(home).join("Library/Application Support/app.wilkes/models")
        })
}

/// Peak resident set size of this process so far, in bytes.
fn peak_rss() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // Safe: `usage` is a live, correctly sized `rusage` and the call only
    // writes into it.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    // macOS reports bytes here; Linux reports kilobytes. This probe is run on
    // macOS and says so rather than guessing at run time.
    usage.ru_maxrss as u64
}

/// One crop's measurement.
struct Row {
    crop: Crop,
    pixels: (u32, u32),
    /// Milliseconds for the whole call — preprocess, graph, decode, fill — at
    /// each thread count asked for, in the order they were asked for.
    ms: Vec<f64>,
    grid: TableGrid,
    empty: u32,
    unassigned: u32,
    words_in_box: u32,
    first_row_empty: bool,
    rectangular: bool,
    admission: OcrAdmission,
    markdown: String,
}

/// The verdict admission reaches for this crop, through the production rule.
///
/// Built as the region the analyzer builds — one region covering the whole
/// crop, kind `Table`, carrying the fill summary — and passed through
/// [`ocr::place_and_admit`] itself. There is no second copy of the rule here:
/// a threshold the probe re-implemented would be a threshold that could differ
/// from the one that decides the reading.
fn verdict(
    markdown: &str,
    summary: table_structure::TableFillSummary,
    grid: &TableGrid,
    covered: &BoundingBox,
    pixels: (u32, u32),
    page: u32,
) -> OcrAdmission {
    let region = SpottedRegion {
        kind: RegionKind::Table,
        text: markdown.to_string(),
        confidence: grid.score,
        quad: [
            Point { x: 0.0, y: 0.0 },
            Point { x: 1.0, y: 0.0 },
            Point { x: 1.0, y: 1.0 },
            Point { x: 0.0, y: 1.0 },
        ],
        truncated: grid.truncated,
        structure: Some(summary),
    };
    let transform = ImageTransform {
        a: covered.width,
        b: 0.0,
        c: 0.0,
        d: covered.height,
        e: covered.x,
        f: covered.y,
    };
    ocr::place_and_admit(
        vec![region],
        &transform,
        covered,
        pixels.0,
        pixels.1,
        page,
        RegionOrigin::Typeset,
        // Empty: the native-glyph duplicate check does not apply to a typeset
        // region, which *is* the page's glyphs rendered. `place_and_admit`
        // says so itself.
        &AnalysisContext::default(),
        table_structure::ADMISSION_THRESHOLD,
    )[0]
    .admission
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    const USAGE: &str = "usage: table_probe <pdf> [--out DIR] [--threads 4,8] [--cache FILE] \
                         [--limit N]";
    let mut args = std::env::args().skip(1);
    let pdf = PathBuf::from(args.next().context(USAGE)?);
    let mut out_dir = PathBuf::from("tables");
    let mut threads: Vec<usize> = vec![dispatch::recognizer_layout(RecognizerRole::Table, "cpu").1];
    let mut cache: Option<PathBuf> = None;
    let mut limit = usize::MAX;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} wants a value\n{USAGE}"))?;
        match flag.as_str() {
            "--out" => out_dir = PathBuf::from(value),
            "--threads" => {
                threads = value
                    .split(',')
                    .map(|n| n.trim().parse::<usize>().context("--threads wants numbers"))
                    .collect::<anyhow::Result<_>>()?
            }
            "--cache" => cache = Some(PathBuf::from(value)),
            "--limit" => limit = value.parse().context("--limit wants a number")?,
            other => anyhow::bail!("unknown flag {other}\n{USAGE}"),
        }
    }
    anyhow::ensure!(!threads.is_empty(), "--threads named no thread count");

    std::fs::create_dir_all(&out_dir)?;
    let dir = model_dir();

    // The crops, from the cache if one was written and from the detector
    // otherwise. The detector is deterministic, so a cache is a saved 160
    // seconds and not a different fixture.
    let crops: Vec<Crop> = match cache.as_ref().filter(|path| path.is_file()) {
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
            let crops: Vec<Crop> = serde_json::from_str(&text)
                .with_context(|| format!("{} is not a crop cache", path.display()))?;
            eprintln!("crops: {} from {}", crops.len(), path.display());
            crops
        }
        None => {
            let detect_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            let crops = detect(&pdf, &dir, detect_threads)?;
            if let Some(path) = cache.as_ref() {
                std::fs::write(path, serde_json::to_string_pretty(&crops)?)?;
            }
            crops
        }
    };

    println!(
        "══ SLANet-plus on {} table/chart crop(s) of {} ══",
        crops.len(),
        pdf.display()
    );
    let document = Document::open(pdf.as_path())?;
    // Through `dispatch`, so the graph, the thread count and the decode are
    // production's. `--threads` overrides only to time the graph at another
    // width; the first entry is what production actually runs.
    let mut readers: Vec<(usize, Box<dyn table_structure::TableStructure>)> = Vec::new();
    for count in &threads {
        readers.push((
            *count,
            Box::new(table_structure::SlanetPlus::load(&dir, *count)?),
        ));
    }
    println!("model: {}", table_structure::identity());
    println!(
        "admission: rectangular, then no empty first row, no unassigned word, and at most \
         {}/{} of the grid empty\n",
        ocr::TABLE_MAX_EMPTY_CELL_NUMERATOR,
        ocr::TABLE_MAX_EMPTY_CELL_DENOMINATOR,
    );

    let mut measured: Vec<Row> = Vec::new();
    for crop in crops.iter().take(limit) {
        let page = document.load_page(crop.page as i32 - 1)?;
        let bbox = BoundingBox {
            x: crop.bbox[0],
            y: crop.bbox[1],
            width: crop.bbox[2],
            height: crop.bbox[3],
        };
        let (decoded, covered) = typeset::render(&page, &bbox)
            .with_context(|| format!("{}: the crop did not render", crop.id))?;
        let pixels = decoded.pixels;
        let text_page =
            page.to_text_page(TextPageFlags::ACCURATE_BBOXES | TextPageFlags::PRESERVE_IMAGES)?;
        let (_, words) = page_words(&text_page, crop.page);

        let mut ms = Vec::with_capacity(readers.len());
        let mut kept: Option<(TableGrid, table_structure::FilledTable)> = None;
        for (_, reader) in readers.iter_mut() {
            let started = Instant::now();
            let grid = reader
                .read_batch(std::slice::from_ref(&pixels))?
                .pop()
                .expect("one crop in, one grid out");
            let filled = table_structure::fill_from_page(&grid, &covered, &words)?;
            ms.push(started.elapsed().as_secs_f64() * 1000.0);
            kept = Some((grid, filled));
        }
        let (grid, filled) = kept.expect("at least one thread count was asked for");

        let rectangular = markdown_table_is_rectangular(&filled.markdown);
        let admission = verdict(
            &filled.markdown,
            filled.summary,
            &grid,
            &covered,
            (pixels.width(), pixels.height()),
            crop.page,
        );

        // The overlay: the crop, with every cell the model proposed drawn on
        // it. Spanning cells in a second colour, so a merged-cell error is
        // visible rather than inferred from the numbers.
        let mut canvas = pixels.clone();
        let (pw, ph) = (pixels.width() as f32, pixels.height() as f32);
        for cell in &grid.cells {
            let colour = if cell.spans() {
                Rgb([0, 110, 220])
            } else {
                Rgb([220, 30, 30])
            };
            draw_box(
                &mut canvas,
                (cell.x0 * pw, cell.y0 * ph, cell.x1 * pw, cell.y1 * ph),
                colour,
                2,
            );
        }
        canvas.save(out_dir.join(format!("{}.png", crop.id)))?;

        measured.push(Row {
            crop: crop.clone(),
            pixels: (pixels.width(), pixels.height()),
            ms,
            empty: filled.summary.empty_cells,
            unassigned: filled.summary.unassigned_words,
            words_in_box: filled.summary.words_in_box,
            first_row_empty: filled.summary.first_row_empty,
            rectangular,
            admission,
            markdown: filled.markdown,
            grid,
        });
    }

    // ── The table ────────────────────────────────────────────────────────────
    let head: Vec<String> = threads.iter().map(|n| format!("ms@{n}")).collect();
    println!(
        "{:<10} {:>5} {:>11} {:>8} {:>8} {:>7} {:>6} {:>6} {:>6} {:>6} {:>6} {:>5} {:>5}  {}",
        "id",
        "page",
        "px",
        head.first().map(String::as_str).unwrap_or(""),
        head.get(1).map(String::as_str).unwrap_or(""),
        "rowsxcol",
        "cells",
        "span",
        "empty",
        "unasg",
        "words",
        "rect",
        "score",
        "verdict",
    );
    for row in &measured {
        println!(
            "{:<10} {:>5} {:>11} {:>8.1} {:>8.1} {:>7} {:>6} {:>6} {:>6} {:>6} {:>6} {:>5} {:>5.2}  {:?}{}",
            row.crop.id,
            row.crop.page,
            format!("{}x{}", row.pixels.0, row.pixels.1),
            row.ms.first().copied().unwrap_or(f64::NAN),
            row.ms.get(1).copied().unwrap_or(f64::NAN),
            format!("{}x{}", row.grid.rows, row.grid.cols),
            row.grid.cells.len(),
            row.grid.cells.iter().filter(|c| c.spans()).count(),
            row.empty,
            row.unassigned,
            row.words_in_box,
            if row.rectangular { "yes" } else { "NO" },
            row.grid.score,
            row.admission,
            if row.grid.truncated {
                "  TRUNCATED"
            } else {
                ""
            },
        );
    }

    let total: Vec<f64> = (0..threads.len())
        .map(|index| measured.iter().map(|row| row.ms[index]).sum::<f64>() / 1000.0)
        .collect();
    println!();
    for (count, seconds) in threads.iter().zip(&total) {
        println!(
            "{} crop(s) at {count} intra-op thread(s): {seconds:.2}s total, {:.1} ms each",
            measured.len(),
            seconds * 1000.0 / measured.len().max(1) as f64
        );
    }
    println!(
        "rectangular: {} of {}",
        measured.iter().filter(|row| row.rectangular).count(),
        measured.len()
    );
    println!(
        "truncated decodes: {}",
        measured.iter().filter(|row| row.grid.truncated).count()
    );
    // The whole point of the run: what admission did with each of them.
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    for row in &measured {
        *verdicts.entry(format!("{:?}", row.admission)).or_default() += 1;
    }
    println!("admission:");
    for (verdict, count) in &verdicts {
        println!("  {verdict:<28} {count}");
    }
    println!("peak RSS: {:.0} MB", peak_rss() as f64 / 1e6);

    // ── The readings, for eyes rather than for a total ───────────────────────
    let mut dump = String::new();
    for row in &measured {
        dump.push_str(&format!(
            "== {} page {} {}x{} px · {}x{} · {} cell(s), {} spanning · {} unassigned word(s) \
             of {} · rectangular {} · {:?}\n",
            row.crop.id,
            row.crop.page,
            row.pixels.0,
            row.pixels.1,
            row.grid.rows,
            row.grid.cols,
            row.grid.cells.len(),
            row.grid.cells.iter().filter(|c| c.spans()).count(),
            row.unassigned,
            row.words_in_box,
            row.rectangular,
            row.admission,
        ));
        dump.push_str(&row.markdown);
        dump.push('\n');
    }
    let path = out_dir.join("slanet.md");
    std::fs::write(&path, dump)?;

    let mut tsv = String::from(
        "id\tpage\tkind\tpx_w\tpx_h\tms_a\tms_b\trows\tcols\tcells\tspan\tempty\tunassigned\t\
         words\tfirst_row_empty\trect\tscore\ttruncated\tadmission\n",
    );
    for row in &measured {
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{:?}\n",
            row.crop.id,
            row.crop.page,
            row.crop.kind,
            row.pixels.0,
            row.pixels.1,
            row.ms.first().copied().unwrap_or(f64::NAN),
            row.ms.get(1).copied().unwrap_or(f64::NAN),
            row.grid.rows,
            row.grid.cols,
            row.grid.cells.len(),
            row.grid.cells.iter().filter(|c| c.spans()).count(),
            row.empty,
            row.unassigned,
            row.words_in_box,
            row.first_row_empty,
            row.rectangular,
            row.grid.score,
            row.grid.truncated,
            row.admission,
        ));
    }
    std::fs::write(out_dir.join("slanet.tsv"), tsv)?;
    println!(
        "wrote {} and slanet.tsv beside the overlays",
        path.display()
    );
    Ok(())
}
