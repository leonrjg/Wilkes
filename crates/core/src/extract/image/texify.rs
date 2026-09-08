//! Texify under ONNX Runtime: the recognizer for a formula and nothing else.
//!
//! A page parser reads a page. Handed a crop of one expression it comes apart —
//! measured on this repository's corpus, ten inline crops through
//! granite-docling produced no admissible region at all, four of them decoding
//! to their token cap and looping. That is not a threshold that can be tuned:
//! the model was trained on pages and a fragment of one is a different input.
//!
//! This is the other half of that split. Texify is a Donut vision encoder over
//! an mBART decoder, trained on exactly this input — a cropped expression, out
//! comes its LaTeX — and it is the same class of model as UniMERNet, which
//! publishes only PaddlePaddle weights and so cannot run on the runtime
//! already in this tree.
//!
//! ## What it buys that geometry could not
//!
//! Not merely tidier output. On page 21 of one corpus document the page draws
//! `A ⋁ B ⇔ B ⋁ A`, and the font's encoding names *nothing* for the `⇔`:
//! MuPDF reports no character there and PDF.js maps it to 14.4 points of
//! whitespace. The reading held `A⋁B` and `B⋁A` as two fragments with the
//! operator between them simply gone, and no amount of reading the page's own
//! geometry recovers it, because there is no glyph record to read. Texify
//! returns `A\lor B\leftrightarrow B\lor A`.
//!
//! ## Why a pinned snapshot of an archived model
//!
//! Texify's own repository was archived in January 2025 and its work continued
//! in Surya. That is a reason to pin, not a reason to move:
//!
//! - **Surya does not run here.** It publishes PyTorch weights and a GGUF
//!   build; there is no first-party ONNX, and the community exports have no
//!   downloads and no provenance worth pinning. Adopting it would mean a
//!   second inference runtime beside `ort`, which is the argument this project
//!   already declined for whole-pipeline alternatives.
//! - **The terms moved the wrong way.** `vikp/texify` is CC-BY-SA-4.0; its
//!   successor `datalab-to/texify` is CC-BY-**NC**-SA-4.0 and
//!   `datalab-to/surya-ocr-2` is OpenRAIL. This pin is the last snapshot of
//!   the family that is not restricted to non-commercial use.
//!
//! Nothing is being tracked upstream in any case: the revision is a commit,
//! the artifacts are checked against their digests, and a reading records the
//! recipe that produced it. An archived model that is pinned and verified is
//! not a maintenance burden — it is a fixed input.
//!
//! ## Why the quantized set
//!
//! The opposite of [`super::granite_docling`]'s finding, and for a reason.
//! That model's int8 export loops and drops words; this one's does not, and
//! the difference is the decode length. Granite reads a whole page — hundreds
//! of tokens where a single early error compounds — while a formula is twenty
//! tokens and the decode ends before drift accumulates. Measured on this
//! repository's corpus at int8: `\frac{a+b}{2}<\sqrt{a b}`, `\sqrt{n^{2}}=n`,
//! and a four-step derivation with `\Rightarrow` and `\left(a-b\right)^{2}<0`,
//! all correct. So this ships at 543 MB rather than the 2.14 GB of the fp32
//! set, and the size argument is made by measurement rather than by default.
//!
//! ## Why two decoder graphs
//!
//! This module used to say that the cache was "the optimization for the rare
//! long derivation, and not worth the plumbing until one is measured to cost".
//! One was measured to cost, and it was not the rare long derivation — it was
//! every crop.
//!
//! The uncached graph takes the whole prefix and no cache, so a decode of `n`
//! tokens runs the decoder `n` times over a prefix that grows each time. Worse
//! for an encoder-decoder than for a decoder-only model: every one of those
//! runs also re-projects the encoder's 196 positions into eight layers of
//! cross-attention keys and values, which do not depend on the prefix at all
//! and are the same tensors every step. Measured over the 42 formula crops the
//! `perf_profile` probe cuts from this repository's `formula_recall` fixture,
//! at four intra-op threads on a 10-core M4:
//!
//! | decoder  | ms/crop | preprocess | encoder | decoder | ms/step |
//! |----------|---------|------------|---------|---------|---------|
//! | uncached | 1292    | 1.5        | 148.9   | 1141.8  | 31.39   |
//! | cached   | 299     | 1.5        | 138.3   | 158.7   | 4.35    |
//!
//! And end to end, over the fixture's five most heavily labelled pages — 24.8
//! formula crops a page — at the same four threads, in ms a page:
//!
//! | decoder  | whole page | texify decoder | texify encoder | decoder's share |
//! |----------|------------|----------------|----------------|-----------------|
//! | uncached | 19027      | 14420          | 3418           | 75.8%           |
//! | cached   | 7455       | 3000           | 3268           | 40.2%           |
//!
//! A prose page holds no formula and is 1.11 s either way, unchanged. What the
//! table does *not* say is that the page is now fast enough: 7.5 s is not the
//! 2 s a math-heavy page is wanted in. It says the decode has stopped being
//! the reason — the encoder is, at 43.8% of the page, one 420x420 Donut pass
//! per crop and 24.8 crops to pay it for. That is the next measurement, and it
//! is a different one.
//!
//! So both graphs ship and one decode runs across them: step 0 through the
//! graph that takes no past — one token in, the whole `present.*` cache out —
//! and every step after through the graph that takes it. The self-attention
//! cache grows by a position a step and is fed straight back; the
//! cross-attention cache is computed once at step 0 and handed back unchanged
//! for the rest of the decode, which is where most of the saving is.
//!
//! Two things it is not. It is not a fallback: [`CacheShape::discover`] reads
//! both graphs at load and refuses an export the two do not agree about, and
//! there is no uncached loop left to fall back to. And it is not free of
//! consequence — the two graphs are dynamically quantized over different
//! tensors and do not decode the same LaTeX, which is why [`identity`] names
//! the cached graph and a library read under the old recipe is re-read.
//!
//! What the second graph costs is 222 MB on disk and 94 MiB of peak resident
//! set — 1829 MiB against 1735 MiB over the same five pages — which is the
//! whole of it: the cross-attention cache it holds is 8 layers x 16 heads x
//! 196 positions x 64 wide, twice, and is dropped with the crop.
//!
//! ## Where the decode loop went
//!
//! Everything above is measured on a loop that is no longer in this file. When
//! [`super::unimernet`] arrived it was the same three-graph export of the same
//! Donut-encoder / mBART-decoder architecture, and a second copy of this loop
//! would have been a second place for the cross-attention cache to be
//! recomputed by accident — with these numbers still printed beside it,
//! describing neither. So the loop, the cache discovery and the reader pool
//! moved to [`super::donut_formula`] and are driven by a [`Checkpoint`]; what
//! stayed here is what is Texify's own — the pins, the frame, the statistics,
//! and the measurements above, which were taken through that loop and are
//! still taken through it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbImage;

