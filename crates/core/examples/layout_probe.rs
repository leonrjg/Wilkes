//! Gating check for the layout detector: does its graph load and run on the
//! `ort` already in the tree, and does it find on a real page what the page
//! actually holds?
//!
//!     cargo run --release --example layout_probe -- <model.onnx> [pdf] [page…]

use anyhow::Context as _;
use mupdf::{Colorspace, Device, Document, IRect, Matrix};
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, ValueType};

/// PP-DocLayoutV2's classes, in the order its `config.json` lists them. The
/// index is the class id the graph returns, so the order is the contract.
const LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

const SIDE: usize = 800;

fn describe(kind: &ValueType) -> String {
    match kind {
        ValueType::Tensor { ty, shape, .. } => format!("{ty:?} {shape:?}"),
        other => format!("{other:?}"),
    }
}

/// One page, rasterized to the square the graph declares. `keep_ratio` is
/// false in the model's own preprocessing, so the page is stretched rather
/// than letterboxed and the inverse map is two independent scales.
fn render_page(page: &mupdf::Page) -> anyhow::Result<(Vec<f32>, f32, f32)> {
    let bounds = page.bounds()?;
    let (pw, ph) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
    let (sx, sy) = (SIDE as f32 / pw, SIDE as f32 / ph);
    let canvas = IRect {
        x0: 0,
        y0: 0,
        x1: SIDE as i32,
        y1: SIDE as i32,
    };
    let mut pixmap = mupdf::Pixmap::new_with_rect(&Colorspace::device_rgb(), canvas, false)?;
    pixmap.clear_with(0xff)?;
    let device = Device::from_pixmap(&pixmap)?;
    page.run(&device, &Matrix::new_scale(sx, sy))?;
    drop(device);

    let (w, h, n, stride) = (
        pixmap.width() as usize,
        pixmap.height() as usize,
        pixmap.n() as usize,
        pixmap.stride() as usize,
    );
    let samples = pixmap.samples();
    // CHW, RGB, divided by 255: the `Preprocess` block is Resize, then
    // NormalizeImage with mean 0 and std 1, then Permute.
    let mut chw = vec![0f32; 3 * w * h];
    for y in 0..h {
        for x in 0..w {
            let px = &samples[y * stride + x * n..];
            for c in 0..3 {
                chw[c * w * h + y * w + x] = f32::from(px[c]) / 255.0;
            }
        }
    }
    Ok((chw, sx, sy))
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("usage: <model.onnx> [pdf] [page…]");
    let pdf = args.next();
    let pages: Vec<i32> = args.filter_map(|a| a.parse().ok()).collect();

    let mut session = Session::builder()
        .map_err(|e| anyhow::anyhow!("builder: {e}"))?
        .with_intra_threads(4)
        .map_err(|e| anyhow::anyhow!("threads: {e}"))?
        .commit_from_file(&model)
        .map_err(|e| anyhow::anyhow!("LOAD FAILED: {e}"))?;
    println!("loaded on ort 2.0.0-rc.13");
    println!("inputs:");
    for input in session.inputs() {
        println!("  {:<16} {}", input.name(), describe(&input.dtype()));
    }
    println!("outputs:");
    for output in session.outputs() {
        println!("  {:<16} {}", output.name(), describe(&output.dtype()));
    }

    let Some(pdf) = pdf else { return Ok(()) };
    let document = Document::open(&pdf)?;

    for number in pages {
        let page = document.load_page(number - 1)?;
        let (chw, sx, sy) = render_page(&page).context("could not render the page")?;

        let image = Tensor::from_array((vec![1i64, 3, SIDE as i64, SIDE as i64], chw))?;
        // Identity, so the boxes come back in the square's own coordinates and
        // the map back to page points stays here, where it can be checked.
        let unit = Tensor::from_array((vec![1i64, 2], vec![1.0f32, 1.0]))?;
        let shape = Tensor::from_array((vec![1i64, 2], vec![SIDE as f32, SIDE as f32]))?;
        let feed: Vec<(String, SessionInputValue)> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .map(|name| {
                let value: SessionInputValue = match name.as_str() {
                    "image" => image.clone().into(),
                    "scale_factor" => unit.clone().into(),
                    "im_shape" => shape.clone().into(),
                    other => panic!("unknown input {other}"),
                };
                (name, value)
            })
            .collect();

        let started = std::time::Instant::now();
        let outputs = session.run(feed).map_err(|e| anyhow::anyhow!("RUN: {e}"))?;
        let (shape0, boxes) = outputs[0].try_extract_tensor::<f32>().unwrap();
        let count = outputs[1]
            .try_extract_tensor::<i32>()
            .map(|(_, d)| d[0])
            .unwrap_or(shape0[0] as i32);
        let cols = shape0[1] as usize;
        println!(
            "\n── page {number}: {} rows x {cols} cols, count={count}, {:?}",
            shape0[0],
            started.elapsed()
        );

        let mut rows: Vec<&[f32]> = boxes.chunks(cols).collect();
        rows.sort_by(|a, b| b[1].partial_cmp(&a[1]).expect("no NaN scores"));
        for row in rows.iter().take(14) {
            let (class, score) = (row[0] as usize, row[1]);
            if score < 0.30 {
                break;
            }
            let label = LABELS.get(class).copied().unwrap_or("?");
            println!(
                "  {label:<18} {score:.3}  square[{:.0},{:.0} → {:.0},{:.0}]  \
                 page[{:.0},{:.0} → {:.0},{:.0}]  tail={:?}",
                row[2],
                row[3],
                row[4],
                row[5],
                row[2] / sx,
                row[3] / sy,
                row[4] / sx,
                row[5] / sy,
                &row[6..]
            );
        }
    }
    Ok(())
}
