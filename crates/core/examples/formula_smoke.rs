//! Exercise a formula recognizer through the same dispatch as the worker.
//!
//! cargo run -p wilkes-core --example formula_smoke -- <model_dir> <model_id> <png>...
//!
//! This standalone process owns the model; Ctrl-C kills its inference. It is
//! not precedent for loading models in the application host.
use anyhow::Context;
use std::path::Path;
use std::time::Instant;
use wilkes_core::extract::image::{dispatch, ocr};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: formula_smoke <model_dir> <model_id> <png>..."
    );
    let dir = Path::new(&args[0]);
    let model =
        dispatch::formula_model(dir, Some(&args[1]))?.context("formula reader unavailable")?;
    let engine = dispatch::load_recognizer_local(model.engine, &model.model_id, dir, "cpu")?;
    let images = args
        .iter()
        .skip(2)
        .map(|path| image::open(path).map(|im| im.to_rgb8()))
        .collect::<Result<Vec<_>, _>>()?;
    let started = Instant::now();
    let answers = engine.spot_batch(&images)?;
    anyhow::ensure!(answers.len() == images.len(), "lost a crop");
    println!(
        "{} crops in {:.3}s",
        answers.len(),
        started.elapsed().as_secs_f64()
    );
    for (path, answer) in args.iter().skip(2).zip(&answers) {
        println!(
            "{}",
            serde_json::json!({ "path": path, "regions": answer.regions.iter().map(|r| serde_json::json!({
            "latex": r.text, "truncated": r.truncated, "parses": ocr::latex_parses(&r.text)
        })).collect::<Vec<_>>() })
        );
    }
    engine.release();
    let repeated = engine.spot_batch(&images[..1])?;
    anyhow::ensure!(
        serde_json::to_value(&repeated[0])? == serde_json::to_value(&answers[0])?,
        "release/reload changed the reading"
    );
    println!("release/reload: identical first-crop result");
    Ok(())
}
