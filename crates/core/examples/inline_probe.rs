//! Can the recognizer read an *inline* formula the detector finds?
//!
//! The display-formula path is settled: a detected area becomes a crop, the
//! recognizer answers `Formula`, and the answer replaces the lines it covers.
//! Inline mathematics is the open case — `(2k^2 + 2k)` sharing its line with
//! prose — and before any of the plumbing is worth writing there are two
//! questions that only pixels can answer:
//!
//! 1. Does the recognizer, handed a crop of an inline expression, come back
//!    with `Formula` and parseable LaTeX — or with `Text`, which admission
//!    refuses for a typeset region by design?
//! 2. Is the page's own glyph run for that expression actually wrong? An
//!    inline `n` extracts perfectly; only the ones the text layer flattens are
//!    worth a recognizer call.
//!
//!     PROBE_MODEL_DIR=… cargo run --release --example inline_probe -- <pdf> <page…>
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use std::path::PathBuf;

use anyhow::Context as _;
use mupdf::{Colorspace, Device, Document, IRect, Matrix, TextPageFlags};

use wilkes_core::extract::image::granite_docling::GraniteDocling;
use wilkes_core::extract::image::ocr::{latex_parses, OcrEngine};
use wilkes_core::extract::image::{decode, doclayout, LayoutModel};
use wilkes_core::types::{BoundingBox, RegionKind};

/// The same numbers `typeset.rs` renders production crops at. Duplicated
/// rather than imported because that module is private to the PDF backend —
/// and a probe that rendered differently from production would be measuring a
/// picture the recognizer will never see.
const MARGIN_POINTS: f32 = 8.0;
const TARGET_LONGEST_PX: f32 = 1600.0;
const MIN_RENDER_SCALE: f32 = 2.0;
const MAX_RENDER_SCALE: f32 = 8.0;
const MAX_ASPECT: f32 = 4.0;

fn model_dir() -> PathBuf {
    std::env::var("PROBE_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("a home directory");
            PathBuf::from(home).join("Library/Application Support/app.wilkes/models")
        })
}

fn pad_to_aspect(region: IRect) -> IRect {
    let (width, height) = (region.x1 - region.x0, region.y1 - region.y0);
    let (padded_width, padded_height) = if width as f32 > height as f32 * MAX_ASPECT {
        (
            width,
            ((width as f32 / MAX_ASPECT).floor() as i32).max(height),
        )
    } else if height as f32 > width as f32 * MAX_ASPECT {
        (
            ((height as f32 / MAX_ASPECT).floor() as i32).max(width),
            height,
        )
    } else {
        (width, height)
    };
    let x0 = region.x0 - (padded_width - width) / 2;
    let y0 = region.y0 - (padded_height - height) / 2;
    IRect {
        x0,
        y0,
        x1: x0 + padded_width,
        y1: y0 + padded_height,
    }
}

fn render(page: &mupdf::Page, bbox: &BoundingBox) -> anyhow::Result<image::RgbImage> {
    let longest = bbox.width.max(bbox.height).max(1.0);
    let scale = (TARGET_LONGEST_PX / longest).clamp(MIN_RENDER_SCALE, MAX_RENDER_SCALE);
    let scaled = IRect {
        x0: (bbox.x * scale).floor() as i32,
        y0: (bbox.y * scale).floor() as i32,
        x1: ((bbox.x + bbox.width) * scale).ceil() as i32,
        y1: ((bbox.y + bbox.height) * scale).ceil() as i32,
    };
    let canvas = pad_to_aspect(scaled);
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap_with_clip(&pixmap, scaled)?;
    page.run(&device, &Matrix::new_scale(scale, scale))?;
    drop(device);
    let decoded = decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("region did not decode: {reason}"))?;
    Ok(decoded.pixels)
}

