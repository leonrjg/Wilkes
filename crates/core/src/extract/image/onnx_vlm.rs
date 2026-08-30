//! Running an Optimum-exported vision-language model under ONNX Runtime.
//!
//! Three graphs, one contract. Optimum exports every VLM the same way — a
//! vision encoder, a token embedder, and a decoder carrying its own key/value
//! cache — so this module is written against that convention rather than
//! against granite-docling. What is model-specific (how pixels are prepared,
//! what the prompt says, how the answer is parsed) lives with the model; what
//! is exported-graph-specific lives here.
//!
//! The runner **discovers** its shape instead of declaring it. Layer count,
//! key/value head count, head width, and whether the decoder wants
//! `position_ids` are all read off the session's own input list at load. That
//! is not speculative generality: granite-docling's decoder takes no
//! `position_ids` and carries 30 layers, Qwen2-VL's takes one and carries 28,
//! and a runner that hardcodes either is a runner for exactly one checkpoint —
//! which is the thing addressing recognition by engine and model was meant to
//! stop.

use std::path::Path;

use anyhow::{Context, Result};
use ort::session::{Session, SessionInputValue};
use ort::value::{DynValue, Tensor, ValueType};

/// The names Optimum gives the three graphs' tensors. Constants because a
/// typo in a tensor name is a runtime error deep in a decode loop, and
/// because writing them once is how the shape below stays checkable.
const INPUT_IDS: &str = "input_ids";
const INPUTS_EMBEDS: &str = "inputs_embeds";
const ATTENTION_MASK: &str = "attention_mask";
const POSITION_IDS: &str = "position_ids";
const PIXEL_VALUES: &str = "pixel_values";
const PIXEL_ATTENTION_MASK: &str = "pixel_attention_mask";
const PAST_PREFIX: &str = "past_key_values.";
const PRESENT_PREFIX: &str = "present.";

/// What a decoder graph said it wants, read from the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderShape {
    /// How many `past_key_values.N.{key,value}` pairs the graph declares.
    pub layers: usize,
    /// The key/value head count, from the static dimension of a past input.
    pub kv_heads: i64,
    /// The width of one head, likewise.
    pub head_dim: i64,
    /// Whether the graph takes `position_ids`. granite-docling does not —
    /// it derives positions from the cache length — and Qwen2-VL does.
    pub wants_position_ids: bool,
}

impl DecoderShape {
    /// Read the shape off a loaded decoder session.
    ///
    /// Every fact here is a property of the exported graph, so asking the
    /// graph is the only answer that cannot drift from it.
    fn discover(session: &Session) -> Result<Self> {
        let mut layers = 0usize;
        let mut kv_heads = None;
        let mut head_dim = None;
        let mut wants_position_ids = false;

        for input in session.inputs() {
            let name = input.name();
            if name == POSITION_IDS {
                wants_position_ids = true;
            }
            if !name.starts_with(PAST_PREFIX) {
                continue;
            }
            if name.ends_with(".key") {
                layers += 1;
            }
            // [batch, kv_heads, past_len, head_dim]: the first and third are
            // symbolic, the second and fourth are the numbers wanted here.
            if let ValueType::Tensor { shape, .. } = input.dtype() {
                let dims: Vec<i64> = shape.iter().copied().collect();
                if dims.len() == 4 {
                    if dims[1] > 0 {
                        kv_heads.get_or_insert(dims[1]);
                    }
                    if dims[3] > 0 {
                        head_dim.get_or_insert(dims[3]);
                    }
                }
            }
        }

        anyhow::ensure!(
            layers > 0,
            "the decoder graph declares no past_key_values inputs; this is not a \
             merged-decoder export"
        );
        Ok(Self {
            layers,
            kv_heads: kv_heads
                .context("the decoder's past_key_values inputs name no head count")?,
            head_dim: head_dim
                .context("the decoder's past_key_values inputs name no head width")?,
            wants_position_ids,
        })
    }
}

/// One loaded model: the three graphs plus the shape discovered from them.
pub struct OnnxVlm {
    vision: Session,
    embed: Session,
    decoder: Session,
    shape: DecoderShape,
    hidden_size: usize,
}

