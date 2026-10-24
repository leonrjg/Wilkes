//! Headless latency/geometry probe. Never renders the complete comic.
use std::{path::Path, time::Instant};
fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: comic_probe <archive.cbz>");
    let path = Path::new(&path);
    let start = Instant::now();
    let count = wilkes_core::extract::document::comic::page_count(path)?;
    println!("directory: {count} pages in {:?}", start.elapsed());
    let reference = mupdf::Document::open(path.to_str().unwrap())?;
    let mut max = std::time::Duration::ZERO;
    let mut total_bytes = 0;
    for number in [1, count.min(100), count, 1] {
        let start = Instant::now();
        let packet = wilkes_core::extract::document::comic::page(path, number)?;
        let elapsed = start.elapsed();
        max = max.max(elapsed);
        let width = f32::from_le_bytes(packet[4..8].try_into()?);
        let height = f32::from_le_bytes(packet[8..12].try_into()?);
        let bounds = reference.load_page(number as i32 - 1)?.bounds()?;
        anyhow::ensure!(
            width == bounds.x1 && height == bounds.y1,
            "page geometry mismatch"
        );
        total_bytes += packet.len();
        println!(
            "page {number}: {} bytes, {width} × {height} points in {elapsed:?}",
            packet.len()
        );
    }
    println!("four requests: {total_bytes} bytes; slowest {max:?}");
    if std::env::args().any(|arg| arg == "--sweep") {
        let start = Instant::now();
        let mut peak_page = 0;
        for number in 1..=count {
            let packet = wilkes_core::extract::document::comic::page(path, number)?;
            peak_page = peak_page.max(packet.len());
        }
        println!(
            "sequential sweep: {count} pages in {:?}; largest packet {peak_page} bytes",
            start.elapsed()
        );
    }
    Ok(())
}