use super::donut_formula::{Checkpoint, DonutFormula};

// ── The pinned recipe ────────────────────────────────────────────────────────

pub const MODEL_ID: &str = "texify";
const REPO: &str = "Xenova/texify";
/// A commit and not a branch. The tokenizer's vocabulary is the contract
/// between the decode and the LaTeX it spells; a re-export under a moved
/// branch would change what every stored reading means.
const REVISION: &str = "98b3e3d88921ae91525d116d8d79a8402e5b5e4e";

pub const ENCODER_GRAPH: &str = "onnx/encoder_model_quantized.onnx";
/// Step 0's decoder: no cache in, the whole `present.*` cache out.
pub const DECODER_GRAPH: &str = "onnx/decoder_model_quantized.onnx";
/// Every later step's decoder: one token in, the cache in and back out.
///
/// Required, not optional — see "Why two decoder graphs" above for what it
/// was measured to be worth, and [`identity`] for why it changes the recipe.
pub const DECODER_WITH_PAST_GRAPH: &str = "onnx/decoder_with_past_model_quantized.onnx";
/// The vocabulary. Byte-identical to the one [`super::unimernet`] and
/// [`super::pp_formulanet`] read with — one BPE serving the whole family — and
/// deliberately still downloaded per recognizer: this copy is disclosed under
/// `vikp/texify`'s CC-BY-SA-4.0, and sharing it would put those terms on the
/// Apache-2.0 rows beside it to save two megabytes.
pub const TOKENIZER: &str = "tokenizer.json";

pub const ARTIFACTS: &[&str] = &[
    ENCODER_GRAPH,
    DECODER_GRAPH,
    DECODER_WITH_PAST_GRAPH,
    TOKENIZER,
];

