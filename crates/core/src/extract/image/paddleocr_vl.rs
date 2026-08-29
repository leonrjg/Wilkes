//! The production recognizer: PaddleOCR-VL's text-spotting task.
//!
//! Wilkes owns everything outside the weights — the task prompt, the image
//! preprocessing, the decode loop, and the parsing of `<|LOC_n|>` tokens into
//! quadrilaterals. `candle-transformers` owns the model, which is why no new
//! inference dependency, toolchain change or `ort` bump was needed to get here.
//!
//! The decode loop is Wilkes' own rather than the module's `generate`, and for
//! a reason the admission rule depends on: `generate` returns the chosen
//! tokens and discards the logits, so the log-probabilities the admission
//! signal is derived from would not exist. Running `forward` per step keeps
//! them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::paddleocr_vl::{Config, PaddleOCRVLModel};
use hf_hub::api::sync::ApiBuilder;
use sha2::{Digest, Sha256};
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

use crate::embed::engines::candle::{realize_device, select_device_plan};
use crate::models::hf_hub::HfProgressReporter;
use crate::models::progress::ProgressTx;

use super::ocr::{
    parse_spotting, OcrEngine, SpottedRegion, SpottingDecoder, SpottingToken, LOC_MAX,
};

// ── The pinned recipe ────────────────────────────────────────────────────────

/// One shippable recognizer: weights, tokenizer, and the revision they were
/// evaluated at together.
///
/// A checkpoint, not a runtime choice. Wilkes packages and identifies exactly
/// one, chosen by measurement; two checkpoints reachable at runtime would be
/// two answers to "what does this document say".
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint {
    pub name: &'static str,
    pub repo: &'static str,
    /// An immutable commit sha, never a branch. A branch re-resolves to
    /// whatever was last pushed, which is exactly what a pin exists to stop.
    pub revision: &'static str,
    pub weights: Artifact,
    pub tokenizer: Artifact,
    pub config: Artifact,
    /// The SPDX identifier the upstream repository publishes these artifacts
    /// under.
    pub license: &'static str,
    pub license_url: &'static str,
    /// What the weights are made of, upstream of this repository. A checkpoint
    /// is not one work: this one is an encoder and a decoder trained together,
    /// and an inventory that named only the repository it was fetched from
    /// would be an inventory of the download rather than of the model.
    pub derived_from: &'static [&'static str],
}

impl Checkpoint {
    /// Every file this checkpoint is, in the order they are installed.
    ///
    /// One list, so the download, the verification and the inventory cannot
    /// disagree about what the checkpoint consists of — a fourth artifact
    /// added here is downloaded, verified and inventoried by that fact alone.
    pub fn artifacts(&self) -> [&Artifact; 3] {
        [&self.weights, &self.tokenizer, &self.config]
    }

    /// What the checkpoint costs on disk: the pinned sizes, summed. Exact
    /// rather than observed, because the pins are what an install produces.
    pub fn footprint_bytes(&self) -> u64 {
        self.artifacts()
            .iter()
            .map(|artifact| artifact.size_bytes)
            .sum()
    }
}

/// One downloaded file and what it must be.
///
/// Size and digest both, and checked after download rather than trusted: a
/// truncated or substituted artifact that loads at all would change every
/// reading it touched, silently.
#[derive(Debug, Clone, Copy)]
pub struct Artifact {
    pub filename: &'static str,
    pub size_bytes: u64,
    pub sha256: &'static str,
}

/// PaddleOCR-VL 1.5, the first checkpoint with the spotting task.
pub const CHECKPOINT_1_5: Checkpoint = Checkpoint {
    name: "paddleocr-vl-1.5",
    repo: "PaddlePaddle/PaddleOCR-VL-1.5",
    revision: "2a4195faa5e7914c12f2fc601d72c81caf8d2da5",
    weights: Artifact {
        filename: "model.safetensors",
        size_bytes: 1_917_255_968,
        sha256: "d557c9d8997ae57ed3b1b33bdf347be878cc335687f32ca105341c16973f8958",
    },
    tokenizer: Artifact {
        filename: "tokenizer.json",
        size_bytes: 11_189_060,
        sha256: "c8a215a59183d0d0781adc33bacd3ce6162716f7fd568fb30234a74d69803a7d",
    },
    config: Artifact {
        filename: "config.json",
        size_bytes: 2_059,
        sha256: "ce7f4565f8b1db78532ad5d1b9ebe55c2139d49bd4cb04778b580a08a598f171",
    },
    license: "Apache-2.0",
    license_url: "https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.5",
    derived_from: PADDLEOCR_VL_COMPONENTS,
};

/// What every PaddleOCR-VL checkpoint is built from. Shared because 1.5 and
/// 1.6 are the same architecture: two post-trainings of one design, not two
/// models with two provenances.
const PADDLEOCR_VL_COMPONENTS: &[&str] = &[
    "NaViT-style dynamic-resolution vision encoder (Apache-2.0, PaddlePaddle)",
    "ERNIE-4.5-0.3B language decoder (Apache-2.0, Baidu)",
];

/// PaddleOCR-VL 1.6. Same architecture and the same tokenizer; different
/// weights, and therefore a different recipe.
pub const CHECKPOINT_1_6: Checkpoint = Checkpoint {
    name: "paddleocr-vl-1.6",
    repo: "PaddlePaddle/PaddleOCR-VL-1.6",
    revision: "c5630abae1d940eafe0697512a0325494b02ab42",
    weights: Artifact {
        filename: "model.safetensors",
        size_bytes: 1_917_255_968,
        sha256: "85a479d506a11e724e7285d395c551be69f41dbc16b6342d3cacfb189aed71db",
    },
    tokenizer: CHECKPOINT_1_5.tokenizer,
    config: CHECKPOINT_1_5.config,
    license: "Apache-2.0",
    license_url: "https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6",
    derived_from: PADDLEOCR_VL_COMPONENTS,
};

/// The two checkpoints the evaluation compares. It selects one; it does not
/// leave a choice behind.
pub const CHECKPOINTS: &[Checkpoint] = &[CHECKPOINT_1_5, CHECKPOINT_1_6];

/// The checkpoint Wilkes ships.
///
/// **Measured, 2026-08-28**, by [`evaluate`] over the eight-figure corpus in
/// [`crate::extract::image::corpus`]. The result is that the corpus does not
/// distinguish the two, and that is the finding — not a tie-break dressed up
/// as a decision.
///
/// On seven of eight figures they are indistinguishable: both transcribed the
/// clean, low-resolution, coloured, inverted and non-ASCII figures perfectly,
/// both emitted nothing on the figure with no text in it, and both read all
/// twelve drawn lines of the sample document's diagram exactly. Coordinate
/// accuracy is the same to a thousandth — 0.012 against 0.011 of the image,
/// 0.025 worst for each. On the eighth, turned labels, both fail badly and
/// their character error differs by 0.03, which is a difference between two
/// garbled strings and not something to stake a recipe on.
///
/// 1.6 is therefore shipped as the later post-training of the same weights
/// with nothing measured against it. The checkpoint choice is not
/// load-bearing, which is worth knowing: effort spent choosing between these
/// two buys nothing, and the weaknesses the corpus did find are shared.
///
/// The two are not distinguished on speed either. The wall-clock figures
/// differ between runs, but the runs were not made on an equally idle
/// machine, and the checkpoints are the same architecture at the same
/// parameter count, so there is no reason for them to differ and this
/// measurement is not evidence that they do.
pub const SHIPPED_CHECKPOINT: Checkpoint = CHECKPOINT_1_6;

/// The recipe string a recognizer of this checkpoint reads documents under.
///
/// A free function on the checkpoint rather than a method on a loaded model,
/// because it is derived entirely from constants and the process that needs it
/// is not always the process holding the weights. Recognition runs in a worker
/// subprocess; the extraction recipe is decided by the host. Deriving the
/// identity from the same checkpoint the worker is told to load is what keeps
/// those two from disagreeing without a round trip to ask.
pub fn identity_of(checkpoint: &Checkpoint) -> String {
    format!(
        "candle-transformers-0.11+{}+{}+{}+admit-{ADMISSION_THRESHOLD}",
        checkpoint.name, checkpoint.weights.sha256, EXTRACTION_SETTINGS_VERSION,
    )
}

