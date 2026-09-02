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
//! all correct. So this ships at 321 MB rather than the 1.25 GB of the fp32
//! set, and the size argument is made by measurement rather than by default.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;
use tracing::{debug, warn};

use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use crate::types::Point;

// ── The pinned recipe ────────────────────────────────────────────────────────

pub const MODEL_ID: &str = "texify";
const REPO: &str = "Xenova/texify";
/// A commit and not a branch. The tokenizer's vocabulary is the contract
/// between the decode and the LaTeX it spells; a re-export under a moved
/// branch would change what every stored reading means.
const REVISION: &str = "98b3e3d88921ae91525d116d8d79a8402e5b5e4e";

const ENCODER_GRAPH: &str = "onnx/encoder_model_quantized.onnx";
const DECODER_GRAPH: &str = "onnx/decoder_model_quantized.onnx";

pub const ARTIFACTS: &[&str] = &[ENCODER_GRAPH, DECODER_GRAPH, "tokenizer.json"];

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
        2_140_013,
        "02c318d9cfa95bf323371762b8f838a82709530274d36dba6eca880f0add6cc4",
    ),
];

/// Donut's square, and the ImageNet statistics it normalizes with — from the
/// model's own `preprocessor_config.json`.
const SIDE: usize = 420;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// From the model's `generation_config.json`.
const START_TOKEN: i64 = 0;
const EOS_TOKEN: i64 = 2;

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
pub fn identity() -> String {
    format!("ort-2.0.0-rc.13+{MODEL_ID}+{REPO}@{REVISION}+donut-{SIDE}+cap-{MAX_NEW_TOKENS}")
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
pub(super) fn preprocess(crop: &RgbImage) -> Vec<f32> {
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

/// Strip the `$$…$$` the model wraps its answer in.
///
/// The delimiters are the model's notation for "this is display mathematics",
/// not part of the expression, and every consumer of a formula region in this
/// codebase — the LaTeX validity check, the reader's substitution, the
/// embedder — wants the expression.
pub(super) fn unwrap_delimiters(text: &str) -> &str {
    let text = text.trim();
    for fence in ["$$", "\\[", "$"] {
        let close = match fence {
            "\\[" => "\\]",
            other => other,
        };
        if let Some(inner) = text.strip_prefix(fence).and_then(|t| t.strip_suffix(close)) {
            return inner.trim();
        }
    }
    text
}

// ── The engine ───────────────────────────────────────────────────────────────

/// One loaded copy of the two graphs.
struct Reader {
    encoder: Session,
    decoder: Session,
}

pub struct Texify {
    /// Where the graphs are and what to load them with, kept so the sessions
    /// can be dropped and rebuilt: [`OcrEngine::release`] is a promise that a
    /// later `spot_batch` still works.
    dir: PathBuf,
    threads: usize,
    /// `None` until the first crop, and after a release. Loading is deferred
    /// so a runtime that attaches a recognizer and indexes nothing does not
    /// pay 321 MB for it.
    reader: Mutex<Option<Reader>>,
    tokenizer: Tokenizer,
}

impl Texify {
    pub fn load(model_dir: &Path, threads: usize) -> Result<Self> {
        let dir = install_dir(model_dir);
        anyhow::ensure!(
            is_installed(model_dir),
            "the {MODEL_ID} recognizer is not installed under {}",
            dir.display()
        );
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!("could not read the tokenizer: {e}"))?;
        Ok(Self {
            dir,
            threads,
            reader: Mutex::new(None),
            tokenizer,
        })
    }

    fn session(&self, name: &str) -> Result<Session> {
        Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(self.threads)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(self.dir.join(name))
            .map_err(|e| anyhow::anyhow!("could not load {name}: {e}"))
    }

    /// Read one crop: encode once, then decode greedily.
    ///
    /// No key/value cache. The decoder is re-run over the whole prefix at each
    /// step, which is quadratic in the answer's length and irrelevant at the
    /// length of a formula — measured, twenty tokens in 0.3 seconds. The
    /// merged graph that carries a cache is the optimization for the rare long
    /// derivation, and it is not worth the `use_cache_branch` plumbing until
    /// one is measured to cost.
    fn read(&self, reader: &mut Reader, crop: &RgbImage) -> Result<(String, f32)> {
        let pixels =
            Tensor::from_array((vec![1i64, 3, SIDE as i64, SIDE as i64], preprocess(crop)))?;
        let encoded = reader
            .encoder
            .run(vec![(
                "pixel_values".to_string(),
                SessionInputValue::from(pixels),
            )])
            .map_err(|e| anyhow::anyhow!("the vision encoder failed: {e}"))?;
        let (shape, hidden) = encoded[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("the vision encoder returned nothing usable: {e}"))?;
        let shape: Vec<i64> = shape.to_vec();
        let hidden: Vec<f32> = hidden.to_vec();
        drop(encoded);

        let mut ids: Vec<i64> = vec![START_TOKEN];
        // The decoder's own certainty about each token it chose, averaged.
        // Uncalibrated, like every other admission signal in this module: it
        // says how sure the decode was of its next token, not how often such a
        // decode is right.
        let mut confidence = 0.0f64;
        let mut hit_the_cap = true;
        for _ in 0..MAX_NEW_TOKENS {
            let input = Tensor::from_array((vec![1i64, ids.len() as i64], ids.clone()))?;
            let states = Tensor::from_array((shape.clone(), hidden.clone()))?;
            let out = reader
                .decoder
                .run(vec![
                    ("input_ids".to_string(), SessionInputValue::from(input)),
                    (
                        "encoder_hidden_states".to_string(),
                        SessionInputValue::from(states),
                    ),
                ])
                .map_err(|e| anyhow::anyhow!("the decoder failed: {e}"))?;
            let (logit_shape, logits) = out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("the decoder returned nothing usable: {e}"))?;
            let vocabulary = *logit_shape.last().unwrap_or(&0) as usize;
            anyhow::ensure!(vocabulary > 0, "the decoder returned an empty vocabulary");
            let step = &logits[(ids.len() - 1) * vocabulary..ids.len() * vocabulary];
            let (next, probability) = argmax_with_probability(step);
            confidence += f64::from(probability);
            if next == EOS_TOKEN {
                hit_the_cap = false;
                break;
            }
            ids.push(next);
        }
        if hit_the_cap {
            warn!("the {MODEL_ID} decode hit its {MAX_NEW_TOKENS}-token cap; this crop is partial");
        }

        let steps = ids.len().max(1) as f64;
        let text = self
            .tokenizer
            .decode(
                &ids[1..].iter().map(|id| *id as u32).collect::<Vec<u32>>(),
                true,
            )
            .map_err(|e| anyhow::anyhow!("could not detokenize the answer: {e}"))?;
        Ok((
            unwrap_delimiters(&text).to_string(),
            (confidence / steps) as f32,
        ))
    }
}