/// The words the page draws whose centre falls inside `bbox`, in page order.
/// This is the glyph run an admitted region would displace — the thing to
/// compare the recognizer's answer against.
fn native_within(page: &mupdf::Page, bbox: &BoundingBox) -> anyhow::Result<String> {
    let text_page = page.to_text_page(TextPageFlags::ACCURATE_BBOXES)?;
    let mut out = String::new();
    for block in text_page.blocks() {
        for line in block.lines() {
            for ch in line.chars() {
                let Some(c) = ch.char() else { continue };
                let q = ch.quad();
                let cx = (q.ul.x + q.lr.x) / 2.0;
                let cy = (q.ul.y + q.lr.y) / 2.0;
                if cx >= bbox.x
                    && cx <= bbox.x + bbox.width
                    && cy >= bbox.y
                    && cy <= bbox.y + bbox.height
                {
                    out.push(c);
                }
            }
        }
    }
    Ok(out)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let pdf = args.next().expect("usage: <pdf> <page…>");
    let wanted: Vec<i32> = args.filter_map(|a| a.parse().ok()).collect();
    let dir = model_dir();

    let detector = doclayout::DocLayout::load(&dir, 4).context("the layout detector loads")?;
    let recognizer = GraniteDocling::load(&dir, 1, 4).context("the recognizer loads")?;
    let out = PathBuf::from(std::env::var("PROBE_OUT").unwrap_or_else(|_| "/tmp".into()));

    let document = Document::open(&pdf)?;
    for number in wanted {
        let page = document.load_page(number - 1)?;
        let bounds = page.bounds()?;
        let page_box = BoundingBox {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.x1 - bounds.x0,
            height: bounds.y1 - bounds.y0,
        };
        // The detector's own render: whole page, stretched into its square.
        let square = {
            let side = detector.input_side();
            let canvas = IRect {
                x0: 0,
                y0: 0,
                x1: side as i32,
                y1: side as i32,
            };
            let mut pixmap =
                mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
            pixmap.clear_with(0xff)?;
            let device = Device::from_pixmap(&pixmap)?;
            page.run(
                &device,
                &Matrix::new_scale(side as f32 / page_box.width, side as f32 / page_box.height),
            )?;
            drop(device);
            decode(
                pixmap.width(),
                pixmap.height(),
                pixmap.n() as usize,
                pixmap.stride() as usize,
                pixmap.samples(),
            )
            .map_err(|reason| anyhow::anyhow!("page did not decode: {reason}"))?
            .pixels
        };
        let found = detector.detect(&square)?;

        let mut crops = Vec::new();
        let mut about = Vec::new();
        for (ordinal, detection) in found.iter().enumerate() {
            if detection.label != "inline_formula" && detection.label != "display_formula" {
                continue;
            }
            let area = BoundingBox {
                x: page_box.x + detection.bbox.x * page_box.width - MARGIN_POINTS,
                y: page_box.y + detection.bbox.y * page_box.height - MARGIN_POINTS,
                width: detection.bbox.width * page_box.width + 2.0 * MARGIN_POINTS,
                height: detection.bbox.height * page_box.height + 2.0 * MARGIN_POINTS,
            };
            let native = native_within(&page, &area)?;
            let crop = render(&page, &area)?;
            let name = out.join(format!("p{number}-{ordinal}-{}.png", detection.label));
            crop.save(&name)?;
            about.push((detection.label, detection.score, area, native, name));
            crops.push(crop);
        }

        println!("\n── page {number}: {} crops ──", crops.len());
        if crops.is_empty() {
            continue;
        }
        let started = std::time::Instant::now();
        let answers = recognizer.spot_batch(&crops)?;
        let elapsed = started.elapsed();
        for ((label, score, area, native, name), answer) in about.iter().zip(&answers) {
            println!(
                "\n{label} {score:.2}  {:.0}x{:.0}pt at ({:.0},{:.0})  → {}",
                area.width,
                area.height,
                area.x,
                area.y,
                name.display()
            );
            println!("  native : {native:?}");
            if answer.regions.is_empty() {
                println!(
                    "  read   : nothing ({} not text, {} unroutable)",
                    answer.not_text, answer.unroutable
                );
            }
            for region in &answer.regions {
                let verdict = match region.kind {
                    RegionKind::Formula => {
                        if latex_parses(&region.text) {
                            "ADMITTED (formula, latex parses)"
                        } else {
                            "refused: invalid latex"
                        }
                    }
                    // Admission refuses any typeset region whose kind does not
                    // supersede a glyph run — this is the wall the display
                    // path already meets 9600 times over.
                    kind if !kind.supersedes_native_glyphs() => "refused: not a superseding kind",
                    _ => "admitted",
                };
                println!(
                    "  read   : {:?} conf {:.2} → {verdict}\n           {:?}",
                    region.kind, region.confidence, region.text
                );
            }
        }
        println!(
            "\n  {} crops recognized in {elapsed:?} ({:?} each)",
            crops.len(),
            elapsed / crops.len().max(1) as u32
        );
    }
    Ok(())
}