/// One decoded token and how sure the decoder was of it.
#[derive(Debug, Clone, Copy)]
pub struct DecodedToken {
    pub id: u32,
    /// Log-probability of the chosen token under the step's own distribution.
    pub logprob: f32,
}

impl OnnxVlm {
    /// Load the three graphs from a directory, by file name.
    ///
    /// By path and never from memory: the weights live in `.onnx_data`
    /// sidecars that ONNX Runtime resolves relative to the graph file, and a
    /// session committed from a buffer cannot find them.
    pub fn load(
        dir: &Path,
        vision_file: &str,
        embed_file: &str,
        decoder_file: &str,
        threads: usize,
    ) -> Result<Self> {
        let open = |file: &str| -> Result<Session> {
            let path = dir.join(file);
            anyhow::ensure!(path.is_file(), "{} is missing from {}", file, dir.display());
            let mut builder = Session::builder()
                .map_err(|e| anyhow::anyhow!("could not start an ONNX session builder: {e}"))?;
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| anyhow::anyhow!("could not set the session thread count: {e}"))?;
            builder
                .commit_from_file(&path)
                .map_err(|e| anyhow::anyhow!("could not load {}: {e}", path.display()))
        };

        let vision = open(vision_file)?;
        let embed = open(embed_file)?;
        let decoder = open(decoder_file)?;
        let shape = DecoderShape::discover(&decoder)?;

        // The embedding width is the decoder's declared `inputs_embeds` last
        // dimension. Asked rather than assumed, for the same reason as above.
        let hidden_size = decoder
            .inputs()
            .iter()
            .find(|i| i.name() == INPUTS_EMBEDS)
            .and_then(|i| match i.dtype() {
                ValueType::Tensor { shape, .. } => shape.iter().copied().last(),
                _ => None,
            })
            .filter(|d| *d > 0)
            .context("the decoder graph does not declare an inputs_embeds width")?
            as usize;

