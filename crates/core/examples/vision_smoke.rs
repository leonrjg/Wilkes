//! End-to-end check of the Apple Vision recognizer, over real images.
//!
//! `cargo run -p wilkes-core --features recognize-vision --example vision_smoke -- <image>...`
//!
//! Exercises the whole path the worker uses: the Objective-C shim, the JSON
//! boundary, the rect-to-quad flip and admission. Unit tests cover the geometry
//! arithmetic; nothing but running it covers the FFI.

use std::time::Instant;

use wilkes_core::extract::image::ocr::OcrEngine;
use wilkes_core::extract::image::vision::AppleVision;

fn main() -> anyhow::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!paths.is_empty(), "usage: vision_smoke <image>...");

    let engine = AppleVision::load()?;
    println!("identity: {}", engine.identity());
    println!("threshold: {}\n", engine.admission_threshold());

    let mut images = Vec::with_capacity(paths.len());
    for path in &paths {
        images.push(image::open(path)?.to_rgb8());
    }

    let started = Instant::now();
    let readings = engine.spot_batch(&images)?;
    let elapsed = started.elapsed();

    let mut admitted = 0usize;
    let mut rejected = 0usize;
    let mut chars = 0usize;
    for (path, reading) in paths.iter().zip(&readings) {
        for region in &reading.regions {
            if region.confidence >= engine.admission_threshold() {
                admitted += 1;
                chars += region.text.chars().count();
            } else {
                rejected += 1;
            }
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        println!("{:>4} regions  {name}", reading.regions.len());
        if let Some(first) = reading.regions.first() {
            let quad = first.quad;
            println!(
                "       first: {:?} conf {:.3} quad ({:.3},{:.3})-({:.3},{:.3})",
                first.kind, first.confidence, quad[0].x, quad[0].y, quad[2].x, quad[2].y
            );
            println!(
                "       text: {:?}",
                first.text.chars().take(72).collect::<String>()
            );
        }
    }

    println!(
        "\n{} images in {:.2}s ({:.1} ms/image), {admitted} regions admitted, \
         {rejected} below threshold, {chars} chars",
        images.len(),
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / images.len() as f64,
    );
    Ok(())
}
