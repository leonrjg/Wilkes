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

use std::sync::Arc;

use wilkes_core::extract::image::serialize::{reading_regions, superseded_areas};
use wilkes_core::extract::image::{doclayout, granite_docling, texify, NativeImageAnalyzer};
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ContentExtractor;
use wilkes_core::types::ImageScope;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wilkes_core=info".into()),
        )
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

    let analyzer = NativeImageAnalyzer::new(
        Box::new(granite_docling::GraniteDocling::load(&dir, 1, 6)?),
        None,
        ImageScope::TypesetOnly,
        Some(Box::new(doclayout::DocLayout::load(&dir, 6)?)),
    )
    .with_formula_reader(Box::new(texify::Texify::load(&dir, 6)?));

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