/// Size and SHA-256 of each artifact at [`REVISION`], in the same order.
const DIGESTS: &[(u64, &str)] = &[
    (
        79_294_829,
        "302452c132a82b1c70389f0f646952586afa20c3f9a16b517e40600c49eb8f23",
    ),
    (
        239_413_094,
        "8ed0845be59ad059bcd8f3b7a053c7161d563fd9a4ac7e6edad2b237930c181a",
    ),
    (
        222_476_851,
        "6d19016f081fa156c1f4c961d2d6e860c909543930dad657c76e80eb1acb1881",
    ),
    (
        2_140_013,
        "02c318d9cfa95bf323371762b8f838a82709530274d36dba6eca880f0add6cc4",
    ),
];

/// Donut's square, and the ImageNet statistics it normalizes with — from the
/// model's own `preprocessor_config.json`.
pub const SIDE: usize = 420;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// From the model's `generation_config.json`.
pub const START_TOKEN: i64 = 0;
pub const EOS_TOKEN: i64 = 2;

/// A decode that has not stopped by here has stopped saying anything.
///
/// The longest good answer measured on this repository's corpus was 247 tokens
/// — a four-step derivation set as one display block. This is past that and
/// far short of a number that would never trip.
const MAX_NEW_TOKENS: usize = 512;

/// Formulas are admitted on whether their LaTeX parses, never on a score — see
/// [`super::ocr::admit`]. This is declared because the trait asks every engine
/// for one, and it is the value the engine's own regions would be held to if
/// this model ever returned prose. It never does: it emits one region, of one
/// kind, and that kind is a formula.
pub const ADMISSION_THRESHOLD: f32 = 0.0;

/// What the recipe records about a reading this recognizer produced.
///
/// It names the decoder graph, and it did not have to before there were two of
/// them. Both are dynamically quantized — an int8 matmul takes its activation
/// scale from the range of the tensor it runs over, and the cached graph
/// multiplies one row where the uncached one multiplied the whole prefix — so
/// the two do not decode the same LaTeX. Measured over the 124 formula crops of
/// this repository's `formula_recall` fixture: 14 readings of 124 differ, most
/// of them a re-bracketing (`\\operatorname{Var}[X]` against
/// `\\operatorname{Var}\\!\\left[X\\right]`) and one of them a decode that runs to
/// its cap where the other stopped. That is a different reading, so it is a
/// different recipe, and a library read under the old one is re-read rather
/// than left half of each.
pub fn identity() -> String {
    format!(
        "ort-2.0.0-rc.13+{MODEL_ID}+{REPO}@{REVISION}+donut-{SIDE}+cap-{MAX_NEW_TOKENS}\
         +{DECODER_WITH_PAST_GRAPH}"
    )
}

pub fn footprint_bytes() -> u64 {
    DIGESTS.iter().map(|(size, _)| size).sum()
}

pub fn install_dir(model_dir: &Path) -> PathBuf {
    model_dir.join("recognizers").join(MODEL_ID)
}

pub fn is_installed(model_dir: &Path) -> bool {
    let dir = install_dir(model_dir);
    ARTIFACTS.iter().all(|name| dir.join(name).is_file())
}

pub fn inventory() -> crate::types::RecognizerInventory {
    crate::types::RecognizerInventory {
        name: MODEL_ID.to_string(),
        repo: REPO.to_string(),
        revision: REVISION.to_string(),
        license: "CC-BY-SA-4.0".to_string(),
        license_url: "https://huggingface.co/vikp/texify".to_string(),
        derived_from: vec![
            // The export itself declares no licence; its card names
            // `vikp/texify` as the base model, and that is where the terms
            // above come from. Said here rather than inferred silently,
            // because this is disclosed beside a download button.
            "ONNX export by Xenova, which declares no licence of its own".to_string(),
            "vikp/texify (CC-BY-SA-4.0, Vik Paruchuri) — the base model it names".to_string(),
            "Donut Swin vision encoder (MIT, NAVER)".to_string(),
            "mBART decoder (MIT, Facebook)".to_string(),
        ],
        artifacts: ARTIFACTS
            .iter()
            .zip(DIGESTS)
            .map(
                |(filename, (size_bytes, sha256))| crate::types::InventoriedArtifact {
                    filename: (*filename).to_string(),
                    size_bytes: *size_bytes,
                    sha256: (*sha256).to_string(),
                },
            )
            .collect(),
        footprint_bytes: footprint_bytes(),
    }
}

