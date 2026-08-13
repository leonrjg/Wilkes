// Derived from candle-transformers 0.11.0's Gemma 3 implementation.
// Candle is licensed under MIT OR Apache-2.0. This local decoder owns the
// bounded-prefill cache/mask correction required by Wilkes' Metal runtime.
//! Gemma LLM architecture (Google) inference implementation.
//!
//! See ["Introducing Gemma 3: The most capable model you can run on a single GPU or TPU"](https://blog.google/technology/developers/gemma-3/)
//!
//! Based on implementations from HuggingFace transformers.

use std::sync::Arc;

use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::{linear_b as linear, Activation, Linear, VarBuilder};
use candle_transformers::utils::repeat_kv;

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Config {
    pub attention_bias: bool,
    pub head_dim: usize,
    pub hidden_activation: Activation,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_attention_heads: usize,
    pub num_hidden_layers: usize,
    pub num_key_value_heads: usize,
    pub rms_norm_eps: f64,
    pub rope_theta: f64,
    pub rope_local_base_freq: f64,
    pub vocab_size: usize,
    pub final_logit_softcapping: Option<f64>,
    pub attn_logit_softcapping: Option<f64>,
    #[serde(rename = "query_pre_attn_scalar")]
    _query_pre_attn_scalar: usize,
    pub sliding_window: usize,
    pub sliding_window_pattern: usize,
    pub max_position_embeddings: usize,
}

#[derive(Debug, Clone)]
struct RmsNorm {
    weight: Tensor,
    eps: f64,
}

impl RmsNorm {
    fn new(dim: usize, eps: f64, vb: VarBuilder) -> Result<Self> {
        let weight = vb.get(dim, "weight")?;
        Ok(Self { weight, eps })
    }
}

impl Module for RmsNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x_dtype = x.dtype();
        let internal_dtype = match x_dtype {
            DType::F16 | DType::BF16 => DType::F32,
            d => d,
        };
        let hidden_size = x.dim(D::Minus1)?;
        let x = x.to_dtype(internal_dtype)?;
        let norm_x = (x.sqr()?.sum_keepdim(D::Minus1)? / hidden_size as f64)?;
        let x_normed = x.broadcast_div(&(norm_x + self.eps)?.sqrt()?)?;
        x_normed
            .to_dtype(x_dtype)?
            .broadcast_mul(&(&self.weight + 1.0)?)
    }
}

#[derive(Debug, Clone)]
struct RotaryEmbedding {
    sin: Tensor,
    cos: Tensor,
}

impl RotaryEmbedding {
    fn new(
        dtype: DType,
        cfg: &Config,
        dev: &Device,
        sliding_window: Option<usize>,
    ) -> Result<Self> {
        let dim = cfg.head_dim;
        let max_seq_len = cfg.max_position_embeddings;
        let rope_freq = if sliding_window.is_some() {
            cfg.rope_local_base_freq
        } else {
            cfg.rope_theta
        };
        let inv_freq: Vec<_> = (0..dim)
            .step_by(2)
            .map(|i| 1f32 / rope_freq.powf(i as f64 / dim as f64) as f32)
            .collect();
        let inv_freq_len = inv_freq.len();
        // Absolute position ids must remain in FP32 while frequencies are
        // calculated. At position 16k, FP16 can represent only every sixteenth
        // integer, which aliases adjacent tokens onto the same rotary phase.
        // Cast the completed table to the model dtype, matching Gemma's
        // reference implementation.
        let inv_freq = Tensor::from_vec(inv_freq, (1, inv_freq_len), dev)?;
        let t = Tensor::arange(0u32, max_seq_len as u32, dev)?
            .to_dtype(DType::F32)?
            .reshape((max_seq_len, 1))?;
        let freqs = t.matmul(&inv_freq)?;
        Ok(Self {
            sin: freqs.sin()?.to_dtype(dtype)?,
            cos: freqs.cos()?.to_dtype(dtype)?,
        })
    }

