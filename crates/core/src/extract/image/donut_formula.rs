//! The Donut-encoder / mBART-decoder formula readers, as one runner.
//!
//! Two checkpoints in this tree are the same machine: a Donut Swin vision
//! encoder over an mBART decoder, exported by Optimum into the three graphs an
//! encoder-decoder needs — the encoder, the decoder that takes no cache, and
//! the decoder that takes one. [`super::texify`] is one and
//! [`super::unimernet`] is the other. What differs between them is the frame a
//! crop is fitted into, how it is normalized, where the graphs are, and what
//! the reading is called; everything else — the cache discovery, the decode
//! loop, the reader pool, the region it emits — is the same code, and it is
//! here rather than written twice.
//!
//! That split is the whole point of [`Checkpoint`]. A second copy of the
//! decode loop is not a second recognizer, it is a second place for the
//! cross-attention cache to be recomputed by accident; the measurements in
//! [`super::texify`] were made against *this* loop and would stop describing
//! anything if a checkpoint quietly ran its own.
//!
//! This module loads weights, so it runs in the recognition worker and nowhere
//! else — see [`super::dispatch`]'s invariant. Nothing here is reachable from
//! the host except through [`super::worker_ocr::attach`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use image::RgbImage;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::{DynValue, Tensor};
use tokenizers::Tokenizer;
use tracing::{debug, warn};

use super::ocr::{ImageRecognition, OcrEngine, RegionKind, SpottedRegion};
use crate::types::Point;