// ── The license and provenance inventory ─────────────────────────────────────

/// One file of a checkpoint, as an inventory names it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InventoriedArtifact {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

/// What the recognizer is, where it came from, and under what terms.
///
/// FIGURE.md requires this of the redistributed checkpoint before it is
/// packaged, and it is data rather than prose for the reason the pins
/// themselves are: an inventory kept in a comment is one nobody can check.
/// Every file the install writes appears here with the digest it is verified
/// against, so the inventory describes the bytes on disk and not a
/// recollection of them.
///
/// Wilkes fetches these artifacts at the user's request rather than shipping
/// them inside the application, which is why the inventory is shown where the
/// download is offered: the terms are disclosed before the bytes arrive, not
/// after.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecognizerInventory {
    pub name: String,
    pub repo: String,
    pub revision: String,
    pub license: String,
    pub license_url: String,
    pub derived_from: Vec<String>,
    pub artifacts: Vec<InventoriedArtifact>,
    pub footprint_bytes: u64,
}

/// The inventory of one checkpoint.
pub fn inventory(checkpoint: &Checkpoint) -> RecognizerInventory {
    RecognizerInventory {
        name: checkpoint.name.to_string(),
        repo: checkpoint.repo.to_string(),
        revision: checkpoint.revision.to_string(),
        license: checkpoint.license.to_string(),
        license_url: checkpoint.license_url.to_string(),
        derived_from: checkpoint
            .derived_from
            .iter()
            .map(|component| (*component).to_string())
            .collect(),
        artifacts: checkpoint
            .artifacts()
            .iter()
            .map(|artifact| InventoriedArtifact {
                filename: artifact.filename.to_string(),
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256.to_string(),
            })
            .collect(),
        footprint_bytes: checkpoint.footprint_bytes(),
    }
}

// ── The pinned extraction settings ───────────────────────────────────────────

/// The task prompt. The model was post-trained on this exact string; it is not
/// a phrasing choice, and changing it changes the recipe.
const SPOTTING_PROMPT: &str = "Spotting:";

/// The chat framing the checkpoint's own template produces, with the image
/// placeholder where the vision tokens go.
const PROMPT_PREFIX: &str = "<|begin_of_sentence|>User: <|IMAGE_START|>";
const PROMPT_SUFFIX: &str = "<|IMAGE_END|>";

/// Patch size times the spatial merge: the recognizer's grid steps in 28-pixel
/// units, so both dimensions of a preprocessed image are multiples of it.
const RESIZE_FACTOR: u32 = 28;

/// The spotting task's own resolution envelope, from the checkpoint's model
/// card. Larger than the default recognition envelope because localization
/// needs the pixels.
const MIN_PIXELS: u64 = 112_896;
const MAX_PIXELS: u64 = 2048 * 28 * 28;

/// Below this on both sides the image is doubled before preprocessing. Small
/// crops are the spotting task's weak end and the model card upsamples them;
/// doing anything else here would be a different recipe from the one the
/// checkpoint was measured under.
///
/// The doubling is Lanczos and the fit to the envelope is bicubic, in that
/// order, as two resamples — which is what the card does and is not the same
/// pixels as one resample straight to the final size.
const UPSCALE_THRESHOLD: u32 = 1500;

/// Normalization: the checkpoint's preprocessor centres on 0.5 with a 0.5
/// spread, per channel.
const IMAGE_MEAN: f32 = 0.5;
const IMAGE_STD: f32 = 0.5;

/// The longest spotting response Wilkes will decode. A figure's labels are
/// tens of tokens; a decode still running at this point has stopped
/// transcribing and started repeating, and the partial result is the honest
/// outcome.
const MAX_NEW_TOKENS: usize = 1024;

/// The admission threshold: mean token probability of a region's text.
///
/// **Measured, 2026-08-28**, by [`evaluate`]'s sweep over 40 emitted regions
/// from the shipped checkpoint. It is the operating point where both errors
/// are zero: every correct region admitted, every incorrect one rejected.
///
/// ```text
/// threshold   correct in   wrong in   correct lost
///      0.60           38          1              0
///      0.70           38          0              0
///      0.80           37          0              1
///      0.90           34          0              4
/// ```
///
/// 0.60, where this sat before it was measured, admitted a garbled region.
/// Above 0.70 the rule starts throwing away transcriptions that were right.
///
/// Forty observations from one corpus is a small basis, and the wrong regions
/// it separates all came from one figure — turned labels, which the weights
/// read badly. It is a real operating point rather than a guess, and it is
/// not a calibration. It is part of the engine identity, so moving it
/// re-extracts rather than quietly re-reading old annotations under a new
/// rule.
pub const ADMISSION_THRESHOLD: f32 = 0.70;

/// Bumped when anything above changes for the same weights. `v2` corrected
/// the resample pipeline to the card's two stages.
const EXTRACTION_SETTINGS_VERSION: &str = "spotting-v2";

// ── Preprocessing ────────────────────────────────────────────────────────────

/// The dimensions the recognizer is given for an image of `width` x `height`.
///
/// Both are multiples of [`RESIZE_FACTOR`], the aspect ratio is held as close
/// as the grid allows, and the total lands inside the task's pixel envelope.
/// Coordinates come back normalized, so this rescaling never has to be undone:
/// a fraction of the resized image is the same fraction of the original.
pub fn spotting_dimensions(width: u32, height: u32) -> (u32, u32) {
    let (width, height) = match upscales(width, height) {
        true => (width.saturating_mul(2), height.saturating_mul(2)),
        false => (width, height),
    };
    smart_resize(width, height)
}

/// Whether the spotting task doubles this image before gridding it.
fn upscales(width: u32, height: u32) -> bool {
    width < UPSCALE_THRESHOLD && height < UPSCALE_THRESHOLD
}

fn smart_resize(width: u32, height: u32) -> (u32, u32) {
    let factor = RESIZE_FACTOR as f64;
    let (mut width, mut height) = (width.max(1) as f64, height.max(1) as f64);

    // A side thinner than one grid step cannot be gridded at all; it is scaled
    // up to one step and the other side follows, which is what the
    // checkpoint's own preprocessor does.
    if height < factor {
        width = (width * factor) / height;
        height = factor;
    }
    if width < factor {
        height = (height * factor) / width;
        width = factor;
    }

    let round_to_grid = |value: f64| ((value / factor).round() * factor).max(factor);
    let (mut resized_width, mut resized_height) = (round_to_grid(width), round_to_grid(height));

    let area = resized_width * resized_height;
    if area > MAX_PIXELS as f64 {
        let beta = ((width * height) / MAX_PIXELS as f64).sqrt();
        resized_width = ((width / beta / factor).floor() * factor).max(factor);
        resized_height = ((height / beta / factor).floor() * factor).max(factor);
    } else if area < MIN_PIXELS as f64 {
        let beta = (MIN_PIXELS as f64 / (width * height)).sqrt();
        resized_width = (width * beta / factor).ceil() * factor;
        resized_height = (height * beta / factor).ceil() * factor;
    }
    (resized_width as u32, resized_height as u32)
}

/// The image as the recognizer's vision encoder takes it: `(1, 3, h, w)`,
/// normalized, with `h` and `w` multiples of the grid step.
fn pixel_tensor(
    image: &image::RgbImage,
    device: &Device,
    dtype: DType,
) -> anyhow::Result<(Tensor, u32, u32)> {
    let (width, height) = spotting_dimensions(image.width(), image.height());
    // Two resamples where the recipe has two, not one straight to the final
    // size. A Lanczos double followed by a bicubic fit does not produce the
    // pixels a single bicubic would, and the pixels are what the model reads;
    // the spec's own rule is that a different filter is a different recipe.
    let doubled;
    let source = if upscales(image.width(), image.height()) {
        doubled = image::imageops::resize(
            image,
            image.width().saturating_mul(2),
            image.height().saturating_mul(2),
            image::imageops::FilterType::Lanczos3,
        );
        &doubled
    } else {
        image
    };
    let resized = image::imageops::resize(
        source,
        width,
        height,
        // Bicubic, as the checkpoint's image processor asks for (`resample: 3`).
        // Catmull-Rom is the cubic PIL's BICUBIC uses.
        image::imageops::FilterType::CatmullRom,
    );

    let count = (width as usize) * (height as usize);
    let mut planes = vec![0f32; 3 * count];
    for (index, pixel) in resized.pixels().enumerate() {
        for channel in 0..3 {
            let value = f32::from(pixel.0[channel]) / 255.0;
            planes[channel * count + index] = (value - IMAGE_MEAN) / IMAGE_STD;
        }
    }
    let tensor = Tensor::from_vec(planes, (1, 3, height as usize, width as usize), device)?
        .to_dtype(dtype)?;
    Ok((tensor, width, height))
}

