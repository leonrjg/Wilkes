//! What the reading and a reading surface get for a page, end to end.
//!
//! Runs the real extractor with no analyzer — so nothing is routed and nothing
//! is recognized — and prints the page's reading beside the superseded areas
//! [`wilkes_core::extract::image::serialize::superseded_areas`] resolves from
//! it. The second list is what a PDF reader is handed to put back into a copy.
//!
//!     cargo run --release --example script_reading -- <pdf> [needle]

use wilkes_core::extract::image::serialize::{reading_regions, superseded_areas};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ContentExtractor;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let pdf = args.next().expect("usage: <pdf> [needle]");
    let needle = args.next();

    let extractor = PdfExtractor::new();
    let started = std::time::Instant::now();
    let content = extractor.extract(std::path::Path::new(&pdf))?;
    println!(
        "read in {:?}, {} bytes",
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

    if let Some(needle) = &needle {
        println!("\n── lines containing {needle:?} ──");
        for line in content
            .text
            .lines()
            .filter(|line| line.contains(needle.as_str()))
        {
            println!("  {line}");
        }
    }

    if let Ok(path) = std::env::var("AREAS_JSON") {
        std::fs::write(&path, serde_json::to_vec(&areas)?)?;
        println!("\nwrote {} areas to {path}", areas.len());
        return Ok(());
    }

    println!("\n── the first 24 areas a reader would substitute ──");
    for area in areas.iter().take(24) {
        println!(
            "  p{:<4} [{:.1},{:.1} {:.1}x{:.1}]  {:?}",
            area.page, area.bbox.x, area.bbox.y, area.bbox.width, area.bbox.height, area.text
        );
    }
    Ok(())
}
