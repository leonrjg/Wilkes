//! UniMERNet under ONNX Runtime: the third formula reader, and the origin of
//! the other two.
//!
//! UniMERNet is the model this tree keeps arriving at from the side. Its
//! vocabulary is the one [`super::texify`] and [`super::pp_formulanet`] both
//! decode with — one 50,000-merge BPE, byte-identical in all three
//! installations. Its eval transform is the one `pp_formulanet` runs, because
//! PP-FormulaNet is PaddlePaddle's re-implementation of this architecture and
//! inherited the recipe with it. And its encoder-decoder pair is the same
//! Donut Swin over mBART that Texify is, exported into the same three graphs.
//! So adding it costs one pinned checkpoint and one preprocessing frame; the
//! decode loop it runs through is [`super::donut_formula`]'s, which is
//! Texify's loop with the parts that were Texify's own taken out.
//!
//! What it reads that the other two do not is a matter of what it was trained
//! on: UniMER-1M includes photographed and handwritten expressions, and this
//! export returns `9 \times 9 + 1 3 \times 1 3 - ( 3 + 3 + 1 ) = 2 4 3` for a
//! handwritten crop and a correct `I ( u ) = \int_{\Omega} …` for a screen
//! capture. Neither is a benchmark. Nothing in this repository has measured
//! the three formula readers against each other on the same corpus, and until
//! something has, the reason to offer this one is that it is a different model
//! trained on different data, not that it is better.
//!
//! ## Why this is UniMERNet-S and not UniMERNet-T
//!
//! The tiny checkpoint was what was asked for and it is not here, because
//! there is nothing to download. UniMERNet publishes four checkpoints —
//! `wanderkid/unimernet`, `_base`, `_small`, `_tiny` — and every one of them
//! is a PyTorch `.pth`. There is no ONNX export of the tiny checkpoint on the
//! model hub, on ModelScope, in OAR OCR's releases, or anywhere a search
//! reaches: the one project that says it has one
//! (`torvexlabs/unimernet-onnx`) hosts its artifacts in a repository that
//! answers 401. A recognizer whose weights cannot be fetched is a catalogue
//! row that is permanently uninstallable, which is worse than not offering it,
//! so what is pinned below is the small checkpoint, which does have an export.
//!
//! If a tiny export appears, it is this file with four digests changed and a
//! frame that is already the same: the tiny and small checkpoints share this
//! vocabulary, this 192x672 input and this eval transform, and differ in the
//! encoder's depth.
//!
//! ## Why an export with no name on it
//!
//! `Cooper114/unimernet-onnx` is an anonymous repository with no stars and no
//! downloads, and that is a real weakness, so it is pinned the way
//! [`super::texify`]'s equally unofficial export is: at a commit, with every
//! file checked against its digest at install and again at load. What that
//! buys is that the artifacts cannot change under a stored reading — the worst
//! a moved repository can do is fail to download.
//!
//! Two things say the export is what it claims. Its vocabulary hashes to the
//! same `02c318d9…` as UniMERNet's own, which is not something a mistaken
//! export produces. And it decodes: the five UniMER-Test crops this module was
//! developed against came back as correct LaTeX under exactly the
//! preprocessing below, which is the check that matters, because a graph
//! wired to the wrong checkpoint returns noise rather than plausible
//! mathematics.
//!
//! The export declares no licence of its own. UniMERNet is Apache-2.0 and this
//! is a derivative of it, so that is what [`inventory`] discloses, naming the
//! export separately so nobody has to infer it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::{imageops, RgbImage};
use tokenizers::Tokenizer;

use super::donut_formula::{Checkpoint, DonutFormula};

// ── The pinned recipe ────────────────────────────────────────────────────────

pub const MODEL_ID: &str = "unimernet-small";

