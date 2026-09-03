//! Does a *purpose-built formula recognizer* read what a page parser could not?
//!
//! `inline_probe` established that granite-docling — a document parser — comes
//! apart on fragments of a page: ten inline crops, none admitted, four decodes
//! looping to their token cap. The open question that left is whether that is a
//! property of *fragments* or a property of *that model*. This answers it with
//! Texify (Donut encoder, mBART decoder), the ONNX-exported model of the same
//! class as UniMERNet and trained on exactly this input: a cropped expression.
//!
//! It is a gating check and nothing more. Nothing here is wired into
//! extraction, no admission rule sees it, and the decoder has no KV cache —
//! the whole prefix is recomputed each step, which is the simplest thing that
//! can answer the question.
//!
//!     cargo run --release --example formula_probe -- <model_dir> <pdf> <page> [x y w h]
//!
//! Loads its models in this process. The application is forbidden from doing
//! that — see the "no inference in the host process" invariant in `AGENTS.md`
//! — but a probe *is* the model's process, and Ctrl-C is the kill. Not
//! precedent for anything under `src/`.

use anyhow::Context as _;
use mupdf::{Colorspace, Device, Document, IRect, Matrix};
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use wilkes_core::extract::image::{decode, doclayout, LayoutModel};
use wilkes_core::types::BoundingBox;

/// Donut's square, from `preprocessor_config.json`.
const SIDE: usize = 420;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// From `generation_config.json`.
const START: i64 = 0;
const EOS: i64 = 2;
const MAX_TOKENS: usize = 384;

/// The same margin and scale band production renders typeset regions at.
const MARGIN_POINTS: f32 = 8.0;
const TARGET_LONGEST_PX: f32 = 1600.0;
const MIN_RENDER_SCALE: f32 = 2.0;
const MAX_RENDER_SCALE: f32 = 8.0;

fn render(page: &mupdf::Page, bbox: &BoundingBox) -> anyhow::Result<image::RgbImage> {
    let longest = bbox.width.max(bbox.height).max(1.0);
    let scale = (TARGET_LONGEST_PX / longest).clamp(MIN_RENDER_SCALE, MAX_RENDER_SCALE);
    let rect = IRect {
        x0: (bbox.x * scale).floor() as i32,
        y0: (bbox.y * scale).floor() as i32,
        x1: ((bbox.x + bbox.width) * scale).ceil() as i32,
        y1: ((bbox.y + bbox.height) * scale).ceil() as i32,
    };
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), rect, false)?;
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap_with_clip(&pixmap, rect)?;
    page.run(&device, &Matrix::new_scale(scale, scale))?;
    drop(device);
    Ok(decode(
        pixmap.width(),
        pixmap.height(),
        pixmap.n() as usize,
        pixmap.stride() as usize,
        pixmap.samples(),
    )
    .map_err(|reason| anyhow::anyhow!("{reason}"))?
    .pixels)
}

/// Donut's preprocessing: fit inside the square keeping the aspect, pad the
/// rest with paper, normalize. `do_align_long_axis` is false, so nothing is
/// rotated.
fn preprocess(crop: &image::RgbImage) -> Vec<f32> {
    let (w, h) = crop.dimensions();
    // Fill the square rather than merely fit inside it. A crop smaller than
    // 420px left at its own size sits in a field of paper, and the model was
    // trained on expressions that fill the frame — which is most of why the
    // first run of this probe read every small inline crop as nonsense.
    let mut scale = (SIDE as f32 / w as f32).min(SIDE as f32 / h as f32);
    if std::env::var("PROBE_NO_UPSCALE").is_ok() {
        scale = scale.min(1.0);
    }
    let (tw, th) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let fitted = image::imageops::resize(crop, tw, th, image::imageops::FilterType::Lanczos3);
    let mut canvas = image::RgbImage::from_pixel(SIDE as u32, SIDE as u32, image::Rgb([255; 3]));
    image::imageops::replace(
        &mut canvas,
        &fitted,
        ((SIDE as u32 - tw) / 2) as i64,
        ((SIDE as u32 - th) / 2) as i64,
    );

    let mut chw = vec![0f32; 3 * SIDE * SIDE];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let px = canvas.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                chw[c * SIDE * SIDE + y * SIDE + x] =
                    ((f32::from(px[c]) / 255.0) - MEAN[c]) / STD[c];
            }
        }
    }
    chw
}

