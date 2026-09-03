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

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::RgbImage;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Tensor};
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

pub const ENCODER_GRAPH: &str = "onnx/encoder_model_quantized.onnx";
/// Step 0's decoder: no cache in, the whole `present.*` cache out.
pub const DECODER_GRAPH: &str = "onnx/decoder_model_quantized.onnx";
/// Every later step's decoder: one token in, the cache in and back out.
///
/// Required, not optional — see "Why two decoder graphs" above for what it
/// was measured to be worth, and [`identity`] for why it changes the recipe.
pub const DECODER_WITH_PAST_GRAPH: &str = "onnx/decoder_with_past_model_quantized.onnx";

pub const ARTIFACTS: &[&str] = &[
    ENCODER_GRAPH,
    DECODER_GRAPH,
    DECODER_WITH_PAST_GRAPH,
    "tokenizer.json",
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

/// Strip the `$$…$$` the model wraps its answer in.
///
/// The delimiters are the model's notation for "this is display mathematics",
/// not part of the expression, and every consumer of a formula region in this
/// codebase — the LaTeX validity check, the reader's substitution, the
/// embedder — wants the expression.
pub fn unwrap_delimiters(text: &str) -> &str {
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

// ── The decoder's key/value cache ─────────────────────────────────────────────

/// The names Optimum gives an encoder-decoder's tensors.
///
/// Constants because a typo in a tensor name is a runtime error deep inside a
/// decode loop, and because writing them once is how [`CacheShape`] below stays
/// checkable. The same convention [`super::onnx_vlm`] discovers granite's
/// decoder by; what differs is that an encoder-decoder carries two kinds of
/// cache rather than one.
const INPUT_IDS: &str = "input_ids";
const ENCODER_HIDDEN_STATES: &str = "encoder_hidden_states";
const LOGITS: &str = "logits";
const PAST_PREFIX: &str = "past_key_values.";
const PRESENT_PREFIX: &str = "present.";
/// `past_key_values.N.decoder.key` — attention over the tokens decoded so far.
const SELF_ATTENTION: &str = ".decoder.";
/// `past_key_values.N.encoder.key` — attention over the encoder's positions.
const CROSS_ATTENTION: &str = ".encoder.";

/// Which cache tensors the two decoder graphs pass between them.
///
/// Read off the graphs at load rather than declared here. The layer count, and
/// the split between the two kinds of cache, are properties of the export, and
/// a runner that hardcoded eight layers would be a runner for exactly one
/// checkpoint — the argument [`super::onnx_vlm::DecoderShape`] already makes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheShape {
    /// `(past_key_values.N.decoder.*, present.N.decoder.*)`.
    ///
    /// One position longer at every step, so it comes back out of every run
    /// and goes straight back in.
    pub self_attention: Vec<(String, String)>,
    /// `(past_key_values.N.encoder.*, present.N.encoder.*)`.
    ///
    /// The encoder's 196 positions projected into each layer's keys and
    /// values. They do not depend on the tokens decoded so far, so step 0
    /// computes them once and every later step is handed the same tensors
    /// back unchanged — the cached graph does not even return them. Not
    /// recomputing this is most of what the cache buys.
    pub cross_attention: Vec<(String, String)>,
}

impl CacheShape {
    /// Read the shape off the two loaded decoder sessions.
    ///
    /// `first` is the graph that takes no past; `with_past` is the one that
    /// takes it. Every check here is against what the graphs themselves
    /// declare, because the two have to agree for a decode split across them
    /// to mean anything, and the place to find out is at load rather than at
    /// step one of a document.
    ///
    /// `pub` for the same reason [`preprocess`] is: a probe that discovered
    /// the cache by its own rules would be measuring a decode nobody runs.
    /// Pure, and holds no state.
    pub fn discover(first: &Session, with_past: &Session) -> Result<Self> {
        let inputs = |session: &Session| -> Vec<String> {
            session
                .inputs()
                .iter()
                .map(|input| input.name().to_string())
                .collect()
        };
        let outputs = |session: &Session| -> Vec<String> {
            session
                .outputs()
                .iter()
                .map(|output| output.name().to_string())
                .collect()
        };
        Self::of(
            &inputs(first),
            &outputs(first),
            &inputs(with_past),
            &outputs(with_past),
        )
    }

    /// The same rules over four lists of names.
    ///
    /// Split out from [`Self::discover`] so what the two graphs have to agree
    /// about can be stated against name lists — which is the whole of the
    /// rule — rather than only against 460 MB of weights a unit test cannot
    /// load.
    fn of(
        first_inputs: &[String],
        first_outputs: &[String],
        with_past_inputs: &[String],
        with_past_outputs: &[String],
    ) -> Result<Self> {
        for (session, declared, wanted) in [
            ("the first-step decoder", first_inputs, INPUT_IDS),
            (
                "the first-step decoder",
                first_inputs,
                ENCODER_HIDDEN_STATES,
            ),
            ("the cached decoder", with_past_inputs, INPUT_IDS),
        ] {
            anyhow::ensure!(
                declared.iter().any(|name| name == wanted),
                "{session} declares no {wanted} input"
            );
        }
        for (session, declared) in [
            ("the first-step decoder", first_outputs),
            ("the cached decoder", with_past_outputs),
        ] {
            anyhow::ensure!(
                declared.iter().any(|name| name == LOGITS),
                "{session} returns no {LOGITS}"
            );
        }
        // If it wanted the encoder's states it would be re-projecting them,
        // which is the cost this graph exists to avoid.
        anyhow::ensure!(
            !with_past_inputs
                .iter()
                .any(|name| name == ENCODER_HIDDEN_STATES),
            "the cached decoder wants {ENCODER_HIDDEN_STATES}, so it is not a \
             decoder-with-past export"
        );

        let mut shape = Self::default();
        for name in with_past_inputs {
            // `strip_prefix`, never a byte offset: these are graph-declared
            // names and the boundary is the prefix's own end.
            let Some(tail) = name.strip_prefix(PAST_PREFIX) else {
                continue;
            };
            let present = format!("{PRESENT_PREFIX}{tail}");
            anyhow::ensure!(
                first_outputs.contains(&present),
                "the cached decoder wants {name}, but the first-step decoder does not \
                 return {present} for it"
            );
            if name.contains(SELF_ATTENTION) {
                anyhow::ensure!(
                    with_past_outputs.contains(&present),
                    "the cached decoder takes {name} but does not return {present}; a \
                     self-attention cache that does not grow is not a cache"
                );
                shape.self_attention.push((name.clone(), present));
            } else if name.contains(CROSS_ATTENTION) {
                anyhow::ensure!(
                    !with_past_outputs.contains(&present),
                    "the cached decoder returns {present}; this loop passes the \
                     cross-attention cache back unchanged and would be discarding it"
                );
                shape.cross_attention.push((name.clone(), present));
            } else {
                anyhow::bail!(
                    "the cached decoder declares {name}, which is neither a \
                     {SELF_ATTENTION} nor an {CROSS_ATTENTION} cache"
                );
            }
        }

        anyhow::ensure!(
            !shape.self_attention.is_empty(),
            "the cached decoder declares no {PAST_PREFIX}* inputs; this is not a \
             decoder-with-past export"
        );
        anyhow::ensure!(
            !shape.cross_attention.is_empty(),
            "the cached decoder declares no cross-attention cache; this is not an \
             encoder-decoder export"
        );
        Ok(shape)
    }

    /// How many layers the two caches cover. Each layer contributes a key and
    /// a value, so this is half the tensor count.
    pub fn layers(&self) -> usize {
        self.self_attention.len() / 2
    }
}

/// The most likely next token off a decoder's `logits`, and its share.
///
/// The last position's row, whatever the graph projected: the first-step graph
/// declares a `decoder_sequence_length` axis and the cached one declares one of
/// exactly 1, and reading from the end is right for both.
fn chosen_token(outputs: &SessionOutputs<'_>) -> Result<(i64, f32)> {
    let (logit_shape, logits) = outputs[LOGITS]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow::anyhow!("the decoder returned nothing usable: {e}"))?;
    let vocabulary = *logit_shape.last().unwrap_or(&0) as usize;
    anyhow::ensure!(vocabulary > 0, "the decoder returned an empty vocabulary");
    anyhow::ensure!(
        logits.len() >= vocabulary,
        "the decoder returned {} logits, short of one {vocabulary}-wide row",
        logits.len()
    );
    Ok(argmax_with_probability(
        &logits[logits.len() - vocabulary..],
    ))
}

