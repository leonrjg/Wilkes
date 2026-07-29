use std::io::{Read, Seek};

use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::models::gemma3::Model as Gemma3Model;
use candle_transformers::models::quantized_qwen3::ModelWeights as Qwen3Weights;

use super::protocol::ModelFamily;

/// Architecture-specific decoder weights behind the one interface the token
/// loop needs. Protocol selection and tensor implementation share the same
/// `ModelFamily`, so a catalog entry cannot load as one family and frame as
/// another.
pub(super) enum DecoderWeights {
    Qwen3(Box<Qwen3Weights>),
    Gemma3(Box<Gemma3Model>),
}

impl DecoderWeights {
    pub(super) fn from_gguf<R: Read + Seek>(
        family: ModelFamily,
        content: gguf_file::Content,
        reader: &mut R,
        device: &Device,
    ) -> anyhow::Result<Self> {
        Ok(match family {
            ModelFamily::Qwen3 => {
                Self::Qwen3(Box::new(Qwen3Weights::from_gguf(content, reader, device)?))
            }
            ModelFamily::Gemma3 => anyhow::bail!(
                "Gemma 3 must use dense safetensors so its rotating KV cache is preserved"
            ),
        })
    }

    pub(super) fn from_dense_gemma(model: Gemma3Model) -> Self {
        Self::Gemma3(Box::new(model))
    }

    pub(super) fn reset_cache(&mut self) {
        match self {
            Self::Qwen3(weights) => weights.clear_kv_cache(),
            Self::Gemma3(weights) => weights.clear_kv_cache(),
        }
    }

    pub(super) fn forward(&mut self, input: &Tensor, offset: usize) -> candle_core::Result<Tensor> {
        match self {
            Self::Qwen3(weights) => weights.forward(input, offset),
            // The dense implementation retains a singleton sequence axis
            // (`batch × 1 × vocab`); the quantized Qwen decoder returns
            // `batch × vocab`. Normalize at the architecture boundary.
            Self::Gemma3(weights) => weights.forward(input, offset)?.squeeze(1),
        }
    }
}