    fn apply_rotary_emb_qkv(
        &self,
        q: &Tensor,
        k: &Tensor,
        seqlen_offset: usize,
    ) -> Result<(Tensor, Tensor)> {
        let (_b_sz, _h, seq_len, _n_embd) = q.dims4()?;
        let cos = self.cos.narrow(0, seqlen_offset, seq_len)?;
        let sin = self.sin.narrow(0, seqlen_offset, seq_len)?;
        let q_embed = candle_nn::rotary_emb::rope(&q.contiguous()?, &cos, &sin)?;
        let k_embed = candle_nn::rotary_emb::rope(&k.contiguous()?, &cos, &sin)?;
        Ok((q_embed, k_embed))
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::upper_case_acronyms)]
struct MLP {
    gate_proj: Linear,
    up_proj: Linear,
    down_proj: Linear,
    act_fn: candle_nn::Activation,
}

impl MLP {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let hidden_sz = cfg.hidden_size;
        let intermediate_sz = cfg.intermediate_size;
        let gate_proj = linear(hidden_sz, intermediate_sz, false, vb.pp("gate_proj"))?;
        let up_proj = linear(hidden_sz, intermediate_sz, false, vb.pp("up_proj"))?;
        let down_proj = linear(intermediate_sz, hidden_sz, false, vb.pp("down_proj"))?;
        Ok(Self {
            gate_proj,
            up_proj,
            down_proj,
            act_fn: cfg.hidden_activation,
        })
    }
}

impl Module for MLP {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let lhs = xs.apply(&self.gate_proj)?.apply(&self.act_fn)?;
        let rhs = xs.apply(&self.up_proj)?;
        (lhs * rhs)?.apply(&self.down_proj)
    }
}

#[derive(Debug, Clone)]
enum KvCache {
    Normal(candle_nn::kv_cache::KvCache),
    Rotating(candle_nn::kv_cache::RotatingKvCache),
}

#[derive(Debug, Clone)]
struct Attention {
    q_proj: Linear,
    k_proj: Linear,
    v_proj: Linear,
    o_proj: Linear,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    num_heads: usize,
    num_kv_heads: usize,
    num_kv_groups: usize,
    head_dim: usize,
    attn_logit_softcapping: Option<f64>,
    rotary_emb: Arc<RotaryEmbedding>,
    kv_cache: KvCache,
    use_flash_attn: bool,
}

impl Attention {
    fn new(
        rotary_emb: Arc<RotaryEmbedding>,
        use_flash_attn: bool,
        cfg: &Config,
        sliding_window: Option<usize>,
        vb: VarBuilder,
    ) -> Result<Self> {
        let hidden_sz = cfg.hidden_size;
        let num_heads = cfg.num_attention_heads;
        let num_kv_heads = cfg.num_key_value_heads;
        let num_kv_groups = num_heads / num_kv_heads;
        let head_dim = cfg.head_dim;
        let bias = cfg.attention_bias;
        let q_proj = linear(hidden_sz, num_heads * head_dim, bias, vb.pp("q_proj"))?;
        let k_proj = linear(hidden_sz, num_kv_heads * head_dim, bias, vb.pp("k_proj"))?;
        let v_proj = linear(hidden_sz, num_kv_heads * head_dim, bias, vb.pp("v_proj"))?;
        let o_proj = linear(num_heads * head_dim, hidden_sz, bias, vb.pp("o_proj"))?;
        let q_norm = RmsNorm::new(head_dim, cfg.rms_norm_eps, vb.pp("q_norm"))?;
        let k_norm = RmsNorm::new(head_dim, cfg.rms_norm_eps, vb.pp("k_norm"))?;
        let kv_cache = match sliding_window {
            Some(sliding_window) => {
                // Candle's original Gemma mask admits the current token plus
                // `sliding_window` preceding tokens. Retaining that extra slot
                // preserves the same boundary while bounding local KV memory.
                let cache_len = sliding_window.checked_add(1).ok_or_else(|| {
                    candle_core::Error::Msg("Gemma sliding window exceeds usize".to_string())
                })?;
                KvCache::Rotating(candle_nn::kv_cache::RotatingKvCache::new(2, cache_len))
            }
            None => KvCache::Normal(candle_nn::kv_cache::KvCache::new(
                2,
                cfg.max_position_embeddings,
            )),
        };
        Ok(Self {
            q_proj,
            k_proj,
            v_proj,
            o_proj,
            q_norm,
            k_norm,
            num_heads,
            num_kv_heads,
            num_kv_groups,
            head_dim,
            attn_logit_softcapping: cfg.attn_logit_softcapping,
            rotary_emb,
            kv_cache,
            use_flash_attn,
        })
    }