        tracing::info!(
            layers = shape.layers,
            kv_heads = shape.kv_heads,
            head_dim = shape.head_dim,
            position_ids = shape.wants_position_ids,
            hidden_size,
            "loaded an ONNX vision-language model"
        );
        Ok(Self {
            vision,
            embed,
            decoder,
            shape,
            hidden_size,
        })
    }

    pub fn shape(&self) -> DecoderShape {
        self.shape
    }

    pub fn hidden_size(&self) -> usize {
        self.hidden_size
    }

    /// Encode prepared tiles into visual embeddings.
    ///
    /// `pixels` is `[1, tiles, 3, side, side]` flattened in that order, which
    /// is the rank the exported graph declares — the tile axis is the graph's,
    /// not a batch this code folded away.
    pub fn encode_image(&mut self, pixels: &[f32], tiles: usize, side: usize) -> Result<Vec<f32>> {
        let expected = tiles * 3 * side * side;
        anyhow::ensure!(
            pixels.len() == expected,
            "expected {expected} pixel values for {tiles} {side}x{side} tiles, got {}",
            pixels.len()
        );
        let values = Tensor::from_array((
            vec![1i64, tiles as i64, 3, side as i64, side as i64],
            pixels.to_vec(),
        ))
        .context("could not build the pixel tensor")?;
        // The mask is bool in this export, and every prepared tile is fully
        // valid: Wilkes resizes to whole tiles rather than padding to them.
        let mask = Tensor::from_array((
            vec![1i64, tiles as i64, side as i64, side as i64],
            vec![true; tiles * side * side],
        ))
        .context("could not build the pixel attention mask")?;

        let outputs = self
            .vision
            .run(ort::inputs![PIXEL_VALUES => values, PIXEL_ATTENTION_MASK => mask])
            .context("the vision encoder failed")?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("the vision encoder returned a tensor this build cannot read")?;
        Ok(data.to_vec())
    }

    /// Look up token embeddings.
    pub fn embed_tokens(&mut self, ids: &[u32]) -> Result<Vec<f32>> {
        let ids64: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
        let tensor = Tensor::from_array((vec![1i64, ids64.len() as i64], ids64))
            .context("could not build the token id tensor")?;
        let outputs = self
            .embed
            .run(ort::inputs![INPUT_IDS => tensor])
            .context("the token embedder failed")?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .context("the token embedder returned a tensor this build cannot read")?;
        Ok(data.to_vec())
    }

    /// Greedily decode until `stop` says so or `max_tokens` is reached.
    ///
    /// The loop lives here rather than in the caller because the key/value
    /// cache is the loop's own state: threading sixty tensors back into the
    /// next step is not something a caller should have to know about.
    ///
    /// `logits_to_token` is greedy, and the log-probability of the chosen
    /// token comes back with it. That number is the admission signal, and it
    /// only exists if the decode keeps the logits — which is why this is a
    /// hand-rolled loop and not a generate call.
    pub fn decode(
        &mut self,
        prompt_embeds: &[f32],
        max_tokens: usize,
        mut stop: impl FnMut(u32) -> bool,
        mut on_token: impl FnMut(usize),
    ) -> Result<Vec<DecodedToken>> {
        let hidden = self.hidden_size;
        anyhow::ensure!(
            prompt_embeds.len() % hidden == 0,
            "prompt embeddings are not a whole number of {hidden}-wide rows"
        );
        let prompt_len = prompt_embeds.len() / hidden;

        // The cache is carried as the decoder's own output values, moved
        // straight back into the next step's inputs. Reading them into Rust
        // and rebuilding tensors would copy the whole, growing cache twice per
        // token, which is quadratic in the number of tokens produced; ORT
        // values are reference-counted handles, so moving them is free.
        let names: Vec<(String, String)> = (0..self.shape.layers)
            .flat_map(|layer| {
                ["key", "value"].map(|part| {
                    (
                        format!("{PAST_PREFIX}{layer}.{part}"),
                        format!("{PRESENT_PREFIX}{layer}.{part}"),
                    )
                })
            })
            .collect();

        let empty = || -> Result<DynValue> {
            Ok(Tensor::from_array((
                vec![1i64, self.shape.kv_heads, 0, self.shape.head_dim],
                Vec::<f32>::new(),
            ))
            .context("could not build an empty key/value cache entry")?
            .into_dyn())
        };
        let mut cache: Vec<DynValue> = names.iter().map(|_| empty()).collect::<Result<Vec<_>>>()?;

        let mut current: Vec<f32> = prompt_embeds.to_vec();
        let mut current_len = prompt_len;
        let mut total_len = prompt_len;
        let mut produced: Vec<DecodedToken> = Vec::new();

        for step in 0..max_tokens {
            let mut feed: Vec<(std::borrow::Cow<'_, str>, SessionInputValue<'_>)> = Vec::new();

            let embeds = Tensor::from_array((
                vec![1i64, current_len as i64, hidden as i64],
                std::mem::take(&mut current),
            ))
            .context("could not build the decoder input embeddings")?;
            feed.push((INPUTS_EMBEDS.into(), SessionInputValue::from(embeds)));

            let mask = Tensor::from_array((vec![1i64, total_len as i64], vec![1i64; total_len]))
                .context("could not build the attention mask")?;
            feed.push((ATTENTION_MASK.into(), SessionInputValue::from(mask)));

            if self.shape.wants_position_ids {
                // Positions continue from what the cache already holds.
                let start = total_len - current_len;
                let ids: Vec<i64> = (start..total_len).map(|p| p as i64).collect();
                let tensor = Tensor::from_array((vec![1i64, current_len as i64], ids))
                    .context("could not build position ids")?;
                feed.push((POSITION_IDS.into(), SessionInputValue::from(tensor)));
            }

            for ((past, _), value) in names.iter().zip(cache.drain(..)) {
                feed.push((past.clone().into(), SessionInputValue::from(value)));
            }

            let mut outputs = self
                .decoder
                .run(feed)
                .with_context(|| format!("the decoder failed at step {step}"))?;

            let (id, logprob) = {
                let (logits_shape, logits) = outputs["logits"]
                    .try_extract_tensor::<f32>()
                    .context("the decoder returned logits this build cannot read")?;
                let vocab = *logits_shape
                    .last()
                    .context("the decoder's logits have no vocabulary dimension")?
                    as usize;
                anyhow::ensure!(vocab > 0, "the decoder reported an empty vocabulary");
                if step == 0 {
                    // Whether prefill projects every position into the whole
                    // vocabulary, or only the last one, is a property of the
                    // export and decides how much of this run is discarded.
                    tracing::debug!(
                        prefill_positions = current_len,
                        logits_shape = ?logits_shape,
                        logits_values = logits.len(),
                        "decoder prefill logits"
                    );
                }
                greedy(&logits[logits.len() - vocab..])
            };
            produced.push(DecodedToken { id, logprob });
            on_token(produced.len());

            // Carry `present` back into `past` before the outputs are dropped.
            for (past, present) in &names {
                cache.push(
                    outputs.remove(present.as_str()).with_context(|| {
                        format!("the decoder did not return {present} for {past}")
                    })?,
                );
            }
            drop(outputs);

            if stop(id) {
                break;
            }

            current = self.embed_tokens(&[id])?;
            current_len = 1;
            total_len += 1;
        }

        Ok(produced)
    }
}