// ── The engine ───────────────────────────────────────────────────────────────

/// One loaded copy of the three graphs, and what they declared.
struct Reader {
    encoder: Session,
    /// Step 0: the start token and the encoder's states in, the whole cache
    /// out.
    decoder: Session,
    /// Every later step: one token and the cache in, the grown self-attention
    /// cache out.
    decoder_with_past: Session,
    cache: CacheShape,
}

pub struct Texify {
    /// Where the graphs are and what to load them with, kept so the sessions
    /// can be dropped and rebuilt: [`OcrEngine::release`] is a promise that a
    /// later `spot_batch` still works.
    dir: PathBuf,
    threads: usize,
    /// One loaded copy of the three graphs per crop read at once.
    ///
    /// A crop's read is a vision encode and then a serial decode, and neither
    /// fills the machine: at four intra-op threads one reader burns three
    /// cores of ten and the decode gains 1.5% from doubling them, because each
    /// step is a pass over the whole decoder and no thread count makes memory
    /// arrive sooner. The way to use the rest is more readers rather than
    /// wider ones — the same finding [`super::granite_docling`] records, and
    /// the same shape of answer. [`super::dispatch::recognizer_layout`] holds
    /// the measurement and picks the numbers.
    ///
    /// Each entry is `None` until the crop that first needs it, and after a
    /// release: a runtime that attaches a recognizer and indexes nothing must
    /// not pay 543 MB a reader for it, and a document with one crop in it must
    /// not pay for three.
    readers: Vec<Mutex<Option<Reader>>>,
    tokenizer: Tokenizer,
}