/// Fetch the recognizer's artifacts into `model_dir`, checking each against the
/// size and digest declared above.
pub fn install(
    model_dir: &Path,
    progress: Option<crate::models::progress::ProgressTx>,
) -> Result<()> {
    use hf_hub::api::sync::ApiBuilder;

    let dir = install_dir(model_dir);
    std::fs::create_dir_all(dir.join("onnx"))
        .context("could not create the recognizer directory")?;

    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .context("could not reach the model hub")?
        .repo(hf_hub::Repo::with_revision(
            REPO.to_string(),
            hf_hub::RepoType::Model,
            REVISION.to_string(),
        ));

    let reporter = progress.map(crate::models::hf_hub::HfProgressReporter::new);
    for (filename, (size_bytes, sha256)) in ARTIFACTS.iter().zip(DIGESTS) {
        let target = dir.join(filename);
        if target.is_file() && super::verify_artifact(&target, *size_bytes, sha256).is_ok() {
            continue;
        }
        let fetched = match reporter.clone() {
            Some(reporter) => api.download_with_progress(filename, reporter),
            None => api.download(filename),
        }
        .with_context(|| format!("could not download {filename} from {REPO}"))?;
        std::fs::copy(&fetched, &target)
            .with_context(|| format!("could not place {filename} under {}", dir.display()))?;
        // A file that does not match is removed rather than left where
        // `is_installed` would count it.
        if let Err(error) = super::verify_artifact(&target, *size_bytes, sha256) {
            let _ = std::fs::remove_file(&target);
            return Err(error);
        }
    }
    Ok(())
}

// ── Preprocessing ────────────────────────────────────────────────────────────

/// Fit the crop into Donut's square and normalize it.
///
/// Scaled to *fill* the frame rather than merely to fit inside it. The model
/// was trained on expressions that occupy their image, and a small crop left at
/// its own size in a field of paper is read as something else entirely — the
/// first run of this against inline crops returned `\sqrt{n}` for everything
/// until the upscale was put back. The aspect is kept and the remainder is
/// paper, because a squashed expression is not the same expression.
/// `pub` for the same reason [`crate::extract::pdf::typeset::render`] is: a
/// probe that measured the encoder against its own resize would be measuring a
/// model nobody runs. Pure, and holds no state.
pub fn preprocess(crop: &RgbImage) -> Vec<f32> {
    let (width, height) = crop.dimensions();
    let scale = (SIDE as f32 / width.max(1) as f32).min(SIDE as f32 / height.max(1) as f32);
    let fitted = image::imageops::resize(
        crop,
        ((width as f32 * scale).round() as u32).clamp(1, SIDE as u32),
        ((height as f32 * scale).round() as u32).clamp(1, SIDE as u32),
        FilterType::Lanczos3,
    );
    let mut canvas = RgbImage::from_pixel(SIDE as u32, SIDE as u32, image::Rgb([255; 3]));
    image::imageops::replace(
        &mut canvas,
        &fitted,
        ((SIDE as u32 - fitted.width()) / 2) as i64,
        ((SIDE as u32 - fitted.height()) / 2) as i64,
    );

    let mut chw = vec![0f32; 3 * SIDE * SIDE];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let pixel = canvas.get_pixel(x as u32, y as u32);
            for channel in 0..3 {
                chw[channel * SIDE * SIDE + y * SIDE + x] =
                    ((f32::from(pixel[channel]) / 255.0) - MEAN[channel]) / STD[channel];
            }
        }
    }
    chw
}

// ── The checkpoint ───────────────────────────────────────────────────────────

/// Everything [`DonutFormula`] has to be told to read with this export.
///
/// The decode loop, the cache discovery and the reader pool are shared with
/// [`super::unimernet`], which is the same architecture behind a different
/// checkpoint; what is Texify's own is above this line, and this table is the
/// join. See [`super::donut_formula`] for why the loop is written once.
pub static CHECKPOINT: Checkpoint = Checkpoint {
    model_id: MODEL_ID,
    encoder_graph: ENCODER_GRAPH,
    decoder_graph: DECODER_GRAPH,
    decoder_with_past_graph: DECODER_WITH_PAST_GRAPH,
    tokenizer: TOKENIZER,
    pixels: (3, SIDE, SIDE),
    // Infallible: the fit is defined for any crop with a pixel in it, and an
    // empty image never reaches a recognizer — `spot_batch` is handed decoded
    // crops. Wrapped rather than made fallible so the signature is the one
    // the runner takes for either checkpoint.
    preprocess: |crop| Ok(preprocess(crop)),
    identity,
    admission_threshold: ADMISSION_THRESHOLD,
    start_token: START_TOKEN,
    eos_token: EOS_TOKEN,
    max_new_tokens: MAX_NEW_TOKENS,
};