// ── Artifacts ────────────────────────────────────────────────────────────────

fn cached_path(data_dir: &Path, checkpoint: &Checkpoint, artifact: &Artifact) -> Option<PathBuf> {
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::with_revision(
            checkpoint.repo.to_string(),
            hf_hub::RepoType::Model,
            checkpoint.revision.to_string(),
        ))
        .get(artifact.filename)
}

/// Whether the shipped recognizer is installed and intact on this machine.
pub fn is_installed(data_dir: &Path, checkpoint: &Checkpoint) -> bool {
    resolve(data_dir, checkpoint).is_ok()
}

struct ResolvedArtifacts {
    weights: PathBuf,
    tokenizer: PathBuf,
    config: PathBuf,
}

fn resolve(data_dir: &Path, checkpoint: &Checkpoint) -> anyhow::Result<ResolvedArtifacts> {
    let locate = |artifact: &Artifact| -> anyhow::Result<PathBuf> {
        let path = cached_path(data_dir, checkpoint, artifact).ok_or_else(|| {
            anyhow::anyhow!(
                "'{}' is not installed for '{}' at {}",
                artifact.filename,
                checkpoint.repo,
                checkpoint.revision
            )
        })?;
        let size = std::fs::metadata(&path)?.len();
        anyhow::ensure!(
            size == artifact.size_bytes,
            "'{}' is {size} bytes, {} expected",
            artifact.filename,
            artifact.size_bytes
        );
        Ok(path)
    };
    Ok(ResolvedArtifacts {
        weights: locate(&checkpoint.weights)?,
        tokenizer: locate(&checkpoint.tokenizer)?,
        config: locate(&checkpoint.config)?,
    })
}

/// The digest of a file on disk, streamed.
pub fn file_sha256(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Download and verify one checkpoint.
///
/// Explicit: nothing here runs on a timer or on first use, so offline
/// operation after installation is the ordinary case and an unversioned
/// runtime download can never change what a document extracts to.
pub fn install(
    data_dir: &Path,
    checkpoint: &Checkpoint,
    progress: Option<ProgressTx>,
) -> anyhow::Result<()> {
    let reporter = progress.map(HfProgressReporter::new);
    for artifact in checkpoint.artifacts() {
        let path = match cached_path(data_dir, checkpoint, artifact) {
            Some(path) => path,
            None => {
                let api = ApiBuilder::new()
                    .with_cache_dir(data_dir.to_path_buf())
                    .build()
                    .context("could not initialise the HF hub API")?;
                let repo = api.repo(hf_hub::Repo::with_revision(
                    checkpoint.repo.to_string(),
                    hf_hub::RepoType::Model,
                    checkpoint.revision.to_string(),
                ));
                match reporter.clone() {
                    Some(reporter) => repo.download_with_progress(artifact.filename, reporter),
                    None => repo.download(artifact.filename),
                }
                .map_err(|error| {
                    anyhow::anyhow!(
                        "could not download '{}' from '{}': {error:#}",
                        artifact.filename,
                        checkpoint.repo
                    )
                })?
            }
        };

        let size = std::fs::metadata(&path)?.len();
        let digest = file_sha256(&path)?;
        if size != artifact.size_bytes || digest != artifact.sha256 {
            // A wrong artifact that loads at all would change every reading
            // it touched, so it does not stay on disk to be found next time.
            let _ = std::fs::remove_file(&path);
            anyhow::bail!(
                "'{}' does not match the pinned artifact: {size} bytes / {digest}, \
                 {} bytes / {} expected",
                artifact.filename,
                artifact.size_bytes,
                artifact.sha256
            );
        }
        debug!("verified {} ({size} bytes)", artifact.filename);
    }
    info!(
        "installed recognizer {} from {} at {}",
        checkpoint.name, checkpoint.repo, checkpoint.revision
    );
    Ok(())
}

// ── The engine ───────────────────────────────────────────────────────────────

/// Which vocabulary ids are `<|LOC_n|>`, resolved from the tokenizer that
/// shipped with the weights rather than hardcoded.
///
/// The ids are contiguous in the checkpoints Wilkes pins, but reading them
/// from the tokenizer is what keeps the parser correct if a later checkpoint
/// renumbers its vocabulary — and what makes a checkpoint whose vocabulary has
/// no location tokens a load error instead of a run of nonsense coordinates.
struct LocationVocabulary {
    first: u32,
    last: u32,
}

impl LocationVocabulary {
    fn resolve(tokenizer: &Tokenizer) -> anyhow::Result<Self> {
        let id_of = |value: u16| {
            tokenizer
                .token_to_id(&format!("<|LOC_{value}|>"))
                .ok_or_else(|| anyhow::anyhow!("the tokenizer has no <|LOC_{value}|> token"))
        };
        let first = id_of(0)?;
        let last = id_of(LOC_MAX)?;
        anyhow::ensure!(
            last.checked_sub(first) == Some(u32::from(LOC_MAX)),
            "the tokenizer's location tokens are not contiguous: \
             <|LOC_0|> is {first} and <|LOC_{LOC_MAX}|> is {last}"
        );
        Ok(Self { first, last })
    }

    fn value(&self, id: u32) -> Option<u16> {
        (id >= self.first && id <= self.last).then(|| (id - self.first) as u16)
    }
}

struct TokenizerDecoder<'a>(&'a Tokenizer);

impl SpottingDecoder for TokenizerDecoder<'_> {
    fn decode(&self, ids: &[u32]) -> anyhow::Result<String> {
        self.0
            .decode(ids, /* skip_special_tokens */ true)
            .map_err(|error| anyhow::anyhow!("could not decode spotting output: {error}"))
    }
}

/// The loaded recognizer.
///
/// The model is behind a mutex because decoding mutates its KV cache, and one
/// document's images are transcribed one after another. Extraction of a
/// library is already parallel per file; making one image's decode parallel
/// with another's would contend for the same weights.
pub struct PaddleOcrVl {
    checkpoint: Checkpoint,
    model: Mutex<PaddleOCRVLModel>,
    tokenizer: Tokenizer,
    locations: LocationVocabulary,
    image_token_id: u32,
    eos_token_id: u32,
    device: Device,
    /// The device the recognizer actually realized, as the evaluation reports
    /// it. A latency figure that does not say what it ran on is not one.
    device_name: String,
    dtype: DType,
    spatial_merge: usize,
    patch_size: usize,
}

impl PaddleOcrVl {
    /// Load the installed checkpoint.
    pub fn load(data_dir: &Path, checkpoint: Checkpoint, device: &str) -> anyhow::Result<Self> {
        let artifacts = resolve(data_dir, &checkpoint)?;
        let realized = realize_device(select_device_plan(device))?;
        if let Some(reason) = &realized.fallback_reason {
            warn!("recognizer falling back to {}: {reason}", realized.name);
        }
        let device_name = realized.name.clone();
        let device = realized.device;
        // F32 everywhere: the CPU path is the one every supported platform
        // has, and candle's accelerate backend does not carry f16 matmul.
        let dtype = DType::F32;

        let config: Config = serde_json::from_slice(&std::fs::read(&artifacts.config)?)
            .context("could not read the recognizer's config")?;
        let tokenizer = Tokenizer::from_file(&artifacts.tokenizer)
            .map_err(|error| anyhow::anyhow!("could not read the recognizer's tokenizer: {error}"))?;
        let locations = LocationVocabulary::resolve(&tokenizer)?;
        let eos_token_id = tokenizer
            .token_to_id("</s>")
            .ok_or_else(|| anyhow::anyhow!("the tokenizer has no end-of-sequence token"))?;

        let weights = unsafe {
            VarBuilder::from_mmaped_safetensors(&[artifacts.weights.clone()], dtype, &device)?
        };
        let model = PaddleOCRVLModel::new(&config, weights)
            .context("could not build the recognizer from its weights")?;

        info!(
            "recognizer {} loaded on {} ({} vision layers, {} decoder layers)",
            checkpoint.name,
            realized.name,
            config.vision_config.num_hidden_layers,
            config.num_hidden_layers,
        );

        Ok(Self {
            checkpoint,
            model: Mutex::new(model),
            tokenizer,
            locations,
            image_token_id: config.image_token_id,
            eos_token_id,
            device,
            device_name,
            dtype,
            spatial_merge: config.vision_config.spatial_merge_size,
            patch_size: config.vision_config.patch_size,
        })
    }