/// One export of the family: what the runner has to be told to read with it.
///
/// A `&'static` table of facts rather than a trait, for the same reason
/// [`super::paddleocr_vl::Checkpoint`] is one: the host answers `identity` and
/// `admission_threshold` with no weights anywhere near it, and a trait object
/// would tie those answers to a loaded model. Every field is something the
/// export itself decides — none of them is a setting.
pub struct Checkpoint {
    /// The catalogue's id for this reading. Appears in the log lines a decode
    /// writes, so a worker running two of these says which one it is.
    pub model_id: &'static str,
    /// The three graphs, relative to the recognizer's install directory.
    pub encoder_graph: &'static str,
    /// Step 0's decoder: no cache in, the whole `present.*` cache out.
    pub decoder_graph: &'static str,
    /// Every later step's decoder: one token in, the cache in and back out.
    pub decoder_with_past_graph: &'static str,
    /// The vocabulary file, relative to the same directory.
    pub tokenizer: &'static str,
    /// `pixel_values` after the batch axis: channels, height, width. Checked
    /// against what [`Self::preprocess`] returns before it is fed, because a
    /// tensor of the right length and the wrong shape is a silent misread
    /// rather than an error.
    pub pixels: (usize, usize, usize),
    /// Fit a crop into [`Self::pixels`] and normalize it the way this
    /// checkpoint was trained. Channel-planar, `channels * height * width`
    /// long. A function rather than a set of constants because the two
    /// checkpoints do not merely differ in their statistics — one letterboxes
    /// onto paper and the other crops to the ink and pads with black.
    pub preprocess: fn(&RgbImage) -> Result<Vec<f32>>,
    /// The recipe string a reading produced by this checkpoint is stored
    /// under, answerable with no weights loaded.
    pub identity: fn() -> String,
    /// Declared because [`OcrEngine`] asks every engine for one. Both
    /// checkpoints here declare zero: a formula is admitted on whether its
    /// LaTeX parses, never on a score.
    pub admission_threshold: f32,
    /// The token a decode is started from, from the export's own
    /// `generation_config.json`.
    pub start_token: i64,
    /// The token that ends one.
    pub eos_token: i64,
    /// A decode that has not stopped by here has stopped saying anything.
    pub max_new_tokens: usize,
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
///
/// Shared by both checkpoints and not a [`Checkpoint`] field, because they are
/// not this project's choice: they are what the exporter writes, and an export
/// that spelled them differently would not be one this loop could drive.
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
/// checkpoint — the argument [`super::onnx_vlm::DecoderShape`] already makes,
/// and the reason this module drives two exports without being told the
/// difference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheShape {
    /// `(past_key_values.N.decoder.*, present.N.decoder.*)`.
    ///
    /// One position longer at every step, so it comes back out of every run
    /// and goes straight back in.
    pub self_attention: Vec<(String, String)>,
    /// `(past_key_values.N.encoder.*, present.N.encoder.*)`.
    ///
    /// The encoder's positions projected into each layer's keys and values.
    /// They do not depend on the tokens decoded so far, so step 0 computes
    /// them once and every later step is handed the same tensors back
    /// unchanged — the cached graph does not even return them. Not
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
    /// `pub` so a probe that discovered the cache by its own rules would be
    /// measuring a decode nobody runs. Pure, and holds no state.
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
    /// rule — rather than only against hundreds of megabytes of weights a unit
    /// test cannot load.
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

/// A loaded checkpoint of the family, and the readers it is spent through.
pub struct DonutFormula {
    /// Which export this is. Everything that differs between the two
    /// recognizers this struct serves is in here, and nothing that differs is
    /// anywhere else.
    checkpoint: &'static Checkpoint,
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
    /// not pay a reader's footprint for it, and a document with one crop in it
    /// must not pay for three.
    readers: Vec<Mutex<Option<Reader>>>,
    tokenizer: Tokenizer,
}

impl DonutFormula {
    /// Address `readers` copies of `checkpoint` under `dir`, each to be given
    /// `threads` threads when it is first needed.
    ///
    /// The two numbers are the caller's because they describe the machine
    /// rather than the model, exactly as they are for
    /// [`super::granite_docling::GraniteDocling::load`]; the policy and the
    /// measurements behind it live in [`super::dispatch`]. Whether the
    /// artifacts under `dir` are the pinned ones is the calling module's
    /// question, because the calling module is the one that pinned them.
    pub fn load(
        checkpoint: &'static Checkpoint,
        dir: &Path,
        readers: usize,
        threads: usize,
    ) -> Result<Self> {
        anyhow::ensure!(readers > 0, "a recognizer needs at least one reader");
        anyhow::ensure!(threads > 0, "a reader needs at least one thread");
        let tokenizer = Tokenizer::from_file(dir.join(checkpoint.tokenizer))
            .map_err(|e| anyhow::anyhow!("could not read the tokenizer: {e}"))?;
        Ok(Self {
            checkpoint,
            dir: dir.to_path_buf(),
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
        let encoder = self.session(self.checkpoint.encoder_graph)?;
        let decoder = self.session(self.checkpoint.decoder_graph)?;
        let decoder_with_past = self.session(self.checkpoint.decoder_with_past_graph)?;
        let cache = CacheShape::discover(&decoder, &decoder_with_past)?;
        debug!(
            model = self.checkpoint.model_id,
            layers = cache.layers(),
            self_attention = cache.self_attention.len(),
            cross_attention = cache.cross_attention.len(),
            "loaded the decoder pair"
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

    /// The crop as this checkpoint's `pixel_values`.
    ///
    /// The shape is asserted against what the checkpoint declared rather than
    /// derived from what came back: a preprocessing function that returned the
    /// right number of floats in the wrong arrangement would otherwise be a
    /// reading that is quietly wrong for every crop, which is the one failure
    /// mode a recognizer cannot report on itself.
    fn pixel_values(&self, crop: &RgbImage) -> Result<Tensor<f32>> {
        let (channels, height, width) = self.checkpoint.pixels;
        let pixels = (self.checkpoint.preprocess)(crop)?;
        anyhow::ensure!(
            pixels.len() == channels * height * width,
            "{} preprocessed a crop to {} floats, {} expected",
            self.checkpoint.model_id,
            pixels.len(),
            channels * height * width
        );
        Ok(Tensor::from_array((
            vec![1i64, channels as i64, height as i64, width as i64],
            pixels,
        ))?)
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
    /// 1292 ms a crop became 4.35 ms a step and 299 ms a crop. Measured on
    /// [`super::texify`], whose documentation carries the table; the loop is
    /// the same one for either checkpoint, which is why it is measured once.
    fn read(&self, reader: &mut Reader, crop: &RgbImage) -> Result<(String, f32, bool)> {
        let encoded = reader
            .encoder
            .run(vec![(
                "pixel_values".to_string(),
                SessionInputValue::from(self.pixel_values(crop)?),
            )])
            .map_err(|e| anyhow::anyhow!("the vision encoder failed: {e}"))?;
        let (shape, hidden) = encoded[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("the vision encoder returned nothing usable: {e}"))?;
        let shape: Vec<i64> = shape.to_vec();
        let hidden: Vec<f32> = hidden.to_vec();
        drop(encoded);

        let mut ids: Vec<i64> = vec![self.checkpoint.start_token];
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
        let mut feed_token = self.checkpoint.start_token;
        for step in 0..self.checkpoint.max_new_tokens {
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
            if next == self.checkpoint.eos_token {
                hit_the_cap = false;
                break;
            }
            ids.push(next);
            feed_token = next;
        }
        if hit_the_cap {
            warn!(
                "the {} decode hit its {}-token cap; this crop is partial",
                self.checkpoint.model_id, self.checkpoint.max_new_tokens
            );
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

impl OcrEngine for DonutFormula {
    fn identity(&self) -> String {
        (self.checkpoint.identity)()
    }

    fn admission_threshold(&self) -> f32 {
        self.checkpoint.admission_threshold
    }

    /// One region per crop, covering the whole of it.
    ///
    /// Neither checkpoint delimits: each is given an expression and returns
    /// that expression's LaTeX, so the region *is* the image and inventing a
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
                "read formula {} of {} with {}: {text:?}",
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
                images.len(),
                self.checkpoint.model_id
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
            debug!(
                "{} released {dropped} reader(s); the next crop reloads them",
                self.checkpoint.model_id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── The cached decoder ───────────────────────────────────────────────────

    /// The names the pinned exports declare, as `ort` reports them. Written
    /// out rather than read from the graphs so the rules below are testable on
    /// a machine that has never downloaded the weights; the graphs themselves
    /// are what [`CacheShape::discover`] reads at load, and the probe's
    /// `equivalence` mode is what checks these against them.
    ///
    /// Both checkpoints declare exactly these names — they are the exporter's
    /// convention rather than either model's — which is why one fixture covers
    /// both and why the runner needs no per-checkpoint list.
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

    /// Both pinned exports carry eight layers, split into a growing
    /// self-attention cache and a cross-attention one that is computed once.
    #[test]
    fn the_cache_splits_into_a_growing_half_and_a_fixed_half() {
        let shape = shape_of(8).expect("the pinned exports' own names");
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

    #[test]
    fn the_most_likely_token_carries_the_share_of_the_distribution_it_held() {
        let (token, probability) = argmax_with_probability(&[0.0, 10.0, 0.0]);
        assert_eq!(token, 1);
        assert!(probability > 0.99, "{probability}");
        let (_, flat) = argmax_with_probability(&[1.0, 1.0, 1.0, 1.0]);
        assert!((flat - 0.25).abs() < 1e-5, "{flat}");
    }

    /// Every checkpoint this runner serves declares a frame its preprocessing
    /// fills exactly, a vocabulary file, three graphs, and a cap. The runner
    /// checks the length at every crop; this checks that the two checkpoints
    /// do not merely agree with themselves but differ from each other, because
    /// two rows of a catalogue that named one graph would read a library twice
    /// under one recipe.
    #[test]
    fn the_two_checkpoints_are_two_and_each_is_complete() {
        let checkpoints = [
            &super::super::texify::CHECKPOINT,
            &super::super::unimernet::CHECKPOINT,
        ];
        for checkpoint in checkpoints {
            let (channels, height, width) = checkpoint.pixels;
            assert!(channels > 0 && height > 0 && width > 0);
            assert!(!checkpoint.model_id.is_empty());
            assert!(!checkpoint.tokenizer.is_empty());
            assert!(checkpoint.max_new_tokens > 0);
            assert_ne!(checkpoint.start_token, checkpoint.eos_token);
            // A formula is admitted on whether its LaTeX parses, never on a
            // score; a checkpoint that declared one would be declaring a
            // number nothing consults.
            assert_eq!(checkpoint.admission_threshold, 0.0);
            // What the runner asserts per crop, asserted once here against an
            // image of a size neither checkpoint's frame is.
            let crop = RgbImage::from_pixel(37, 11, image::Rgb([90, 90, 90]));
            assert_eq!(
                (checkpoint.preprocess)(&crop).unwrap().len(),
                channels * height * width,
                "{} fills its own frame",
                checkpoint.model_id
            );
            assert!((checkpoint.identity)().contains(checkpoint.model_id));
        }
        assert_ne!(checkpoints[0].model_id, checkpoints[1].model_id);
        assert_ne!((checkpoints[0].identity)(), (checkpoints[1].identity)());
        assert_ne!(checkpoints[0].pixels, checkpoints[1].pixels);
    }
}