pub const ENCODER_GRAPH: &str = "encoder_model_quantized.onnx";
/// Step 0's decoder: no cache in, the whole `present.*` cache out.
pub const DECODER_GRAPH: &str = "decoder_model_quantized.onnx";
/// Every later step's decoder: one token in, the cache in and back out.
pub const DECODER_WITH_PAST_GRAPH: &str = "decoder_with_past_model_quantized.onnx";
pub const TOKENIZER: &str = "tokenizer.json";

/// The frame a crop is fitted into, as the encoder graph declares it:
/// `pixel_values [batch, 3, 192, 672]`. Wide rather than square, which is the
/// shape an expression actually has and the reason this reader spends a third
/// of Texify's pixels on one.
pub const HEIGHT: u32 = 192;
pub const WIDTH: u32 = 672;

/// From the checkpoint's `config.json`: `decoder_start_token_id` and the
/// decoder's `eos_token_id`.
pub const START_TOKEN: i64 = 0;
pub const EOS_TOKEN: i64 = 2;

/// Texify's cap, for Texify's reason, and not a second measurement: a decode
/// that has not stopped by here has stopped saying anything, and a crop cut
/// off by it is marked truncated and refused by [`super::ocr::admit`].
pub const MAX_NEW_TOKENS: usize = 512;

/// Formulas are admitted on whether their LaTeX parses, never on a score.
pub const ADMISSION_THRESHOLD: f32 = 0.0;

const VOCAB_SIZE: usize = 50_000;

/// Where the export lives. A commit, not a branch — the graphs are the
/// contract between a crop and the LaTeX stored for it.
const EXPORT_REPO: &str = "Cooper114/unimernet-onnx";
const EXPORT_REVISION: &str = "411ee76221baaad144ffbf996d4deef8df013b54";

/// Filename, size, SHA-256, and where each is fetched from.
///
/// The three graphs come from the export; the vocabulary comes from UniMERNet
/// itself, at the same commit [`super::pp_formulanet`] takes it from and with
/// the same digest. Taking it from the export would have worked — it is the
/// same bytes — and taking it from the original means the file this reader
/// decodes with is the one its authors published, under the licence they
/// published it under, rather than a copy inside an anonymous mirror.
const ARTIFACTS: &[(&str, u64, &str, &str)] = &[
    (ENCODER_GRAPH, 64_884_402,
     "8ee7108e18fcf46f496b6e0d68a7dc8eb30d2190d76d6f8b0c4bf2c9a8a7db21",
     "https://huggingface.co/Cooper114/unimernet-onnx/resolve/411ee76221baaad144ffbf996d4deef8df013b54/small/encoder_model_quantized.onnx"),
    (DECODER_GRAPH, 144_823_893,
     "902f105dba11a1b5ef0b475ff69421ec3cdb88897b9b5350711a4eeb9d86b064",
     "https://huggingface.co/Cooper114/unimernet-onnx/resolve/411ee76221baaad144ffbf996d4deef8df013b54/small/decoder_model_quantized.onnx"),
    (DECODER_WITH_PAST_GRAPH, 138_075_003,
     "3593f94e158a006290b267dbe8f4606bf510242b5836d56a8757bce1b5234c00",
     "https://huggingface.co/Cooper114/unimernet-onnx/resolve/411ee76221baaad144ffbf996d4deef8df013b54/small/decoder_with_past_model_quantized.onnx"),
    (TOKENIZER, 2_140_013,
     "02c318d9cfa95bf323371762b8f838a82709530274d36dba6eca880f0add6cc4",
     "https://huggingface.co/wanderkid/unimernet/resolve/4be874ffb637a90929de479109c8a377689b5a09/tokenizer.json"),
];

/// The recipe a reading produced by this recognizer is stored under.
///
/// Names the cached decoder for the reason [`super::texify::identity`] does:
/// both decoder graphs are dynamically quantized over different tensors and do
/// not decode the same LaTeX, so which one ran is part of what the reading is.
pub fn identity() -> String {
    format!(
        "ort-2.0.0-rc.13+{MODEL_ID}+{EXPORT_REPO}@{EXPORT_REVISION}\
         +crop200-fit{HEIGHT}x{WIDTH}-triangle-blackpad-gray-v1+cap-{MAX_NEW_TOKENS}\
         +{DECODER_WITH_PAST_GRAPH}"
    )
}