fn read(
    encoder: &mut Session,
    decoder: &mut Session,
    tokenizer: &Tokenizer,
    crop: &image::RgbImage,
) -> anyhow::Result<(String, usize)> {
    let pixels = Tensor::from_array((vec![1i64, 3, SIDE as i64, SIDE as i64], preprocess(crop)))?;
    let encoded = encoder
        .run(vec![(
            "pixel_values".to_string(),
            SessionInputValue::from(pixels),
        )])
        .map_err(|e| anyhow::anyhow!("encoder: {e}"))?;
    let (shape, hidden) = encoded[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("encoder output: {e}"))?;
    let hidden: Vec<f32> = hidden.to_vec();
    let shape: Vec<i64> = shape.to_vec();

    let mut ids: Vec<i64> = vec![START];
    for _ in 0..MAX_TOKENS {
        let input = Tensor::from_array((vec![1i64, ids.len() as i64], ids.clone()))?;
        let states = Tensor::from_array((shape.clone(), hidden.clone()))?;
        let out = decoder
            .run(vec![
                ("input_ids".to_string(), SessionInputValue::from(input)),
                (
                    "encoder_hidden_states".to_string(),
                    SessionInputValue::from(states),
                ),
            ])
            .map_err(|e| anyhow::anyhow!("decoder: {e}"))?;
        let (logit_shape, logits) = out[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("decoder output: {e}"))?;
        let vocab = logit_shape[2] as usize;
        let last = &logits[(ids.len() - 1) * vocab..ids.len() * vocab];
        let next = last
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index as i64)
            .expect("a vocabulary");
        if next == EOS {
            break;
        }
        ids.push(next);
    }

    let steps = ids.len() - 1;
    let text = tokenizer
        .decode(
            &ids[1..].iter().map(|id| *id as u32).collect::<Vec<u32>>(),
            true,
        )
        .map_err(|e| anyhow::anyhow!("detokenize: {e}"))?;
    Ok((text, steps))
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: <model_dir> <pdf> <page> [box]"));
    let pdf = args.next().expect("a pdf");
    let number: i32 = args.next().expect("a page").parse()?;
    let explicit: Vec<f32> = args.filter_map(|a| a.parse().ok()).collect();

    let mut encoder = Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(6)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(dir.join("encoder_model_quantized.onnx"))
        .map_err(|e| anyhow::anyhow!("encoder load: {e}"))?;
    let mut decoder = Session::builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_intra_threads(6)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .commit_from_file(dir.join("decoder_model_quantized.onnx"))
        .map_err(|e| anyhow::anyhow!("decoder load: {e}"))?;
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("tokenizer: {e}"))?;
    println!("texify loaded on ort 2.0.0-rc.13");

    let document = Document::open(std::path::Path::new(&pdf))?;
    let page = document.load_page(number - 1)?;
    let bounds = page.bounds()?;
    let page_box = BoundingBox {
        x: bounds.x0,
        y: bounds.y0,
        width: bounds.x1 - bounds.x0,
        height: bounds.y1 - bounds.y0,
    };

    // Either the box the caller named, or every area the layout detector calls
    // a formula on this page.
    let areas: Vec<(String, BoundingBox)> = if explicit.len() == 4 {
        vec![(
            "asked for".to_string(),
            BoundingBox {
                x: explicit[0] - MARGIN_POINTS,
                y: explicit[1] - MARGIN_POINTS,
                width: explicit[2] + 2.0 * MARGIN_POINTS,
                height: explicit[3] + 2.0 * MARGIN_POINTS,
            },
        )]
    } else {
        let detector = doclayout::DocLayout::load(
            &std::path::PathBuf::from(std::env::var("PROBE_MODEL_DIR").unwrap_or_else(|_| {
                format!(
                    "{}/Library/Application Support/app.wilkes/models",
                    std::env::var("HOME").unwrap_or_default()
                )
            })),
            4,
        )
        .context("the layout detector loads")?;
        let side = detector.input_side();
        let canvas = IRect {
            x0: 0,
            y0: 0,
            x1: side as i32,
            y1: side as i32,
        };
        let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
        pixmap.clear_with(0xff)?;
        let device = Device::from_pixmap(&pixmap)?;
        page.run(
            &device,
            &Matrix::new_scale(side as f32 / page_box.width, side as f32 / page_box.height),
        )?;
        drop(device);
        let square = decode(
            pixmap.width(),
            pixmap.height(),
            pixmap.n() as usize,
            pixmap.stride() as usize,
            pixmap.samples(),
        )
        .map_err(|reason| anyhow::anyhow!("{reason}"))?
        .pixels;
        detector
            .detect_document(1, &mut |_| Ok(square.clone()))?
            .remove(0)
            .into_iter()
            .filter(|d| d.label.contains("formula") || d.label == "table")
            .map(|d| {
                (
                    format!("{} {:.2}", d.label, d.score),
                    BoundingBox {
                        x: page_box.x + d.bbox.x * page_box.width - MARGIN_POINTS,
                        y: page_box.y + d.bbox.y * page_box.height - MARGIN_POINTS,
                        width: d.bbox.width * page_box.width + 2.0 * MARGIN_POINTS,
                        height: d.bbox.height * page_box.height + 2.0 * MARGIN_POINTS,
                    },
                )
            })
            .collect()
    };

    println!("\n{} area(s) on page {number}", areas.len());
    for (label, area) in &areas {
        let crop = render(&page, area)?;
        if let Ok(out) = std::env::var("PROBE_OUT") {
            let name = format!(
                "{out}/p{number}-{:.0}x{:.0}-{}.png",
                area.x,
                area.y,
                label.split_whitespace().next().unwrap_or("area")
            );
            crop.save(&name)?;
            println!("  saved {name}");
        }
        let started = std::time::Instant::now();
        let (text, steps) = read(&mut encoder, &mut decoder, &tokenizer, &crop)?;
        println!(
            "\n{label}  {:.0}x{:.0}pt at ({:.0},{:.0})  {steps} tokens in {:?}",
            area.width,
            area.height,
            area.x,
            area.y,
            started.elapsed()
        );
        println!("  {text}");
    }
    Ok(())
}