    /// The prompt token ids for one image, with the placeholder run expanded
    /// to exactly the number of vision tokens the encoder will produce.
    fn prompt_ids(&self, grid_height: usize, grid_width: usize) -> anyhow::Result<Vec<u32>> {
        let encode = |text: &str| -> anyhow::Result<Vec<u32>> {
            Ok(self
                .tokenizer
                .encode(text, /* add_special_tokens */ false)
                .map_err(|error| anyhow::anyhow!("could not encode the task prompt: {error}"))?
                .get_ids()
                .to_vec())
        };
        let merge = self.spatial_merge * self.spatial_merge;
        let vision_tokens = grid_height * grid_width / merge;

        let mut ids = encode(PROMPT_PREFIX)?;
        ids.extend(std::iter::repeat(self.image_token_id).take(vision_tokens));
        ids.extend(encode(&format!(
            "{PROMPT_SUFFIX}{SPOTTING_PROMPT}\nAssistant:\n"
        ))?);
        Ok(ids)
    }

    /// Decode one spotting response, keeping the log-probability of every
    /// token chosen.
    ///
    /// Greedy: the transcription of a fixed image is a fact about the image,
    /// and a sampled one would make the reading — and therefore the rendition
    /// hash — depend on a random seed.
    fn decode_spotting(
        &self,
        prompt: &[u32],
        pixels: &Tensor,
        grid: &Tensor,
    ) -> anyhow::Result<Vec<SpottingToken>> {
        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow::anyhow!("the recognizer's lock was poisoned"))?;
        model.clear_kv_cache();

        let mut tokens = Vec::new();
        let input = Tensor::new(prompt, &self.device)?.unsqueeze(0)?;
        let mut logits = model.forward(&input, Some(pixels), Some(grid), 0)?;
        let mut offset = prompt.len();

        for _ in 0..MAX_NEW_TOKENS {
            let (id, logprob) = greedy(&logits)?;
            if id == self.eos_token_id {
                break;
            }
            tokens.push(SpottingToken {
                id,
                logprob,
                loc: self.locations.value(id),
            });
            let next = Tensor::new(&[id], &self.device)?.unsqueeze(0)?;
            logits = model.forward(&next, None, None, offset)?;
            offset += 1;
        }
        Ok(tokens)
    }
}

/// The most likely next token and the log-probability the decode gave it.
///
/// The softmax is taken in f32 and after subtracting the maximum, so the
/// signal does not depend on the logits' scale or overflow on a confident
/// step.
fn greedy(logits: &Tensor) -> anyhow::Result<(u32, f32)> {
    let values = logits
        .flatten_all()?
        .to_dtype(DType::F32)?
        .to_vec1::<f32>()?;
    let (index, max) = values.iter().enumerate().fold(
        (0usize, f32::NEG_INFINITY),
        |(best, best_value), (index, value)| {
            if *value > best_value {
                (index, *value)
            } else {
                (best, best_value)
            }
        },
    );
    let total: f32 = values.iter().map(|value| (value - max).exp()).sum();
    Ok((index as u32, -total.ln()))
}

impl OcrEngine for PaddleOcrVl {
    fn identity(&self) -> String {
        identity_of(&self.checkpoint)
    }

    fn admission_threshold(&self) -> f32 {
        ADMISSION_THRESHOLD
    }

    fn spot(&self, image: &image::RgbImage) -> anyhow::Result<Vec<SpottedRegion>> {
        let (pixels, width, height) = pixel_tensor(image, &self.device, self.dtype)?;
        let (grid_height, grid_width) = (
            height as usize / self.patch_size,
            width as usize / self.patch_size,
        );
        let grid = Tensor::new(
            &[[1u32, grid_height as u32, grid_width as u32]],
            &self.device,
        )?;
        let prompt = self.prompt_ids(grid_height, grid_width)?;
        let tokens = self.decode_spotting(&prompt, &pixels, &grid)?;
        parse_spotting(&tokens, &TokenizerDecoder(&self.tokenizer))
    }
}

// ── The evaluation ───────────────────────────────────────────────────────────

/// One expected transcription and where it sits.
///
/// The centre is in fractions of the image, the same space the recognizer
/// emits its quads in, so coordinate accuracy is a comparison and not a
/// conversion.
#[derive(Clone, Debug)]
pub struct ExpectedRegion {
    pub text: String,
    /// `None` where the corpus knows the words but not the geometry — a case
    /// collected from a real document rather than laid out. Coordinate
    /// accuracy is then simply not measured on it, which is honest; scoring
    /// it against a guessed centre would not be.
    pub centre: Option<crate::types::Point>,
}

/// One image of the evaluation corpus and what it should transcribe to.
pub struct EvaluationCase {
    pub name: String,
    pub image: image::RgbImage,
    /// The text a correct transcription contains, in reading order.
    pub expected: Vec<ExpectedRegion>,
}

/// One emitted region, judged.
///
/// The pair the admission rule is chosen from: how sure the decoder was, and
/// whether it was right. A threshold is only meaningful against the shape of
/// this, which is why the individual observations survive into the result
/// rather than being collapsed to a count at one candidate value.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct RegionObservation {
    pub confidence: f32,
    pub correct: bool,
}

/// What one candidate threshold would do to a corpus.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct AdmissionPoint {
    pub threshold: f32,
    /// Correct regions it lets into the reading.
    pub admitted_correct: usize,
    /// Wrong regions it lets into the reading — text the document does not
    /// contain, arriving with a page locator on it. The cost this rule exists
    /// to control.
    pub admitted_incorrect: usize,
    /// Correct regions it throws away.
    pub rejected_correct: usize,
}

/// What one checkpoint measured on one corpus.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvaluationResult {
    pub checkpoint: String,
    pub cases: usize,
    /// Character error rate against the expected text, joined in emission
    /// order: substitutions, insertions and deletions over expected length.
    pub character_error_rate: f64,
    /// The same edit distance over whitespace-separated words rather than
    /// characters. Reported beside the character rate because they fail
    /// differently: one misread letter costs a character and a whole word, so
    /// a transcription that is nearly right reads badly here and a
    /// transcription that drops a label reads badly in both.
    pub word_error_rate: f64,
    /// Per case, so one unreadable figure is visible as one unreadable figure
    /// rather than as a slightly worse average.
    pub per_case: Vec<CaseResult>,
    /// Regions the model emitted that no expected string accounts for.
    pub false_regions: usize,
    /// Expected strings no emitted region accounts for.
    pub missed_regions: usize,
    /// Mean distance between an emitted region's centre and the centre of the
    /// text it transcribed, in fractions of the image. Only over regions that
    /// transcribed correctly: the location of a misread is not a coordinate
    /// error, it is a different failure already counted above.
    pub centre_error: f64,
    /// The worst such distance. A mean hides the one label placed on the
    /// opposite side of the figure, and that is the failure that would put a
    /// search hit on the wrong part of the page.
    pub worst_centre_error: f64,
    /// How much of the model's emission order agrees with the order the figure
    /// draws its text in: one minus the fraction of region pairs the model
    /// emitted the wrong way round.
    ///
    /// 1.0 is complete agreement and 0.5 is what shuffling would give. The
    /// measurement exists because reading order is a way to be wrong that
    /// character error cannot see: a figure whose every label is transcribed
    /// perfectly still reads incorrectly if the labels arrive in an order the
    /// drawing does not support, and on a figure whose elements sit side by
    /// side there is more than one defensible order.
    ///
    /// Only regions that transcribed correctly are ordered — a misread has no
    /// place in the expected sequence, and it is already counted as a misread.
    /// Two expected regions that read the same string count as one position,
    /// so a repeated label contributes no disagreement either way.
    pub reading_order_agreement: f64,
    /// The pairs behind the fraction above, so an agreement of 1.0 over three
    /// comparisons is not read as an agreement of 1.0 over three hundred.
    pub reading_order_pairs: usize,
    pub observations: Vec<RegionObservation>,
    pub seconds_per_image: f64,
    /// What the checkpoint costs on disk, from its pinned artifact sizes.
    pub model_footprint_bytes: u64,
    /// The process's peak resident set after the run — the whole process, not
    /// the model alone, because that is the number that decides whether a
    /// machine can run this at all. `None` where the platform does not report
    /// it rather than a zero that would read as "measured, and small".
    pub peak_memory_bytes: Option<u64>,
    /// The build and machine the numbers above were produced on.
    ///
    /// FIGURE.md's evaluation list asks for supported-platform packaging, and
    /// this is the part of it a run can establish: a latency figure without
    /// the target, the device and the compiled backends beside it is not
    /// attributable to anything.
    pub platform: String,
}