pub fn footprint_bytes() -> u64 {
    ARTIFACTS.iter().map(|artifact| artifact.1).sum()
}

pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("recognizers").join(MODEL_ID)
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|(name, size, _, _)| {
        dir.join(name)
            .metadata()
            .map(|meta| meta.is_file() && meta.len() == *size)
            .unwrap_or(false)
    })
}

pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: EXPORT_REPO.to_string(),
        revision: EXPORT_REVISION.to_string(),
        license: "Apache-2.0".to_string(),
        license_url: "https://huggingface.co/wanderkid/unimernet_small".to_string(),
        derived_from: vec![
            "UniMERNet by OpenDataLab (Apache-2.0, wanderkid/unimernet_small) — the \
             checkpoint this exports"
                .to_string(),
            // Said rather than inferred silently, because this is disclosed
            // beside a download button: the terms above are the base model's,
            // and the export states none of its own.
            "ONNX export by Cooper114, which declares no licence of its own; its manifest \
             names wanderkid/unimernet_small as the source, and its vocabulary is \
             byte-identical to UniMERNet's"
                .to_string(),
            "Donut Swin vision encoder (MIT, NAVER)".to_string(),
            "mBART decoder (MIT, Facebook)".to_string(),
        ],
        artifacts: ARTIFACTS
            .iter()
            .map(|(name, size, hash, _)| crate::types::InventoriedArtifact {
                filename: (*name).to_string(),
                size_bytes: *size,
                sha256: (*hash).to_string(),
            })
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Fetch the artifacts into `model_dir`, checking each against its size and
/// digest before it is published where [`is_installed`] would count it.
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    let dir = install_dir(model_dir);
    std::fs::create_dir_all(&dir)?;
    for (name, size, hash, url) in ARTIFACTS {
        let target = dir.join(name);
        if super::verify_artifact(&target, *size, hash).is_ok() {
            continue;
        }
        // Only publish a verified download, so a partial transfer is never
        // installed — the same staging [`super::pp_formulanet::install`] does.
        let staged = tempfile::NamedTempFile::new_in(&dir)?;
        crate::models::downloader::LocalModelManager::download(
            url,
            staged.path(),
            *size,
            progress.clone(),
        )?;
        super::verify_artifact(staged.path(), *size, hash)?;
        staged
            .persist(&target)
            .with_context(|| format!("could not install {name}"))?;
    }
    Ok(())
}

// ── Preprocessing ────────────────────────────────────────────────────────────