impl Texify {
    /// Address `readers` copies of the model, each to be given `threads`
    /// threads when it is first needed.
    ///
    /// The two numbers are the caller's because they describe the machine
    /// rather than the model, exactly as they are for
    /// [`super::granite_docling::GraniteDocling::load`]; the policy and the
    /// measurements behind it live in [`super::dispatch`].
    pub fn load(model_dir: &Path, readers: usize, threads: usize) -> Result<Self> {
        let dir = install_dir(model_dir);
        anyhow::ensure!(
            is_installed(model_dir),
            "the {MODEL_ID} recognizer is not installed under {}",
            dir.display()
        );
        anyhow::ensure!(readers > 0, "a recognizer needs at least one reader");
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!("could not read the tokenizer: {e}"))?;
        Ok(Self {
            dir,
            threads,
            readers: (0..readers).map(|_| Mutex::new(None)).collect(),
            tokenizer,
        })
    }

    /// How many crops this recognizer can read at once.
    pub fn readers(&self) -> usize {
        self.readers.len()
    }

    /// The three graphs, loaded. One reader's worth.
    fn open(&self) -> Result<Reader> {
        let encoder = self.session(ENCODER_GRAPH)?;
        let decoder = self.session(DECODER_GRAPH)?;
        let decoder_with_past = self.session(DECODER_WITH_PAST_GRAPH)?;
        let cache = CacheShape::discover(&decoder, &decoder_with_past)?;
        debug!(
            layers = cache.layers(),
            self_attention = cache.self_attention.len(),
            cross_attention = cache.cross_attention.len(),
            "loaded {MODEL_ID}'s decoder pair"
        );
        Ok(Reader {
            encoder,
            decoder,
            decoder_with_past,
            cache,
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

    /// Read one crop: encode once, then decode greedily through the cache.
    ///
    /// Two graphs, one loop. Step 0 goes through the graph that takes no past
    /// and returns the whole cache; every later step goes through the graph
    /// that takes the cache and one token. There is no uncached path beside
    /// this one: an export missing either graph is an error at load, not a
    /// slower decode nobody was told about.
    ///
    /// What that is worth, over the 42 crops of this repository's
    /// `formula_recall` fixture at four intra-op threads: 31.4 ms a step and
    /// 1292 ms a crop became 4.35 ms a step and 299 ms a crop. The module's
    /// own documentation carries the table and the page numbers.
    fn read(&self, reader: &mut Reader, crop: &RgbImage) -> Result<(String, f32, bool)> {
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
        // Both caches are carried as the decoder's own output values, moved
        // straight back into the next step's inputs. Reading them into Rust
        // and rebuilding tensors would copy the whole cache twice a token;
        // ORT values are reference-counted handles, so moving them is free.
        let mut self_cache: Vec<DynValue> = Vec::new();
        let mut cross_cache: Vec<DynValue> = Vec::new();
        let mut feed_token = START_TOKEN;
        for step in 0..MAX_NEW_TOKENS {
            let input = Tensor::from_array((vec![1i64, 1], vec![feed_token]))?;
            let (next, probability) = if step == 0 {
                let states = Tensor::from_array((shape.clone(), hidden.clone()))?;
                let mut out = reader
                    .decoder
                    .run(vec![
                        (INPUT_IDS.to_string(), SessionInputValue::from(input)),
                        (
                            ENCODER_HIDDEN_STATES.to_string(),
                            SessionInputValue::from(states),
                        ),
                    ])
                    .map_err(|e| anyhow::anyhow!("the decoder failed: {e}"))?;
                let chosen = chosen_token(&out)?;
                for (past, present) in &reader.cache.self_attention {
                    self_cache.push(take_cache(&mut out, present, past)?);
                }
                for (past, present) in &reader.cache.cross_attention {
                    cross_cache.push(take_cache(&mut out, present, past)?);
                }
                chosen
            } else {
                let mut feed: Vec<(String, SessionInputValue<'_>)> =
                    Vec::with_capacity(1 + self_cache.len() + cross_cache.len());
                feed.push((INPUT_IDS.to_string(), SessionInputValue::from(input)));
                for ((past, _), value) in
                    reader.cache.self_attention.iter().zip(self_cache.drain(..))
                {
                    feed.push((past.clone(), SessionInputValue::from(value)));
                }
                // By reference, and never rebuilt: the encoder's projections
                // are the same tensors for the whole decode.
                for ((past, _), value) in reader.cache.cross_attention.iter().zip(&cross_cache) {
                    feed.push((past.clone(), SessionInputValue::from(value)));
                }
                let mut out = reader.decoder_with_past.run(feed).map_err(|e| {
                    anyhow::anyhow!("the cached decoder failed at step {step}: {e}")
                })?;
                let chosen = chosen_token(&out)?;
                for (past, present) in &reader.cache.self_attention {
                    self_cache.push(take_cache(&mut out, present, past)?);
                }
                chosen
            };
            confidence += f64::from(probability);
            if next == EOS_TOKEN {
                hit_the_cap = false;
                break;
            }
            ids.push(next);
            feed_token = next;
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
            hit_the_cap,
        ))
    }
}

/// Move one cache tensor out of a decoder's outputs.
///
/// Named rather than inlined because the same three lines are needed for both
/// caches at step 0 and for the self-attention cache at every step after, and
/// because the error has to say which tensor was missing and which input it
/// was going to feed.
fn take_cache(outputs: &mut SessionOutputs<'_>, present: &str, past: &str) -> Result<DynValue> {
    outputs
        .remove(present)
        .with_context(|| format!("the decoder did not return {present} for {past}"))
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
        // Across every reader at once, one crop at a time on each, taken from
        // a shared queue rather than divided up front: crops decode anything
        // from four tokens to the cap, so a fixed division would leave most
        // readers idle behind the one that drew the longest expression. The
        // hand a crop is read on is the reader it locks, and results come back
        // in the caller's order — see [`super::granite_docling::in_parallel`],
        // which is the one mechanism for this and is shared rather than
        // reproduced here.
        let done = std::sync::atomic::AtomicUsize::new(0);
        super::granite_docling::in_parallel(images, self.readers.len(), |hand, image| {
            let mut held = self.readers[hand]
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.is_none() {
                *held = Some(self.open()?);
            }
            let reader = held.as_mut().expect("just loaded");
            let (text, confidence, truncated) = self.read(reader, image)?;
            debug!(
                "read formula {} of {} with {MODEL_ID}: {text:?}",
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
                images.len()
            );
            Ok(if text.trim().is_empty() {
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
                    // Set from the decode loop, never guessed at here: only
                    // the loop that ran the cap knows whether it ended on EOS
                    // or was cut off by it. `ocr::admit` refuses this before
                    // it ever asks whether the LaTeX closes.
                    truncated,
                    // A formula, not a table: no grid, nothing to judge.
                    structure: None,
                }])
            })
        })
    }

    fn release(&self) {
        let mut dropped = 0usize;
        for reader in &self.readers {
            if reader
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .is_some()
            {
                dropped += 1;
            }
        }
        if dropped > 0 {
            debug!("{MODEL_ID} released {dropped} reader(s); the next crop reloads them");
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

    // ── The cached decoder ───────────────────────────────────────────────────

    /// The names the pinned export declares, as `ort` reports them. Written
    /// out rather than read from the graphs so the rules below are testable on
    /// a machine that has never downloaded 543 MB of weights; the graphs
    /// themselves are what [`CacheShape::discover`] reads at load, and the
    /// probe's `equivalence` mode is what checks these against them.
    fn pinned_names(layers: usize) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        let first_inputs = vec![INPUT_IDS.to_string(), ENCODER_HIDDEN_STATES.to_string()];
        let mut first_outputs = vec![LOGITS.to_string()];
        let mut with_past_inputs = vec![INPUT_IDS.to_string()];
        let mut with_past_outputs = vec![LOGITS.to_string()];
        for layer in 0..layers {
            for part in ["key", "value"] {
                first_outputs.push(format!("present.{layer}.decoder.{part}"));
                first_outputs.push(format!("present.{layer}.encoder.{part}"));
                with_past_inputs.push(format!("past_key_values.{layer}.decoder.{part}"));
                with_past_inputs.push(format!("past_key_values.{layer}.encoder.{part}"));
                // The cached graph returns the self-attention cache and *not*
                // the cross-attention one: that is the whole shape of the
                // saving, and the discovery below refuses an export where it
                // is not true.
                with_past_outputs.push(format!("present.{layer}.decoder.{part}"));
            }
        }
        (
            first_inputs,
            first_outputs,
            with_past_inputs,
            with_past_outputs,
        )
    }

    fn shape_of(layers: usize) -> Result<CacheShape> {
        let (a, b, c, d) = pinned_names(layers);
        CacheShape::of(&a, &b, &c, &d)
    }

    /// The pinned export's eight layers split into a growing self-attention
    /// cache and a cross-attention one that is computed once.
    #[test]
    fn the_cache_splits_into_a_growing_half_and_a_fixed_half() {
        let shape = shape_of(8).expect("the pinned export's own names");
        assert_eq!(shape.layers(), 8);
        assert_eq!(shape.self_attention.len(), 16);
        assert_eq!(shape.cross_attention.len(), 16);
        assert_eq!(
            shape.self_attention[0],
            (
                "past_key_values.0.decoder.key".to_string(),
                "present.0.decoder.key".to_string()
            )
        );
        assert_eq!(
            shape.cross_attention[0],
            (
                "past_key_values.0.encoder.key".to_string(),
                "present.0.encoder.key".to_string()
            )
        );
        // Every past input is paired with the present output it is fed from,
        // and the two names differ only in their prefix. A pairing that drifted
        // would feed layer 3's keys into layer 4 and decode nonsense.
        for (past, present) in shape.self_attention.iter().chain(&shape.cross_attention) {
            let tail = past.strip_prefix(PAST_PREFIX).expect("a past input");
            assert_eq!(*present, format!("{PRESENT_PREFIX}{tail}"));
        }
    }

    /// The cross-attention cache is passed back unchanged, so an export that
    /// returned a fresh one every step would mean this loop was discarding it.
    #[test]
    fn a_cached_decoder_that_recomputes_the_encoder_cache_is_refused() {
        let (first_inputs, first_outputs, with_past_inputs, mut with_past_outputs) =
            pinned_names(2);
        with_past_outputs.push("present.0.encoder.key".to_string());
        let error = CacheShape::of(
            &first_inputs,
            &first_outputs,
            &with_past_inputs,
            &with_past_outputs,
        )
        .expect_err("the loop would be throwing that away");
        assert!(error.to_string().contains("unchanged"), "{error}");
    }

    /// A graph that still wants the encoder's states is the uncached one under
    /// another name, and running it would buy nothing.
    #[test]
    fn a_cached_decoder_that_still_wants_the_encoder_states_is_refused() {
        let (first_inputs, first_outputs, mut with_past_inputs, with_past_outputs) =
            pinned_names(2);
        with_past_inputs.push(ENCODER_HIDDEN_STATES.to_string());
        let error = CacheShape::of(
            &first_inputs,
            &first_outputs,
            &with_past_inputs,
            &with_past_outputs,
        )
        .expect_err("that is the graph it replaces");
        assert!(error.to_string().contains(ENCODER_HIDDEN_STATES), "{error}");
    }

    /// Step 0 is what fills the cache. A first-step graph that does not return
    /// a tensor the cached one wants would be discovered at step 1 of a
    /// document otherwise.
    #[test]
    fn a_first_step_decoder_that_does_not_fill_the_cache_is_refused() {
        let (first_inputs, mut first_outputs, with_past_inputs, with_past_outputs) =
            pinned_names(2);
        first_outputs.retain(|name| name != "present.1.encoder.value");
        let error = CacheShape::of(
            &first_inputs,
            &first_outputs,
            &with_past_inputs,
            &with_past_outputs,
        )
        .expect_err("nothing would fill that entry");
        assert!(
            error.to_string().contains("present.1.encoder.value"),
            "{error}"
        );
    }

    /// A self-attention cache that does not come back out does not grow, and a
    /// decode over a cache that does not grow reads the same token forever.
    #[test]
    fn a_cached_decoder_that_does_not_return_its_self_attention_cache_is_refused() {
        let (first_inputs, first_outputs, with_past_inputs, mut with_past_outputs) =
            pinned_names(2);
        with_past_outputs.retain(|name| name != "present.1.decoder.key");
        let error = CacheShape::of(
            &first_inputs,
            &first_outputs,
            &with_past_inputs,
            &with_past_outputs,
        )
        .expect_err("a cache that does not grow is not one");
        assert!(error.to_string().contains("does not grow"), "{error}");
    }

    /// A graph with no past at all is the uncached decoder, and this build has
    /// no loop that would run it: an error, never a slower decode nobody was
    /// told about.
    #[test]
    fn a_decoder_with_no_cache_at_all_is_refused_rather_than_run_uncached() {
        let (first_inputs, first_outputs, _, _) = pinned_names(8);
        let error = CacheShape::of(
            &first_inputs,
            &first_outputs,
            &[INPUT_IDS.to_string()],
            &[LOGITS.to_string()],
        )
        .expect_err("there is nothing to feed");
        assert!(error.to_string().contains(PAST_PREFIX), "{error}");
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

    #[test]
    fn the_most_likely_token_carries_the_share_of_the_distribution_it_held() {
        let (token, probability) = argmax_with_probability(&[0.0, 10.0, 0.0]);
        assert_eq!(token, 1);
        assert!(probability > 0.99, "{probability}");
        let (_, flat) = argmax_with_probability(&[1.0, 1.0, 1.0, 1.0]);
        assert!((flat - 0.25).abs() < 1e-5, "{flat}");
    }
}