/// Load this recognizer in the calling process.
///
/// Must only be called from a worker subprocess — see [`super::dispatch`]'s
/// invariant. The artifacts are checked for presence rather than digest here,
/// as they were before the runner was shared: `install` verifies every file
/// against its digest before it is published into this directory, and
/// `is_installed` is what the catalogue answers with.
pub fn load(model_dir: &Path, readers: usize, threads: usize) -> Result<DonutFormula> {
    let dir = install_dir(model_dir);
    anyhow::ensure!(
        is_installed(model_dir),
        "the {MODEL_ID} recognizer is not installed under {}",
        dir.display()
    );
    DonutFormula::load(&CHECKPOINT, &dir, readers, threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inventory_names_every_artifact_with_a_digest() {
        let inventory = inventory();
        assert_eq!(inventory.artifacts.len(), ARTIFACTS.len());
        assert!(inventory
            .artifacts
            .iter()
            .all(|artifact| artifact.size_bytes > 0 && artifact.sha256.len() == 64));
        assert_eq!(inventory.footprint_bytes, footprint_bytes());
    }

    /// The revision is a commit, not a branch. The tokenizer's vocabulary is
    /// what the stored LaTeX means; a moved branch would change that silently.
    #[test]
    fn the_revision_is_pinned_to_a_commit() {
        assert_eq!(REVISION.len(), 40);
        assert!(REVISION.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(identity().contains(REVISION));
    }

    /// The cached decoder does not decode the same LaTeX as the uncached one —
    /// 14 readings of 124 differ on this repository's fixture — so a reading
    /// stored under the old recipe must not be counted as this one's.
    #[test]
    fn the_recipe_names_the_decoder_graph_that_produced_the_reading() {
        assert!(
            identity().contains(DECODER_WITH_PAST_GRAPH),
            "{}",
            identity()
        );
    }

    #[test]
    fn a_crop_becomes_donuts_square_whatever_shape_it_was() {
        for (width, height) in [(1600u32, 120u32), (40, 40), (3, 900)] {
            let crop = RgbImage::from_pixel(width, height, image::Rgb([200, 200, 200]));
            assert_eq!(preprocess(&crop).len(), 3 * SIDE * SIDE);
        }
    }

    /// The remainder around a fitted crop is paper. A model trained on
    /// expressions on white reads a black surround as ink.
    #[test]
    fn the_frame_around_a_crop_is_paper() {
        // A wide crop leaves bands above and below.
        let crop = RgbImage::from_pixel(SIDE as u32, 10, image::Rgb([0, 0, 0]));
        let planes = preprocess(&crop);
        let white = (1.0 - MEAN[0]) / STD[0];
        assert!((planes[0] - white).abs() < 1e-4, "top-left is paper");
    }

    /// The cached decoder is an artifact like any other: named, sized,
    /// digested, and required. `is_installed` counts it, so an installation
    /// that predates it reinstalls rather than decoding through a graph that
    /// is not there.
    #[test]
    fn the_cached_decoder_is_a_required_artifact() {
        assert!(ARTIFACTS.contains(&DECODER_WITH_PAST_GRAPH));
        assert_eq!(ARTIFACTS.len(), DIGESTS.len());
        let (size, sha256) = DIGESTS[ARTIFACTS
            .iter()
            .position(|name| *name == DECODER_WITH_PAST_GRAPH)
            .expect("just asserted")];
        assert_eq!(size, 222_476_851);
        assert_eq!(sha256.len(), 64);
        assert_eq!(
            footprint_bytes(),
            DIGESTS.iter().map(|(n, _)| n).sum::<u64>()
        );
        assert!(
            footprint_bytes() > size,
            "the footprint counts the cached decoder beside the rest"
        );
    }

    /// Three of the four files is not an installation. Without this the
    /// recognizer would be offered as cached and fail on the first crop.
    #[test]
    fn an_installation_without_the_cached_decoder_is_not_one() {
        let root = tempfile::tempdir().unwrap();
        let dir = install_dir(root.path());
        std::fs::create_dir_all(dir.join("onnx")).unwrap();
        for name in ARTIFACTS {
            if *name == DECODER_WITH_PAST_GRAPH {
                continue;
            }
            std::fs::write(dir.join(name), b"not really a graph").unwrap();
        }
        assert!(!is_installed(root.path()));
        std::fs::write(dir.join(DECODER_WITH_PAST_GRAPH), b"nor is this").unwrap();
        assert!(is_installed(root.path()));
    }
}