/// UniMERNet's eval transform: crop to the ink, fit, pad with black, grey.
///
/// One plane of `height * width`, row-major. This is the whole of
/// `FormulaImageEvalProcessor` — normalize the luminance, keep everything
/// darker than 200 of 255, take that bounding box, scale it to fit the frame
/// with its aspect intact, centre it on black, and normalize the grey with the
/// statistics UniMERNet trained on.
///
/// It is here, and [`super::pp_formulanet::preprocess`] calls it, because it
/// is one recipe rather than two that resemble each other: PP-FormulaNet is
/// PaddlePaddle's re-implementation of this architecture and runs this
/// transform at a 384x384 frame. Two copies would be two places for the
/// threshold or the statistics to drift, and both readers' stored readings
/// name this recipe in their identity — a drift in one copy would silently
/// change what one of them means.
///
/// Two details are choices rather than transcription, and both are versioned
/// in the callers' identity strings. Luminance uses the RGB coefficients
/// explicitly, because `image`'s generic luma conversion uses different ones
/// from Pillow and OpenCV. And the crop is resampled straight to its fitted
/// size, rather than through the intermediate enlargement upstream's
/// resize-then-thumbnail pair allocates; the two agree on the final scale —
/// `min(height/h, width/w)` either way — and differ only in what they
/// resample through.
pub fn fit_and_gray(crop: &RgbImage, height: u32, width: u32) -> Result<Vec<f32>> {
    anyhow::ensure!(crop.width() > 0 && crop.height() > 0, "empty formula image");
    anyhow::ensure!(height > 0 && width > 0, "empty formula frame");
    let luma = |p: &image::Rgb<u8>| -> u8 {
        (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8
    };
    let (mut low, mut high) = (u8::MAX, u8::MIN);
    for p in crop.pixels() {
        let v = luma(p);
        low = low.min(v);
        high = high.max(v);
    }
    let mut bounds = (crop.width(), crop.height(), 0, 0);
    if high > low {
        for (x, y, p) in crop.enumerate_pixels() {
            if (luma(p) - low) as f32 * 255.0 / ((high - low) as f32) < 200.0 {
                bounds.0 = bounds.0.min(x);
                bounds.1 = bounds.1.min(y);
                bounds.2 = bounds.2.max(x);
                bounds.3 = bounds.3.max(y);
            }
        }
    }
    let cropped = if bounds.0 <= bounds.2 && bounds.1 <= bounds.3 {
        imageops::crop_imm(
            crop,
            bounds.0,
            bounds.1,
            bounds.2 - bounds.0 + 1,
            bounds.3 - bounds.1 + 1,
        )
        .to_image()
    } else {
        crop.clone()
    };
    let scale = (f64::from(height) / f64::from(cropped.height()))
        .min(f64::from(width) / f64::from(cropped.width()));
    let w = ((cropped.width() as f64 * scale) as u32).clamp(1, width);
    let h = ((cropped.height() as f64 * scale) as u32).clamp(1, height);
    let resized = imageops::resize(&cropped, w, h, imageops::FilterType::Triangle);
    let mut padded = RgbImage::new(width, height);
    imageops::overlay(
        &mut padded,
        &resized,
        ((width - w) / 2) as i64,
        ((height - h) / 2) as i64,
    );
    Ok(padded
        .pixels()
        .map(|p| {
            let gray = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0;
            (gray - 0.7931) / 0.1738
        })
        .collect())
}

/// The crop as this export's `pixel_values`.
///
/// One grey plane, repeated across three channels. The transform above ends at
/// one channel — upstream's takes `[:1]` of the tensor it built — and this
/// export's encoder declares `[batch, 3, 192, 672]`, so the plane is the
/// answer and the three channels are the graph's shape. Repeating is what the
/// export was converted and parity-checked with, and it is what the crops this
/// module was developed against decode correctly under.
pub fn preprocess(crop: &RgbImage) -> Result<Vec<f32>> {
    Ok(fit_and_gray(crop, HEIGHT, WIDTH)?.repeat(3))
}

// ── The checkpoint ───────────────────────────────────────────────────────────

/// Everything [`DonutFormula`] has to be told to read with this export.
pub static CHECKPOINT: Checkpoint = Checkpoint {
    model_id: MODEL_ID,
    encoder_graph: ENCODER_GRAPH,
    decoder_graph: DECODER_GRAPH,
    decoder_with_past_graph: DECODER_WITH_PAST_GRAPH,
    tokenizer: TOKENIZER,
    pixels: (3, HEIGHT as usize, WIDTH as usize),
    preprocess,
    identity,
    admission_threshold: ADMISSION_THRESHOLD,
    start_token: START_TOKEN,
    eos_token: EOS_TOKEN,
    max_new_tokens: MAX_NEW_TOKENS,
};

/// Load this recognizer in the calling process.
///
/// Must only be called from a worker subprocess — see [`super::dispatch`]'s
/// invariant. Every artifact is checked against its digest here and not merely
/// counted, because the export is an anonymous mirror and the digest is the
/// only thing standing between a moved file and a library read under a recipe
/// that never produced it.
pub fn load(model_dir: &Path, readers: usize, threads: usize) -> Result<DonutFormula> {
    let dir = install_dir(model_dir);
    for (name, size, hash, _) in ARTIFACTS {
        super::verify_artifact(&dir.join(name), *size, hash)?;
    }
    // The vocabulary is the contract between the ids the graph emits and the
    // LaTeX stored for them. Checked rather than assumed, exactly as
    // [`super::pp_formulanet::PpFormulaNet::load`] checks it: a file that
    // passed its digest and holds a different BPE cannot happen, but a future
    // re-pin that changed one and not the other could.
    let tokenizer = Tokenizer::from_file(dir.join(TOKENIZER))
        .map_err(|e| anyhow::anyhow!("could not load the formula tokenizer: {e}"))?;
    anyhow::ensure!(
        tokenizer.get_vocab_size(true) == VOCAB_SIZE
            && tokenizer.token_to_id("</s>") == Some(EOS_TOKEN as u32),
        "unexpected formula vocabulary"
    );
    DonutFormula::load(&CHECKPOINT, &dir, readers, threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every artifact is fetched from somewhere that cannot move under it, and
    /// pinned by digest as well. A branch would let a re-export change what
    /// every stored reading means while the digest here still described the
    /// file it replaced.
    #[test]
    fn every_artifact_is_pinned_to_a_commit_and_a_digest() {
        assert_eq!(EXPORT_REVISION.len(), 40);
        assert!(EXPORT_REVISION.chars().all(|c| c.is_ascii_hexdigit()));
        for (name, size, hash, url) in ARTIFACTS {
            assert_eq!(hash.len(), 64, "{name} is not pinned by digest");
            assert!(*size > 0, "{name} has no size to verify against");
            assert!(url.starts_with("https://"), "{name} is fetched over {url}");
            let revision = url
                .split("/resolve/")
                .nth(1)
                .and_then(|tail| tail.split('/').next())
                .unwrap_or_else(|| panic!("{name} is not fetched through a /resolve/ path"));
            assert_eq!(revision.len(), 40, "{name}: a branch, not a commit");
            assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
        }
        // Three graphs and a vocabulary, each a different file. A copy-paste
        // that made two digests equal would install one file twice.
        let mut digests: Vec<&str> = ARTIFACTS.iter().map(|artifact| artifact.2).collect();
        digests.sort_unstable();
        digests.dedup();
        assert_eq!(digests.len(), ARTIFACTS.len());
        assert_eq!(inventory().artifacts.len(), ARTIFACTS.len());
        assert_eq!(inventory().footprint_bytes, footprint_bytes());
    }

    /// The vocabulary is UniMERNet's own copy, and it is the same file
    /// [`super::super::pp_formulanet`] and [`super::super::texify`] decode
    /// with. Asserted because it is the one artifact here whose correctness
    /// can be checked without downloading anything: three readers that
    /// disagreed about the BPE would spell three different LaTeX for one
    /// expression.
    #[test]
    fn the_vocabulary_is_the_one_the_other_formula_readers_decode_with() {
        let (name, size, hash, url) = ARTIFACTS[3];
        assert_eq!(name, TOKENIZER);
        assert_eq!(size, 2_140_013);
        assert!(url.contains("wanderkid/unimernet"), "{url}");
        assert!(
            super::super::pp_formulanet::inventory()
                .artifacts
                .iter()
                .any(|artifact| artifact.sha256 == hash),
            "the two readers decode with two vocabularies"
        );
    }

    /// The recipe names the export, the frame and the decoder graph, and is
    /// not the recipe either other formula reader stores under. Two readers
    /// sharing one identity would let a library half-read by each report as
    /// wholly read by either.
    #[test]
    fn the_recipe_is_this_readers_own() {
        let id = identity();
        assert!(id.contains(MODEL_ID), "{id}");
        assert!(id.contains(EXPORT_REVISION), "{id}");
        assert!(id.contains(DECODER_WITH_PAST_GRAPH), "{id}");
        assert!(id.contains("192x672"), "{id}");
        assert_ne!(id, super::super::texify::identity());
        assert_ne!(id, super::super::pp_formulanet::identity());
    }

    /// The frame is filled whatever shape the crop was, and it is filled three
    /// times over — the encoder declares three channels for a transform that
    /// ends at one plane.
    #[test]
    fn a_crop_fills_three_identical_planes_of_the_frame() {
        let plane = (HEIGHT * WIDTH) as usize;
        for (width, height) in [(1600u32, 120u32), (40, 40), (3, 900), (1, 1)] {
            let crop = RgbImage::from_pixel(width, height, image::Rgb([160, 160, 160]));
            let pixels = preprocess(&crop).unwrap();
            assert_eq!(pixels.len(), 3 * plane);
            assert_eq!(&pixels[..plane], &pixels[plane..2 * plane]);
            assert_eq!(&pixels[plane..2 * plane], &pixels[2 * plane..]);
        }
        assert!(preprocess(&RgbImage::new(0, 4)).is_err());
    }

    /// The remainder around a fitted crop is black, and the ink is not.
    /// Upstream pads with zero and the model reads a white surround as a page
    /// it was not shown — the opposite of [`super::super::texify`], whose
    /// Donut frame is paper.
    #[test]
    fn the_frame_around_a_crop_is_black() {
        // Taller than the frame's aspect, so it fits on height and leaves
        // wide bands to the left and right — 40x400 scales to 19x192 and is
        // centred over the frame's middle column.
        let crop = RgbImage::from_pixel(40, 400, image::Rgb([255; 3]));
        let pixels = preprocess(&crop).unwrap();
        let black = (0.0 - 0.7931) / 0.1738;
        let white = (1.0 - 0.7931) / 0.1738;
        assert!(
            (pixels[0] - black).abs() < 1e-4,
            "the corner is not padding"
        );
        let centre = (HEIGHT / 2 * WIDTH + WIDTH / 2) as usize;
        assert!((pixels[centre] - white).abs() < 1e-4, "the crop is not ink");
    }

    /// Margins are cropped away before the fit, so the same expression on more
    /// paper is the same tensor. This is what the threshold at 200 is for.
    #[test]
    fn margins_do_not_change_the_fitted_expression() {
        let mut ink = RgbImage::from_pixel(12, 6, image::Rgb([255; 3]));
        for x in 0..12 {
            ink.put_pixel(x, 0, image::Rgb([0; 3]));
            ink.put_pixel(x, 5, image::Rgb([0; 3]));
        }
        let mut margins = RgbImage::from_pixel(40, 30, image::Rgb([255; 3]));
        imageops::overlay(&mut margins, &ink, 7, 9);
        assert_eq!(preprocess(&ink).unwrap(), preprocess(&margins).unwrap());
    }

    /// One recipe, two frames. The transform this reader runs at 192x672 is
    /// the one PP-FormulaNet runs at 384x384, and the square case is where
    /// the two would part company if the fit were rewritten: a scale taken
    /// from the longer edge and a scale taken from both edges agree only
    /// while the frame is square.
    #[test]
    fn the_square_frame_is_the_recipe_pp_formulanet_reads_under() {
        let side = super::super::pp_formulanet::SIDE;
        for (width, height) in [(1600u32, 120u32), (40, 40), (3, 900)] {
            let mut crop = RgbImage::from_pixel(width, height, image::Rgb([255; 3]));
            crop.put_pixel(width / 2, height / 2, image::Rgb([0; 3]));
            assert_eq!(
                fit_and_gray(&crop, side, side).unwrap(),
                super::super::pp_formulanet::preprocess(&crop).unwrap(),
                "the two readers no longer preprocess alike"
            );
        }
    }
}