/// The most likely token and its log-probability, by a numerically stable
/// log-sum-exp. Stable because the alternative silently produces `-inf` or
/// `NaN` for a confident decode, and the admission rule reads this number.
fn greedy(logits: &[f32]) -> (u32, f32) {
    let mut best = 0usize;
    let mut max = f32::NEG_INFINITY;
    for (i, value) in logits.iter().enumerate() {
        if *value > max {
            max = *value;
            best = i;
        }
    }
    let sum: f32 = logits.iter().map(|v| (v - max).exp()).sum();
    (best as u32, logits[best] - (max + sum.ln()))
}

/// Replace the embedding rows at `slots` with `features`, in order.
///
/// The splice is the caller's because which token id marks an image slot is
/// the model's business, not the runtime's. What is checked here is the thing
/// that silently produces nonsense when it is wrong: a slot count that does
/// not match the feature count means the tiling and the prompt disagree, and
/// the decode would run to completion on a corrupted prefix.
pub fn splice_image_features(
    embeds: &mut [f32],
    hidden: usize,
    slots: &[usize],
    features: &[f32],
) -> Result<()> {
    anyhow::ensure!(
        features.len() == slots.len() * hidden,
        "the prompt has {} image slots but the encoder produced {} embeddings",
        slots.len(),
        features.len() / hidden.max(1)
    );
    for (nth, slot) in slots.iter().enumerate() {
        let to = slot * hidden;
        anyhow::ensure!(
            to + hidden <= embeds.len(),
            "image slot {slot} is past the end of the prompt embeddings"
        );
        embeds[to..to + hidden].copy_from_slice(&features[nth * hidden..(nth + 1) * hidden]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_picks_the_largest_and_scores_it_as_a_log_probability() {
        let (id, logprob) = greedy(&[0.0, 5.0, 1.0]);
        assert_eq!(id, 1);
        // 5 - ln(e^0 + e^5 + e^1) = -0.024745
        assert!((logprob - -0.024745).abs() < 1e-5, "got {logprob}");
        assert!(logprob < 0.0, "a log-probability is never positive");
    }

    /// A one-hot decode must not come back as -inf: the admission rule
    /// averages these, and one -inf would reject a whole region.
    #[test]
    fn a_confident_decode_scores_near_zero_rather_than_negative_infinity() {
        let (_, logprob) = greedy(&[100.0, -100.0, -100.0]);
        assert!(logprob.is_finite(), "got {logprob}");
        assert!(logprob > -1e-6, "got {logprob}");
    }

    #[test]
    fn splicing_rejects_a_slot_count_that_disagrees_with_the_encoder() {
        let mut embeds = vec![0.0f32; 8];
        let err = splice_image_features(&mut embeds, 2, &[0, 1], &[1.0, 2.0])
            .expect_err("two slots need two rows, not one");
        assert!(err.to_string().contains("image slots"), "{err}");
    }

    #[test]
    fn splicing_writes_each_feature_row_into_its_own_slot() {
        let mut embeds = vec![0.0f32; 8];
        splice_image_features(&mut embeds, 2, &[1, 3], &[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(embeds, vec![0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 3.0, 4.0]);
    }
}