/// The target, device and compiled inference backends of this run.
fn platform_description(device: &str) -> String {
    let backends = [
        ("candle-metal", cfg!(feature = "candle-metal")),
        ("candle-accelerate", cfg!(feature = "candle-accelerate")),
    ]
    .into_iter()
    .filter(|(_, enabled)| *enabled)
    .map(|(name, _)| name)
    .collect::<Vec<_>>();
    format!(
        "{}-{} / {device} / f32 / {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        match backends.is_empty() {
            true => "no accelerated backend compiled in".to_string(),
            false => backends.join(" + "),
        }
    )
}

/// The process's peak resident set size, where the platform reports one.
#[cfg(unix)]
fn peak_memory_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `getrusage` fills the whole `rusage` it is given for
    // `RUSAGE_SELF`, and it is only read after a success return.
    let read = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if read != 0 {
        warn!("could not read peak memory: {}", std::io::Error::last_os_error());
        return None;
    }
    let maxrss = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    // macOS reports bytes here and Linux reports kilobytes. The same field
    // with two units is the kind of thing that silently reports a gigabyte as
    // a megabyte, so it is converted by target rather than guessed at.
    Some(match cfg!(target_os = "macos") {
        true => maxrss,
        false => maxrss * 1024,
    })
}

#[cfg(not(unix))]
fn peak_memory_bytes() -> Option<u64> {
    None
}

/// One figure's outcome, kept separately from the aggregate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseResult {
    pub name: String,
    pub character_error_rate: f64,
    pub word_error_rate: f64,
    /// The order agreement of this figure alone, and the pairs it rests on.
    /// Per case because reading order is a property of a layout: one figure
    /// whose elements sit side by side can disagree while every other figure
    /// in the corpus is read top to bottom exactly.
    pub reading_order_agreement: f64,
    pub reading_order_pairs: usize,
    pub expected: usize,
    pub emitted: usize,
    pub missed: usize,
    pub seconds: f64,
    pub failure: Option<String>,
}

impl EvaluationResult {
    /// What each candidate threshold would admit and reject.
    ///
    /// The rule is chosen from this, not from a single number measured at a
    /// value someone already picked.
    pub fn admission_sweep(&self, candidates: &[f32]) -> Vec<AdmissionPoint> {
        candidates
            .iter()
            .map(|&threshold| {
                let admits = |observation: &&RegionObservation| observation.confidence >= threshold;
                AdmissionPoint {
                    threshold,
                    admitted_correct: self
                        .observations
                        .iter()
                        .filter(admits)
                        .filter(|observation| observation.correct)
                        .count(),
                    admitted_incorrect: self
                        .observations
                        .iter()
                        .filter(admits)
                        .filter(|observation| !observation.correct)
                        .count(),
                    rejected_correct: self
                        .observations
                        .iter()
                        .filter(|observation| observation.confidence < threshold)
                        .filter(|observation| observation.correct)
                        .count(),
                }
            })
            .collect()
    }
}