/// The most likely token and how much of the distribution it held.
fn argmax_with_probability(logits: &[f32]) -> (i64, f32) {
    let mut best = 0usize;
    for (index, value) in logits.iter().enumerate() {
        if value > &logits[best] {
            best = index;
        }
    }
    let peak = logits[best];
    let total: f32 = logits.iter().map(|value| (value - peak).exp()).sum();
    (best as i64, if total > 0.0 { 1.0 / total } else { 0.0 })
}

impl OcrEngine for Texify {
    fn identity(&self) -> String {
        identity()
    }

    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }

    /// One region per crop, covering the whole of it.
    ///
    /// This model does not delimit: it is given an expression and returns that
    /// expression's LaTeX, so the region *is* the image and inventing a
    /// tighter polygon would be a claim about where the ink is that nothing
    /// here measured.
    fn spot_batch(&self, images: &[RgbImage]) -> Result<Vec<ImageRecognition>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let mut held = self
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.is_none() {
            *held = Some(Reader {
                encoder: self.session(ENCODER_GRAPH)?,
                decoder: self.session(DECODER_GRAPH)?,
            });
        }
        let reader = held.as_mut().expect("just loaded");

        let mut out = Vec::with_capacity(images.len());
        for (index, image) in images.iter().enumerate() {
            let (text, confidence) = self.read(reader, image)?;
            debug!(
                "read formula {} of {} with {MODEL_ID}: {text:?}",
                index + 1,
                images.len()
            );
            out.push(if text.trim().is_empty() {
                // Nothing to transcribe is a real answer and not a failure —
                // the counter for it belongs to the caller, which is why this
                // is an empty region list rather than an error.
                ImageRecognition {
                    regions: Vec::new(),
                    unroutable: 0,
                    not_text: 1,
                }
            } else {
                ImageRecognition::from_regions(vec![SpottedRegion {
                    kind: RegionKind::Formula,
                    text,
                    confidence,
                    quad: [
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 1.0, y: 0.0 },
                        Point { x: 1.0, y: 1.0 },
                        Point { x: 0.0, y: 1.0 },
                    ],
                }])
            });
        }
        Ok(out)
    }

    fn release(&self) {
        let dropped = self
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some();
        if dropped {
            debug!("{MODEL_ID} released; the next crop reloads it");
        }
    }
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

    #[test]
    fn the_delimiters_are_not_part_of_the_expression() {
        assert_eq!(unwrap_delimiters("$$a+b$$"), "a+b");
        assert_eq!(unwrap_delimiters("  $$ a+b $$ "), "a+b");
        assert_eq!(unwrap_delimiters("\\[a+b\\]"), "a+b");
        assert_eq!(unwrap_delimiters("$x$"), "x");
        assert_eq!(unwrap_delimiters("a+b"), "a+b");
    }

    /// `$` inside an expression is not a fence around it.
    #[test]
    fn an_unpaired_delimiter_is_left_alone() {
        assert_eq!(unwrap_delimiters("$$a+b"), "$$a+b");
        assert_eq!(unwrap_delimiters("a\\$b"), "a\\$b");
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

    #[test]
    fn the_most_likely_token_carries_the_share_of_the_distribution_it_held() {
        let (token, probability) = argmax_with_probability(&[0.0, 10.0, 0.0]);
        assert_eq!(token, 1);
        assert!(probability > 0.99, "{probability}");
        let (_, flat) = argmax_with_probability(&[1.0, 1.0, 1.0, 1.0]);
        assert!((flat - 0.25).abs() < 1e-5, "{flat}");
    }
}
