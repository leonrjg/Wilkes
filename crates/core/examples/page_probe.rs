//! What granite costs on a whole page, which is the input it was built for.
//!
//! Every measurement so far has fed it crops. This prices the other
//! architecture — no layout model, no routing, read the page and keep only the
//! parts worth keeping — so the choice between them is made on numbers.
//!
//!     cargo run --release --example page_probe -- <pdf> <page…>
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use mupdf::{Colorspace, Device, Document, IRect, Matrix};
use wilkes_core::extract::image::granite_docling::GraniteDocling;
use wilkes_core::extract::image::{decode, ocr::OcrEngine};

/// The scale a page is rendered at for a page parser: enough that a subscript
/// survives, which is roughly 150 dpi.
const SCALE: f32 = 2.0;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let pdf = args.next().expect("usage: <pdf> <page…>");
    let pages: Vec<i32> = args.filter_map(|a| a.parse().ok()).collect();
    let dir = std::path::PathBuf::from(format!(
        "{}/Library/Application Support/app.wilkes/models",
        std::env::var("HOME").unwrap_or_default()
    ));

    let granite = GraniteDocling::load(&dir, 1, 6)?;
    let document = Document::open(std::path::Path::new(&pdf))?;

    for number in pages {
        let page = document.load_page(number - 1)?;
        let bounds = page.bounds()?;
        let rect = IRect {
            x0: 0,
            y0: 0,
            x1: ((bounds.x1 - bounds.x0) * SCALE) as i32,
            y1: ((bounds.y1 - bounds.y0) * SCALE) as i32,
        };
        let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, false)?;
        pixmap.clear_with(0xff)?;
        let device = Device::from_pixmap(&pixmap)?;
        page.run(&device, &Matrix::new_scale(SCALE, SCALE))?;
        drop(device);
        let rendered = decode(
            pixmap.width(),
            pixmap.height(),
            pixmap.n() as usize,
            pixmap.stride() as usize,
            pixmap.samples(),
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .pixels;

        let started = std::time::Instant::now();
        let read = granite.spot_batch(std::slice::from_ref(&rendered))?;
        let elapsed = started.elapsed();
        let regions = &read[0];
        let formulas = regions
            .regions
            .iter()
            .filter(|r| format!("{:?}", r.kind) == "Formula")
            .count();
        println!(
            "page {number}: {:?}  {} region(s), {formulas} formula(s), {} not text, {} unroutable",
            elapsed,
            regions.regions.len(),
            regions.not_text,
            regions.unroutable
        );
        for region in regions.regions.iter().take(40) {
            let text = region.text.replace('\n', " ⏎ ");
            println!(
                "   {:?} {:.2}  {}",
                region.kind,
                region.confidence,
                &text[..text.len().min(110)]
            );
        }
    }
    Ok(())
}