    fn forward(
        &mut self,
        xs: &Tensor,
        attention_mask: Option<&Tensor>,
        seqlen_offset: usize,
    ) -> Result<Tensor> {
        let (b_sz, q_len, _) = xs.dims3()?;

        let query_states = self.q_proj.forward(xs)?;
        let key_states = self.k_proj.forward(xs)?;
        let value_states = self.v_proj.forward(xs)?;

        let query_states = query_states
            .reshape((b_sz, q_len, self.num_heads, self.head_dim))?
            .transpose(1, 2)?;
        let key_states = key_states
            .reshape((b_sz, q_len, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let value_states = value_states
            .reshape((b_sz, q_len, self.num_kv_heads, self.head_dim))?
            .transpose(1, 2)?;
        let query_states = self.q_norm.forward(&query_states)?;
        let key_states = self.k_norm.forward(&key_states)?;

        let (query_states, key_states) =
            self.rotary_emb
                .apply_rotary_emb_qkv(&query_states, &key_states, seqlen_offset)?;

        // The rotating cache owns the local mask because its returned keys are
        // in physical ring-buffer order rather than absolute position order.
        let local_attention_mask = match &self.kv_cache {
            KvCache::Normal(_) => None,
            KvCache::Rotating(cache) => cache
                .attn_mask(q_len, xs.device())?
                .map(|mask| additive_attention_mask(&mask, query_states.dtype()))
                .transpose()?,
        };

        let (key_states, value_states) = match &mut self.kv_cache {
            KvCache::Normal(cache) => cache.append(&key_states, &value_states)?,
            KvCache::Rotating(cache) => cache.append(&key_states, &value_states)?,
        };

        let key_states = repeat_kv(key_states, self.num_kv_groups)?.contiguous()?;
        let value_states = repeat_kv(value_states, self.num_kv_groups)?.contiguous()?;

        let attn_output = if self.use_flash_attn {
            // flash-attn expects (b_sz, seq_len, nheads, head_dim)
            let q = query_states.transpose(1, 2)?;
            let k = key_states.transpose(1, 2)?;
            let v = value_states.transpose(1, 2)?;
            let scale = 1f32 / (self.head_dim as f32).sqrt();
            flash_attn(&q, &k, &v, scale, attention_mask.is_some())?.transpose(1, 2)?
        } else {
            let scale = 1f64 / f64::sqrt(self.head_dim as f64);
            let attn_weights = (query_states.matmul(&key_states.transpose(2, 3)?)? * scale)?;

            let attn_weights = match self.attn_logit_softcapping {
                None => attn_weights,
                Some(sc) => ((attn_weights / sc)?.tanh()? * sc)?,
            };

            let attention_mask = local_attention_mask.as_ref().or(attention_mask);
            let attn_weights = match attention_mask {
                None => attn_weights,
                Some(mask) => attn_weights.broadcast_add(mask)?,
            };
            let attn_weights = candle_nn::ops::softmax_last_dim(&attn_weights)?;
            attn_weights.matmul(&value_states)?
        };
        attn_output
            .transpose(1, 2)?
            .reshape((b_sz, q_len, ()))?
            .apply(&self.o_proj)
    }

    fn clear_kv_cache(&mut self) {
        match &mut self.kv_cache {
            KvCache::Normal(c) => c.reset(),
            KvCache::Rotating(c) => c.reset(),
        }
    }
}

fn additive_attention_mask(mask: &Tensor, dtype: DType) -> Result<Tensor> {
    let shape = mask.shape();
    let unmasked = Tensor::zeros(shape, dtype, mask.device())?;
    let masked = Tensor::new(f32::NEG_INFINITY, mask.device())?
        .to_dtype(dtype)?
        .broadcast_as(shape.dims())?;
    mask.where_cond(&masked, &unmasked)?
        .unsqueeze(0)?
        .unsqueeze(0)
}

fn flash_attn(_: &Tensor, _: &Tensor, _: &Tensor, _: f32, _: bool) -> Result<Tensor> {
    candle_core::bail!("flash attention is not enabled in Wilkes' Gemma decoder")
}

#[derive(Debug, Clone)]
struct DecoderLayer {
    self_attn: Attention,
    mlp: MLP,
    input_layernorm: RmsNorm,
    pre_feedforward_layernorm: RmsNorm,
    post_feedforward_layernorm: RmsNorm,
    post_attention_layernorm: RmsNorm,
    sliding_window: Option<usize>,
}

impl DecoderLayer {
    fn new(
        use_flash_attn: bool,
        cfg: &Config,
        vb: VarBuilder,
        sliding_window: Option<usize>,
    ) -> Result<Self> {
        let rotary_emb = Arc::new(RotaryEmbedding::new(
            vb.dtype(),
            cfg,
            vb.device(),
            sliding_window,
        )?);
        let self_attn = Attention::new(
            rotary_emb,
            use_flash_attn,
            cfg,
            sliding_window,
            vb.pp("self_attn"),
        )?;
        let mlp = MLP::new(cfg, vb.pp("mlp"))?;
        let input_layernorm =
            RmsNorm::new(cfg.hidden_size, cfg.rms_norm_eps, vb.pp("input_layernorm"))?;
        let pre_feedforward_layernorm = RmsNorm::new(
            cfg.hidden_size,
            cfg.rms_norm_eps,
            vb.pp("pre_feedforward_layernorm"),
        )?;
        let post_feedforward_layernorm = RmsNorm::new(
            cfg.hidden_size,
            cfg.rms_norm_eps,
            vb.pp("post_feedforward_layernorm"),
        )?;
        let post_attention_layernorm = RmsNorm::new(
            cfg.hidden_size,
            cfg.rms_norm_eps,
            vb.pp("post_attention_layernorm"),
        )?;
        Ok(Self {
            self_attn,
            mlp,
            input_layernorm,
            pre_feedforward_layernorm,
            post_feedforward_layernorm,
            post_attention_layernorm,
            sliding_window,
        })
    }

    fn forward(
        &mut self,
        xs: &Tensor,
        attention_mask: Option<&Tensor>,
        seqlen_offset: usize,
    ) -> Result<Tensor> {
        let residual = xs;
        let xs = self.input_layernorm.forward(xs)?;
        let xs = self.self_attn.forward(&xs, attention_mask, seqlen_offset)?;
        let xs = xs.apply(&self.post_attention_layernorm)?;
        let xs = (xs + residual)?;
        let residual = &xs;
        let xs = xs.apply(&self.pre_feedforward_layernorm)?;
        let xs = xs.apply(&self.mlp)?;
        let xs = xs.apply(&self.post_feedforward_layernorm)?;
        residual + xs
    }

    fn clear_kv_cache(&mut self) {
        self.self_attn.clear_kv_cache()
    }
}

fn prepare_decoder_attention_mask(
    b_size: usize,
    tgt_len: usize,
    seqlen_offset: usize,
    dtype: DType,
    device: &Device,
) -> Result<Tensor> {
    let source_len = seqlen_offset + tgt_len;
    let mask: Vec<_> = (0..tgt_len)
        .flat_map(|query| {
            let absolute_query = seqlen_offset + query;
            (0..source_len).map(move |key| {
                let is_future = key > absolute_query;
                if is_future {
                    f32::NEG_INFINITY
                } else {
                    0.
                }
            })
        })
        .collect();
    Tensor::from_slice(&mask, (tgt_len, source_len), device)?
        .expand((b_size, 1, tgt_len, source_len))?
        .to_dtype(dtype)
}

#[derive(Debug, Clone)]
pub struct Model {
    embed_tokens: candle_nn::Embedding,
    layers: Vec<DecoderLayer>,
    norm: RmsNorm,
    lm_head: Linear,
    final_logit_softcapping: Option<f64>,
    device: Device,
    dtype: DType,
    hidden_size: usize,
}

impl Model {
    pub fn new(use_flash_attn: bool, cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let vb_m = vb.pp("model");
        let embed_tokens =
            candle_nn::embedding(cfg.vocab_size, cfg.hidden_size, vb_m.pp("embed_tokens"))?;
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        let vb_l = vb_m.pp("layers");
        for layer_idx in 0..cfg.num_hidden_layers {
            let sliding_window = (layer_idx + 1) % cfg.sliding_window_pattern > 0;
            let layer = DecoderLayer::new(
                use_flash_attn,
                cfg,
                vb_l.pp(layer_idx),
                sliding_window.then_some(cfg.sliding_window),
            )?;
            layers.push(layer)
        }
        let norm = RmsNorm::new(cfg.hidden_size, cfg.rms_norm_eps, vb_m.pp("norm"))?;
        let lm_head = Linear::new(embed_tokens.embeddings().clone(), None);
        Ok(Self {
            embed_tokens,
            layers,
            norm,
            lm_head,
            final_logit_softcapping: cfg.final_logit_softcapping,
            device: vb.device().clone(),
            dtype: vb.dtype(),
            hidden_size: cfg.hidden_size,
        })
    }

    fn create_attention_mask(
        &self,
        batch_size: usize,
        seq_len: usize,
        seqlen_offset: usize,
    ) -> Result<Option<Tensor>> {
        (seq_len > 1)
            .then(|| {
                prepare_decoder_attention_mask(
                    batch_size,
                    seq_len,
                    seqlen_offset,
                    self.dtype,
                    &self.device,
                )
            })
            .transpose()
    }

    pub fn forward(&mut self, input_ids: &Tensor, seqlen_offset: usize) -> Result<Tensor> {
        let (b_size, seq_len) = input_ids.dims2()?;
        let xs = self.embed_tokens.forward(input_ids)?;
        let mut xs = (xs * (self.hidden_size as f64).sqrt())?;

        let attention_mask = self.create_attention_mask(b_size, seq_len, seqlen_offset)?;

        for layer in self.layers.iter_mut() {
            let mask = if layer.sliding_window.is_some() {
                None
            } else {
                attention_mask.as_ref()
            };
            xs = layer.forward(&xs, mask, seqlen_offset)?
        }
        let logits = xs
            .narrow(1, seq_len - 1, 1)?
            .apply(&self.norm)?
            .apply(&self.lm_head)?;
        let logits = match self.final_logit_softcapping {
            None => logits,
            Some(sc) => ((logits / sc)?.tanh()? * sc)?,
        };

        Ok(logits)
    }

    pub fn clear_kv_cache(&mut self) {
        for layer in self.layers.iter_mut() {
            layer.clear_kv_cache()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rotary_config(max_position_embeddings: usize) -> Config {
        Config {
            attention_bias: false,
            head_dim: 8,
            hidden_activation: Activation::Gelu,
            hidden_size: 8,
            intermediate_size: 16,
            num_attention_heads: 1,
            num_hidden_layers: 1,
            num_key_value_heads: 1,
            rms_norm_eps: 1e-6,
            rope_theta: 1_000_000.0,
            rope_local_base_freq: 10_000.0,
            vocab_size: 16,
            final_logit_softcapping: None,
            attn_logit_softcapping: None,
            _query_pre_attn_scalar: 8,
            sliding_window: 512,
            sliding_window_pattern: 6,
            max_position_embeddings,
        }
    }

    fn mask_rows(mask: &Tensor) -> Result<Vec<Vec<f32>>> {
        mask.clone().squeeze(0)?.squeeze(0)?.to_vec2()
    }

    #[test]
    fn rotating_mask_tracks_ring_order_across_prefill_chunks() -> Result<()> {
        let device = Device::Cpu;
        let mut cache = candle_nn::kv_cache::RotatingKvCache::new(2, 3);
        let first_mask = additive_attention_mask(
            &cache.attn_mask(2, &device)?.expect("two-token mask"),
            DType::F32,
        )?;
        assert_eq!(
            mask_rows(&first_mask)?,
            vec![vec![0.0, f32::NEG_INFINITY], vec![0.0, 0.0],]
        );

        let first = Tensor::zeros((1, 1, 2, 1), DType::F32, &device)?;
        cache.append(&first, &first)?;

        let second_mask = additive_attention_mask(
            &cache.attn_mask(2, &device)?.expect("two-token mask"),
            DType::F32,
        )?;

        assert_eq!(
            mask_rows(&second_mask)?,
            vec![vec![f32::NEG_INFINITY, 0.0, 0.0], vec![0.0, 0.0, 0.0],]
        );
        Ok(())
    }

    #[test]
    fn global_mask_keeps_cached_history_and_masks_only_future_tokens() -> Result<()> {
        let mask = prepare_decoder_attention_mask(1, 2, 4, DType::F32, &Device::Cpu)?;

        assert_eq!(
            mask_rows(&mask)?,
            vec![
                vec![0.0, 0.0, 0.0, 0.0, 0.0, f32::NEG_INFINITY],
                vec![0.0; 6],
            ]
        );
        Ok(())
    }

    #[test]
    fn rotating_cache_retains_the_exact_window_boundary_for_decode() -> Result<()> {
        let device = Device::Cpu;
        let mut cache = candle_nn::kv_cache::RotatingKvCache::new(2, 3);
        let prompt = Tensor::zeros((1, 1, 4, 1), DType::F32, &device)?;
        cache.append(&prompt, &prompt)?;

        assert_eq!(cache.positions(1), vec![4, 2, 3]);
        assert!(cache.attn_mask(1, &device)?.is_none());
        Ok(())
    }

    #[test]
    fn fp16_rotary_table_keeps_adjacent_positions_distinct_near_16k() -> Result<()> {
        let lossy_positions = Tensor::arange(16_588u32, 16_592u32, &Device::Cpu)?
            .to_dtype(DType::F16)?
            .to_dtype(DType::F32)?
            .to_vec1::<f32>()?;
        assert!(lossy_positions.windows(2).any(|pair| pair[0] == pair[1]));

        let rotary = RotaryEmbedding::new(DType::F16, &rotary_config(16_600), &Device::Cpu, None)?;
        let sin = rotary
            .sin
            .narrow(0, 16_588, 4)?
            .to_dtype(DType::F32)?
            .to_vec2::<f32>()?;
        let cos = rotary
            .cos
            .narrow(0, 16_588, 4)?
            .to_dtype(DType::F32)?
            .to_vec2::<f32>()?;

        for index in 1..4 {
            assert!(
                sin[index] != sin[index - 1] || cos[index] != cos[index - 1],
                "positions {} and {} share one rotary phase",
                16_588 + index - 1,
                16_588 + index
            );
        }
        Ok(())
    }
}