/// Measure one checkpoint on a corpus.
///
/// This is FIGURE.md's implementation-plan step 1 and step 9's metric list,
/// made runnable: character and word error, missed and false regions, reading
/// order, coordinate accuracy, latency, model footprint, peak memory, and the
/// platform they were all produced on. It needs the checkpoint installed —
/// 1.9 GB — so its caller is an ignored test rather than the suite.
pub fn evaluate(engine: &PaddleOcrVl, corpus: &[EvaluationCase]) -> EvaluationResult {
    let started = std::time::Instant::now();
    let (mut errors, mut expected_chars) = (0usize, 0usize);
    let (mut word_errors, mut expected_words) = (0usize, 0usize);
    let (mut false_regions, mut missed_regions) = (0usize, 0usize);
    let (mut centre_errors, mut worst_centre_error) = (Vec::new(), 0f64);
    let (mut order_agreements, mut order_pairs) = (0usize, 0usize);
    let (mut observations, mut per_case) = (Vec::new(), Vec::new());

    for case in corpus {
        let case_started = std::time::Instant::now();
        let case_chars: usize = case
            .expected
            .iter()
            .map(|region| region.text.chars().count())
            .sum();
        expected_chars += case_chars;
        let case_words: usize = case
            .expected
            .iter()
            .map(|region| region.text.split_whitespace().count())
            .sum();
        expected_words += case_words;

        let regions = match engine.spot(&case.image) {
            Ok(regions) => regions,
            Err(error) => {
                warn!("{}/{}: recognition failed: {error:#}", engine.checkpoint.name, case.name);
                missed_regions += case.expected.len();
                errors += case_chars;
                word_errors += case_words;
                per_case.push(CaseResult {
                    name: case.name.clone(),
                    character_error_rate: 1.0,
                    word_error_rate: 1.0,
                    // A run that emitted nothing put no regions in an order.
                    // Zero pairs is what says so; an agreement of 1.0 over
                    // zero pairs would read as a figure read perfectly.
                    reading_order_agreement: 1.0,
                    reading_order_pairs: 0,
                    expected: case.expected.len(),
                    emitted: 0,
                    missed: case.expected.len(),
                    seconds: case_started.elapsed().as_secs_f64(),
                    failure: Some(format!("{error:#}")),
                });
                continue;
            }
        };

        let expected_text: Vec<char> = case
            .expected
            .iter()
            .map(|region| region.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect();
        let emitted_text: Vec<char> = regions
            .iter()
            .map(|region| region.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect();
        let case_errors = edit_distance(&expected_text, &emitted_text);
        errors += case_errors;

        let expected_words_of = |regions: &[&str]| -> Vec<String> {
            regions
                .iter()
                .flat_map(|text| text.split_whitespace())
                .map(str::to_string)
                .collect()
        };
        let case_word_errors = edit_distance(
            &expected_words_of(
                &case
                    .expected
                    .iter()
                    .map(|region| region.text.as_str())
                    .collect::<Vec<_>>(),
            ),
            &expected_words_of(
                &regions
                    .iter()
                    .map(|region| region.text.as_str())
                    .collect::<Vec<_>>(),
            ),
        );
        word_errors += case_word_errors;

        // Where each correctly transcribed region sits in the order the figure
        // draws its text. The position of the *first* expected region with
        // that text, so two regions reading the same string share a position
        // and neither can disagree with the other.
        let mut emitted_positions: Vec<usize> = Vec::new();
        for region in &regions {
            let matched = case
                .expected
                .iter()
                .position(|expected| expected.text == region.text);
            observations.push(RegionObservation {
                confidence: region.confidence,
                correct: matched.is_some(),
            });
            match matched {
                None => false_regions += 1,
                Some(position) => {
                    emitted_positions.push(position);
                    if let Some(want) = case.expected[position].centre {
                        let centre = quad_centre(&region.quad);
                        let error =
                            f64::from((centre.x - want.x).hypot(centre.y - want.y));
                        worst_centre_error = worst_centre_error.max(error);
                        centre_errors.push(error);
                    }
                }
            }
        }
        let (case_order_agreements, case_order_pairs) = order_agreement(&emitted_positions);
        order_agreements += case_order_agreements;
        order_pairs += case_order_pairs;
        let missed = case
            .expected
            .iter()
            .filter(|expected| !regions.iter().any(|region| region.text == expected.text))
            .count();
        missed_regions += missed;
        let outcome = CaseResult {
            name: case.name.clone(),
            character_error_rate: if case_chars == 0 {
                f64::from(u32::from(!regions.is_empty()))
            } else {
                case_errors as f64 / case_chars as f64
            },
            word_error_rate: if case_words == 0 {
                f64::from(u32::from(!regions.is_empty()))
            } else {
                case_word_errors as f64 / case_words as f64
            },
            reading_order_agreement: fraction(case_order_agreements, case_order_pairs),
            reading_order_pairs: case_order_pairs,
            expected: case.expected.len(),
            emitted: regions.len(),
            missed,
            seconds: case_started.elapsed().as_secs_f64(),
            failure: None,
        };
        // A CPU run over a corpus is minutes per figure. Reported as it goes,
        // because an evaluation whose only output arrives at the end is one
        // you cannot tell from a hang.
        info!(
            "{}/{}: CER {:.3}, WER {:.3}, order {:.2} over {} pairs, \
             {} of {} expected, {:.1}s — {:?}",
            engine.checkpoint.name,
            outcome.name,
            outcome.character_error_rate,
            outcome.word_error_rate,
            outcome.reading_order_agreement,
            outcome.reading_order_pairs,
            outcome.expected - outcome.missed,
            outcome.expected,
            outcome.seconds,
            regions.iter().map(|region| &region.text).collect::<Vec<_>>(),
        );
        per_case.push(outcome);
    }

    EvaluationResult {
        checkpoint: engine.checkpoint.name.to_string(),
        cases: corpus.len(),
        character_error_rate: if expected_chars == 0 {
            0.0
        } else {
            errors as f64 / expected_chars as f64
        },
        word_error_rate: if expected_words == 0 {
            0.0
        } else {
            word_errors as f64 / expected_words as f64
        },
        per_case,
        false_regions,
        missed_regions,
        centre_error: if centre_errors.is_empty() {
            0.0
        } else {
            centre_errors.iter().sum::<f64>() / centre_errors.len() as f64
        },
        worst_centre_error,
        reading_order_agreement: fraction(order_agreements, order_pairs),
        reading_order_pairs: order_pairs,
        observations,
        seconds_per_image: if corpus.is_empty() {
            0.0
        } else {
            started.elapsed().as_secs_f64() / corpus.len() as f64
        },
        model_footprint_bytes: engine.checkpoint.footprint_bytes(),
        peak_memory_bytes: peak_memory_bytes(),
        platform: platform_description(&engine.device_name),
    }
}

/// `agreements / pairs`, with no pairs reported as complete agreement.
///
/// A corpus that put nothing in an order has nothing to disagree with, and the
/// count of pairs is reported beside every one of these so the distinction
/// between "agreed everywhere" and "nowhere to disagree" is never carried by
/// the fraction alone.
fn fraction(agreements: usize, pairs: usize) -> f64 {
    match pairs {
        0 => 1.0,
        pairs => agreements as f64 / pairs as f64,
    }
}

/// How many pairs of emitted regions are in the order the figure draws them,
/// and how many pairs were comparable at all.
///
/// Every pair of positions is compared, not just adjacent ones, so one label
/// emitted at the far end of the sequence costs as much as it should rather
/// than costing one swap. Pairs at the same position — two regions reading the
/// same string — are not comparable and are counted in neither.
fn order_agreement(positions: &[usize]) -> (usize, usize) {
    let (mut agreements, mut pairs) = (0usize, 0usize);
    for (index, earlier) in positions.iter().enumerate() {
        for later in &positions[index + 1..] {
            if earlier == later {
                continue;
            }
            pairs += 1;
            if earlier < later {
                agreements += 1;
            }
        }
    }
    (agreements, pairs)
}

/// The centre of a quadrilateral, as the mean of its corners.
fn quad_centre(quad: &[crate::types::Point; 4]) -> crate::types::Point {
    crate::types::Point {
        x: quad.iter().map(|point| point.x).sum::<f32>() / 4.0,
        y: quad.iter().map(|point| point.y).sum::<f32>() / 4.0,
    }
}

/// Levenshtein distance over whatever the units are — characters for the
/// character error rate, words for the word error rate. One implementation,
/// because the two rates differ in what they count and in nothing else.
///
/// Character-aware by construction: the caller has already split the text into
/// units, because a transcription is arbitrary Unicode and has no byte offsets
/// that are safe to index.
fn edit_distance<T: PartialEq>(expected: &[T], actual: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=actual.len()).collect();
    let mut current = vec![0usize; actual.len() + 1];
    for (row, want) in expected.iter().enumerate() {
        current[0] = row + 1;
        for (column, got) in actual.iter().enumerate() {
            let substitution = previous[column] + usize::from(want != got);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[actual.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where an evaluation run keeps its 1.9 GB per checkpoint. Named by the
    /// environment rather than defaulted, because a test that silently fills
    /// a home directory with model weights is not a test anyone can run
    /// twice.
    fn evaluation_model_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("WILKES_MODEL_DIR").map(std::path::PathBuf::from)
    }

    /// Byte counts as a person reads them, for the evaluation's own printout.
    fn format_bytes(bytes: u64) -> String {
        const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit + 1 < UNITS.len() {
            value /= 1024.0;
            unit += 1;
        }
        format!("{value:.2} {}", UNITS[unit])
    }

    /// The thresholds the sweep reports on. Wide, because the point is to see
    /// the shape of the decoder's confidence and not to confirm a value
    /// somebody already liked.
    const CANDIDATE_THRESHOLDS: &[f32] =
        &[0.0, 0.30, 0.50, 0.60, 0.70, 0.80, 0.90, 0.95, 0.99];

    /// FIGURE.md implementation-plan step 1, second half: the measurement that
    /// chooses the shipped checkpoint and the admission threshold.
    ///
    /// `WILKES_MODEL_DIR=<dir> [WILKES_SAMPLE_PDF=<path>] cargo test -p wilkes-core \
    ///     the_checkpoints_are_measured -- --ignored --nocapture`
    ///
    /// The sample document is optional and additive: without it the corpus is
    /// the built one, which is reproducible anywhere. With it, the figure this
    /// whole feature was specified against is measured too.
    #[test]
    #[ignore = "needs the installed checkpoints"]
    fn the_checkpoints_are_measured() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .try_init();
        let dir = evaluation_model_dir().expect("set WILKES_MODEL_DIR");

        // Both filters exist because a full run is hours: a corpus of eight
        // figures against two checkpoints, at CPU inference speed. Being able
        // to re-run one case against one checkpoint is what makes the harness
        // usable while the recipe is still being settled.
        let wanted = |name: &str, variable: &str| {
            std::env::var(variable).map_or(true, |filter| {
                filter.split(',').any(|want| name.contains(want.trim()))
            })
        };
        let mut corpus: Vec<EvaluationCase> = super::super::corpus::accuracy_corpus()
            .into_iter()
            .chain(sample_diagram_case())
            .filter(|case| wanted(&case.name, "WILKES_EVAL_CASES"))
            .collect();
        corpus.sort_by(|a, b| {
            (a.image.width() * a.image.height()).cmp(&(b.image.width() * b.image.height()))
        });
        assert!(!corpus.is_empty(), "WILKES_EVAL_CASES matched no figure");
        println!(
            "corpus of {} figures: {}\n",
            corpus.len(),
            corpus
                .iter()
                .map(|case| format!("{} ({}x{})", case.name, case.image.width(), case.image.height()))
                .collect::<Vec<_>>()
                .join(", ")
        );

        let mut report = Vec::new();
        for checkpoint in CHECKPOINTS
            .iter()
            .filter(|checkpoint| wanted(checkpoint.name, "WILKES_EVAL_CHECKPOINTS"))
        {
            let engine = PaddleOcrVl::load(&dir, *checkpoint, "cpu")
                .unwrap_or_else(|error| panic!("{}: {error:#}", checkpoint.name));
            let result = evaluate(&engine, &corpus);
            println!(
                "{}: CER {:.3}  WER {:.3}  missed {}  false {}  centre err {:.3} \
                 (worst {:.3})  order {:.2} over {} pairs  {:.1}s/image",
                result.checkpoint,
                result.character_error_rate,
                result.word_error_rate,
                result.missed_regions,
                result.false_regions,
                result.centre_error,
                result.worst_centre_error,
                result.reading_order_agreement,
                result.reading_order_pairs,
                result.seconds_per_image,
            );
            println!(
                "    {} on disk, peak RSS {}, on {}",
                format_bytes(result.model_footprint_bytes),
                result
                    .peak_memory_bytes
                    .map_or("not reported by this platform".to_string(), format_bytes),
                result.platform,
            );
            for case in &result.per_case {
                println!(
                    "    {:<22} CER {:.3}  WER {:.3}  order {:.2}/{:<3} \
                     expected {:<2} emitted {:<2} missed {:<2} {:.1}s{}",
                    case.name,
                    case.character_error_rate,
                    case.word_error_rate,
                    case.reading_order_agreement,
                    case.reading_order_pairs,
                    case.expected,
                    case.emitted,
                    case.missed,
                    case.seconds,
                    case.failure
                        .as_deref()
                        .map_or(String::new(), |failure| format!("  FAILED: {failure}")),
                );
            }
            println!("    admission sweep (threshold: correct in / wrong in / correct lost)");
            for point in result.admission_sweep(CANDIDATE_THRESHOLDS) {
                println!(
                    "      {:.2}: {} / {} / {}",
                    point.threshold,
                    point.admitted_correct,
                    point.admitted_incorrect,
                    point.rejected_correct,
                );
            }
            report.push(result);
        }

        if let Some(path) = std::env::var_os("WILKES_EVAL_OUT") {
            std::fs::write(&path, serde_json::to_vec_pretty(&report).expect("serializes"))
                .expect("writes the evaluation record");
            println!("\nwrote {}", std::path::Path::new(&path).display());
        }
    }

    /// Write the sample document's diagram to a PNG, so ground truth can be
    /// written from the figure rather than from a summary of it.
    ///
    /// `WILKES_SAMPLE_PDF=<path> WILKES_EVAL_OUT=<file.png> cargo test -p wilkes-core \
    ///     the_sample_diagram_can_be_looked_at -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a local corpus document"]
    fn the_sample_diagram_can_be_looked_at() {
        let Some(case) = sample_diagram_case() else {
            return;
        };
        let out = std::env::var("WILKES_EVAL_OUT").expect("set WILKES_EVAL_OUT to a .png path");
        case.image.save(&out).expect("the figure is written");
        println!("{} ({}x{})", out, case.image.width(), case.image.height());
    }

    /// The figure this feature was specified against, taken out of the real
    /// document by the real extraction path.
    ///
    /// The ground truth is the twelve lines the figure *draws*, in the row
    /// order it draws them — not the six labels the spec's prose summarises
    /// it as. Every label in this diagram is set on two lines inside its own
    /// shape, and the three circles sit side by side, so the drawn reading
    /// order runs across the row before it runs down. Expecting the six
    /// concepts instead scored a completely correct transcription at CER
    /// 0.663 and zero of six regions, which was a fact about this list and
    /// not about the recognizer.
    ///
    /// Text only: the document records what its diagram says, not where on it
    /// each line sits, so this case measures transcription and contributes
    /// nothing to the coordinate figure rather than contributing a guess.
    fn sample_diagram_case() -> Option<EvaluationCase> {
        use crate::extract::ContentExtractor;

        let path = std::env::var("WILKES_SAMPLE_PDF").ok()?;
        let capture = std::sync::Arc::new(super::super::corpus::ImageCapture::default());
        crate::extract::pdf::PdfExtractor::with_image_analyzer(std::sync::Arc::new(
            super::super::NativeImageAnalyzer::new(Box::new(capture.clone()), None),
        ))
        .extract(std::path::Path::new(&path))
        .expect("the sample document extracts");
        let images = capture.images.lock().expect("capture lock");
        let image = images
            .iter()
            .find(|image| (image.width(), image.height()) == (1559, 499))
            .expect("the sample document draws the 1559x499 expert-system diagram")
            .clone();
        Some(EvaluationCase {
            name: "sample-expert-system".to_string(),
            image,
            expected: [
                // Row 1: the tops of the three circles.
                "User",
                "Inference",
                "Knowledge",
                // Row 2: their second lines.
                "interface",
                "engine",
                "base",
                // Row 3: the figure on the left, and the brain on the right.
                "Non-",
                "Expert",
                // Row 4.
                "expert",
                "knowledge",
                // Row 5 and 6: the label on the enclosing box.
                "Expert",
                "system",
            ]
            .into_iter()
            .map(|text| ExpectedRegion {
                text: text.to_string(),
                centre: None,
            })
            .collect(),
        })
    }

    /// FIGURE.md implementation-plan step 1, first half: the pinned artifacts
    /// are what the pins say they are, and the candle module builds a model
    /// out of them.
    ///
    /// `WILKES_MODEL_DIR=<dir> cargo test -p wilkes-core \
    ///     the_pinned_checkpoints_install_and_load -- --ignored --nocapture`
    #[test]
    #[ignore = "downloads 1.9 GB per checkpoint"]
    fn the_pinned_checkpoints_install_and_load() {
        let dir = evaluation_model_dir().expect("set WILKES_MODEL_DIR");
        for checkpoint in CHECKPOINTS {
            install(&dir, checkpoint, None)
                .unwrap_or_else(|error| panic!("{}: {error:#}", checkpoint.name));
            let started = std::time::Instant::now();
            let engine = PaddleOcrVl::load(&dir, *checkpoint, "cpu")
                .unwrap_or_else(|error| panic!("{}: {error:#}", checkpoint.name));
            println!(
                "{}: loaded in {:.1}s as {}",
                checkpoint.name,
                started.elapsed().as_secs_f64(),
                engine.identity()
            );
        }
    }

    /// Both dimensions land on the grid, the aspect ratio survives, and the
    /// area stays inside the task's envelope. The recognizer cannot be given
    /// an image that is not a whole number of merged patches.
    #[test]
    fn preprocessing_grids_the_image_and_respects_the_envelope() {
        for (width, height) in [(1559, 499), (16, 16), (4000, 120), (800, 1200), (3, 900)] {
            let (resized_width, resized_height) = spotting_dimensions(width, height);
            assert_eq!(
                (resized_width % RESIZE_FACTOR, resized_height % RESIZE_FACTOR),
                (0, 0),
                "{width}x{height} is not on the grid"
            );
            let pixels = u64::from(resized_width) * u64::from(resized_height);
            assert!(
                pixels <= MAX_PIXELS,
                "{width}x{height} became {resized_width}x{resized_height}, over the envelope"
            );
            assert!(resized_width > 0 && resized_height > 0);
        }
    }

    /// The sample document's diagram, at the size FIGURE.md records. It is
    /// over the upscale threshold on its long side, so it is not doubled, and
    /// its aspect ratio is held.
    #[test]
    fn the_sample_diagram_keeps_its_shape() {
        let (width, height) = spotting_dimensions(1559, 499);
        let ratio = f64::from(width) / f64::from(height);
        assert!(
            (ratio - 1559.0 / 499.0).abs() < 0.05,
            "{width}x{height} distorts the diagram"
        );
    }

    /// A small crop is doubled first: the spotting task's own recipe, and the
    /// difference between reading a 200-pixel label and not.
    #[test]
    fn a_small_image_is_upscaled_before_gridding() {
        let (small_width, small_height) = spotting_dimensions(200, 100);
        let (large_width, large_height) = spotting_dimensions(1600, 800);
        assert!(
            small_width * small_height >= MIN_PIXELS as u32,
            "{small_width}x{small_height} is under the floor"
        );
        assert!(large_width >= small_width && large_height >= small_height);
    }

    /// A greedy step's log-probability is the log of the chosen token's
    /// softmax share, and it does not depend on the logits' offset.
    #[test]
    fn the_admission_signal_is_the_chosen_tokens_log_probability() {
        let confident = Tensor::new(&[[0.0f32, 10.0, 0.0]], &Device::Cpu).unwrap();
        let (id, logprob) = greedy(&confident).unwrap();
        assert_eq!(id, 1);
        assert!(logprob.exp() > 0.99, "{}", logprob.exp());

        let undecided = Tensor::new(&[[1.0f32, 1.0, 1.0]], &Device::Cpu).unwrap();
        let (_, logprob) = greedy(&undecided).unwrap();
        assert!((logprob.exp() - 1.0 / 3.0).abs() < 1e-5, "{}", logprob.exp());

        // Shifting every logit by a constant is the same distribution.
        let shifted = Tensor::new(&[[100.0f32, 110.0, 100.0]], &Device::Cpu).unwrap();
        let (shifted_id, shifted_logprob) = greedy(&shifted).unwrap();
        assert_eq!(shifted_id, 1);
        let (_, original) = greedy(&confident).unwrap();
        assert!((shifted_logprob - original).abs() < 1e-4);
    }

    /// Every shipped artifact is pinned to an immutable commit and named by
    /// size and digest. A branch would re-resolve to whatever was last pushed.
    #[test]
    fn every_checkpoint_is_pinned_by_revision_size_and_digest() {
        for checkpoint in CHECKPOINTS {
            assert_eq!(checkpoint.revision.len(), 40, "{}", checkpoint.name);
            assert!(
                checkpoint
                    .revision
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "{} is not a commit sha",
                checkpoint.name
            );
            for artifact in checkpoint.artifacts() {
                assert_eq!(artifact.sha256.len(), 64, "{}", artifact.filename);
                assert!(artifact.size_bytes > 0, "{}", artifact.filename);
            }
        }
    }

    /// FIGURE.md: *the redistributed checkpoint must still receive a
    /// model-specific license/provenance inventory before it is packaged.*
    ///
    /// The inventory is checked rather than asserted in prose, and it is
    /// checked against the artifact list the install actually walks — so a
    /// fourth file added to a checkpoint cannot be downloaded onto a user's
    /// disk without appearing in what the user is told they are downloading.
    #[test]
    fn every_shipped_artifact_is_covered_by_a_license_and_provenance_inventory() {
        for checkpoint in CHECKPOINTS {
            let inventory = inventory(checkpoint);
            assert_eq!(inventory.license, "Apache-2.0", "{}", checkpoint.name);
            assert!(
                inventory.license_url.starts_with("https://"),
                "{} has no reachable license statement",
                checkpoint.name
            );
            assert!(
                !inventory.derived_from.is_empty(),
                "{} names nothing it is derived from",
                checkpoint.name
            );
            assert_eq!(
                inventory.artifacts.len(),
                checkpoint.artifacts().len(),
                "{} installs files the inventory does not name",
                checkpoint.name
            );
            for (inventoried, artifact) in inventory.artifacts.iter().zip(checkpoint.artifacts()) {
                assert_eq!(inventoried.filename, artifact.filename);
                assert_eq!(inventoried.sha256, artifact.sha256);
                assert_eq!(inventoried.size_bytes, artifact.size_bytes);
            }
            assert_eq!(
                inventory.footprint_bytes,
                inventory
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.size_bytes)
                    .sum::<u64>(),
            );
            assert_eq!(inventory.revision, checkpoint.revision);
        }
    }

    /// The two checkpoints must be distinguishable, or the evaluation that
    /// chooses between them would be comparing one thing with itself.
    #[test]
    fn the_two_checkpoints_are_different_weights() {
        assert_ne!(CHECKPOINT_1_5.weights.sha256, CHECKPOINT_1_6.weights.sha256);
        assert_ne!(CHECKPOINT_1_5.revision, CHECKPOINT_1_6.revision);
        // Same tokenizer, so a location token means the same thing in both.
        assert_eq!(
            CHECKPOINT_1_5.tokenizer.sha256,
            CHECKPOINT_1_6.tokenizer.sha256
        );
    }

    #[test]
    fn the_edit_distance_counts_all_three_edits() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        assert_eq!(edit_distance(&chars("kitten"), &chars("sitting")), 3);
        assert_eq!(edit_distance(&chars("Größe"), &chars("Größe")), 0);
        assert_eq!(edit_distance(&chars(""), &chars("abc")), 3);
    }

    /// The same distance over words, which is the whole difference between the
    /// two rates. One misread letter is one character and one whole word.
    #[test]
    fn the_edit_distance_counts_words_when_it_is_given_words() {
        let words = |text: &str| {
            text.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            edit_distance(&words("Knowledge base"), &words("Knowledge bose")),
            1
        );
        assert_eq!(
            edit_distance(&words("User interface"), &words("User interface")),
            0
        );
        assert_eq!(edit_distance(&words("a b c"), &words("a c")), 1);
    }

    /// Reading order is a way to be wrong that character error cannot see, so
    /// it is measured on its own: perfect agreement, complete reversal, and
    /// one label emitted out of turn.
    #[test]
    fn reading_order_scores_the_sequence_and_not_the_characters() {
        assert_eq!(order_agreement(&[0, 1, 2, 3]), (6, 6));
        assert_eq!(order_agreement(&[3, 2, 1, 0]), (0, 6));

        // One label emitted last that the figure draws first: it disagrees
        // with each of the three it should have preceded, and nothing else.
        let (agreements, pairs) = order_agreement(&[1, 2, 3, 0]);
        assert_eq!((agreements, pairs), (3, 6));
        assert!((fraction(agreements, pairs) - 0.5).abs() < 1e-9);
    }

    /// Two regions that read the same string are one position, so a repeated
    /// label — `Expert` twice in the sample's diagram — cannot disagree with
    /// itself in either direction.
    #[test]
    fn a_repeated_label_is_one_position_and_disagrees_with_nothing() {
        assert_eq!(order_agreement(&[2, 2, 2]), (0, 0));
        assert_eq!(order_agreement(&[0, 2, 2, 3]), (5, 5));
    }

    /// Nothing emitted is nothing in an order. The pair count is what says so,
    /// which is why every agreement is reported beside one.
    #[test]
    fn an_empty_sequence_has_no_pairs_to_agree_about() {
        assert_eq!(order_agreement(&[]), (0, 0));
        assert_eq!(order_agreement(&[4]), (0, 0));
        assert_eq!(fraction(0, 0), 1.0);
    }

    /// The footprint is the pinned sizes and nothing else — the number a user
    /// is told before a 1.9 GB download starts.
    #[test]
    fn the_footprint_is_the_sum_of_the_pinned_artifacts() {
        for checkpoint in CHECKPOINTS {
            assert_eq!(
                checkpoint.footprint_bytes(),
                checkpoint.weights.size_bytes
                    + checkpoint.tokenizer.size_bytes
                    + checkpoint.config.size_bytes,
            );
        }
        assert!(SHIPPED_CHECKPOINT.footprint_bytes() > 1_900_000_000);
    }

    /// A latency figure with no target, device or compiled backend beside it
    /// is not attributable to anything, so the run records all three.
    #[test]
    fn the_platform_names_the_target_the_device_and_the_backends() {
        let platform = platform_description("cpu");
        assert!(platform.contains(std::env::consts::OS), "{platform}");
        assert!(platform.contains(std::env::consts::ARCH), "{platform}");
        assert!(platform.contains("cpu"), "{platform}");
        assert!(platform.contains("f32"), "{platform}");
    }

    /// Peak memory is the number that decides whether a machine can run this
    /// at all. Where the platform reports one it is a real quantity; where it
    /// does not, it is absent rather than a zero that would read as measured.
    #[test]
    fn peak_memory_is_a_real_quantity_or_is_absent() {
        match peak_memory_bytes() {
            Some(bytes) => assert!(bytes > 1_000_000, "{bytes} bytes is not this process"),
            None => assert!(!cfg!(unix), "unix reports a resident set size"),
        }
    }
}
