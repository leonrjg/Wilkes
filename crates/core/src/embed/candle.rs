use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

use anyhow::Context;
use async_trait::async_trait;
use candle_core::{Device, DType, Tensor, Module};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use candle_transformers::models::jina_bert::{BertModel as JinaBertModel, Config as JinaBertConfig};
use candle_transformers::models::modernbert::{ModernBert, Config as ModernBertConfig};
use hf_hub::api::sync::ApiBuilder;
use tokenizers::Tokenizer;

use crate::types::{EmbedderModel, ModelDescriptor};
use super::Embedder;
use super::installer::{DownloadProgress, EmbedProgress, EmbedderInstaller, ProgressTx};

// ── Static model catalog ──────────────────────────────────────────────────────

struct ModelInfo {
    model_id: &'static str,
    display_name: &'static str,
    description: &'static str,
    dimension: usize,
    is_default: bool,
    is_recommended: bool,
}

const PREEXISTING_MODELS: &[ModelInfo] = &[
    ModelInfo {
        model_id: "BAAI/bge-base-en-v1.5",
        display_name: "bge-base-en-v1.5",
        description: "BGE base English embeddings (768-dim)",
        dimension: 768,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-MiniLM-L12-v2",
        display_name: "all-MiniLM-L12-v2",
        description: "Speed: high, accuracy: medium (English)",
        dimension: 384,
        is_default: true,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "intfloat/multilingual-e5-large-instruct",
        display_name: "multilingual-e5-large-instruct",
        description: "Multilingual instruction-tuned E5 (1024-dim)",
        dimension: 1024,
        is_default: false,
        is_recommended: false,
    },
];

// Files required to load and run a model.
const MODEL_FILES: &[&str] = &["config.json", "tokenizer.json", "model.safetensors"];

// ── HF cache helpers ──────────────────────────────────────────────────────────

fn cached_path(data_dir: &Path, model_id: &str, filename: &str) -> Option<PathBuf> {
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::model(model_id.to_string()))
        .get(filename)
}

fn cached_size_bytes(data_dir: &Path, model_id: &str) -> Option<u64> {
    let total: u64 = MODEL_FILES
        .iter()
        .filter_map(|f| {
            cached_path(data_dir, model_id, f)
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
        })
        .sum();
    if total > 0 { Some(total) } else { None }
}

// ── Public model list ─────────────────────────────────────────────────────────

pub fn list_supported_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    PREEXISTING_MODELS
        .iter()
        .map(|info| {
            let is_cached = super::hf_hub::is_model_cached(data_dir, info.model_id);
            let size_bytes = if is_cached {
                cached_size_bytes(data_dir, info.model_id)
            } else {
                None
            };
            ModelDescriptor {
                model_id: info.model_id.to_string(),
                display_name: info.display_name.to_string(),
                description: info.description.to_string(),
                dimension: info.dimension,
                is_cached,
                is_default: info.is_default,
                is_recommended: info.is_recommended,
                size_bytes,
                preferred_batch_size: Some(EMBED_BATCH_SIZE),
            }
        })
        .collect()
}

// ── Pooling strategy ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum PoolingStrategy {
    Mean,
    Cls,
    Max,
}

fn load_pooling_strategy(data_dir: &Path, model_id: &str) -> PoolingStrategy {
    let Some(path) = cached_path(data_dir, model_id, "1_Pooling/config.json") else {
        return PoolingStrategy::Mean;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return PoolingStrategy::Mean;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return PoolingStrategy::Mean;
    };
    if v.get("pooling_mode_cls_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return PoolingStrategy::Cls;
    }
    if v.get("pooling_mode_max_tokens")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return PoolingStrategy::Max;
    }
    PoolingStrategy::Mean
}

// ── Device / dtype selection ──────────────────────────────────────────────────

fn select_device(device: &str) -> Device {
    if device == "cpu" {
        return Device::Cpu;
    }
    #[cfg(feature = "candle-metal")]
    if candle_core::utils::metal_is_available() {
        match Device::new_metal(0) {
            Ok(d) => return d,
            Err(e) => warn!("Metal device init failed ({e:#}), falling back to CPU"),
        }
    }
    Device::Cpu
}

/// F32 everywhere. Candle 0.9's Metal backend lacks F16 kernels for several ops
/// (layer norm, softmax, GELU), causing silent GPU→CPU→GPU roundtrips that
/// dominate runtime. F32 has full Metal kernel coverage, keeping all operations
/// on the GPU. The 2× memory increase is negligible for embedding-sized models.
fn select_dtype(_device: &Device) -> DType {
    DType::F32
}

// ── CandleEmbedder ────────────────────────────────────────────────────────────

/// Batches of 32 balance Metal dispatch overhead against intermediate tensor
/// memory (attention matrices scale with batch × seq_len²). Very large batches
/// can stall Metal due to memory pressure.
const EMBED_BATCH_SIZE: usize = 32;
const MAX_SEQUENCE_LENGTH: usize = 512;

enum LoadedModel {
    Bert(BertModel),
    JinaBert(JinaBertModel),
    ModernBert(ModernBert),
}

#[derive(serde::Deserialize)]
struct ModelTypePeek {
    model_type: String,
}

pub struct CandleEmbedder {
    model: LoadedModel,
    tokenizer: Tokenizer,
    device: Device,
    dtype: DType,
    model_id: String,
    dimension: usize,
    pooling: PoolingStrategy,
}

impl CandleEmbedder {
    fn prefixes(&self) -> (&'static str, &'static str) {
        // E5 family requires explicit prefixes for reliable retrieval.
        if self.model_id.contains("/multilingual-e5")
            || self.model_id.contains("/e5-")
        {
            ("query: ", "passage: ")
        } else {
            ("", "")
        }
    }

    fn embed_with_prefix(&self, texts: &[&str], prefix: &str) -> anyhow::Result<Vec<Vec<f32>>> {
        if prefix.is_empty() {
            self.embed_slices(texts)
        } else {
            let owned: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            self.embed_slices(&refs)
        }
    }

    fn embed_slices(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let total = texts.len();
        debug!("[candle] embed_slices: {total} texts, batch_size={EMBED_BATCH_SIZE}");
        let t0 = std::time::Instant::now();
        let mut out = Vec::with_capacity(total);
        for (i, chunk) in texts.chunks(EMBED_BATCH_SIZE).enumerate() {
            let tb = std::time::Instant::now();
            out.extend(self.embed_batch(chunk)?);
            debug!("[candle]   batch {i}: {} texts in {:.1}s", chunk.len(), tb.elapsed().as_secs_f64());
        }
        debug!("[candle] embed_slices total: {:.1}s", t0.elapsed().as_secs_f64());
        Ok(out)
    }

    fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let t_tok = std::time::Instant::now();
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("Tokenization error: {e}"))?;

        let batch_size = encodings.len();
        let seq_len = encodings[0].get_ids().len();
        debug!("[candle]   tokenize: {batch_size}×{seq_len} in {:.3}s", t_tok.elapsed().as_secs_f64());
        if seq_len == 0 {
            return Ok(vec![vec![0.0_f32; self.dimension]; batch_size]);
        }

        let input_ids: Vec<u32> = encodings
            .iter()
            .flat_map(|e| e.get_ids().iter().copied())
            .collect();
        let attention_mask: Vec<u32> = encodings
            .iter()
            .flat_map(|e| e.get_attention_mask().iter().copied())
            .collect();
        let token_type_ids: Vec<u32> = encodings
            .iter()
            .flat_map(|e| e.get_type_ids().iter().copied())
            .collect();

        let input_ids =
            Tensor::from_vec(input_ids, (batch_size, seq_len), &self.device)?;
        let attention_mask =
            Tensor::from_vec(attention_mask, (batch_size, seq_len), &self.device)?;
        let token_type_ids =
            Tensor::from_vec(token_type_ids, (batch_size, seq_len), &self.device)?;

        let t_fwd = std::time::Instant::now();
        let token_embeddings = match &self.model {
            LoadedModel::Bert(m) => m.forward(&input_ids, &token_type_ids, Some(&attention_mask))?,
            LoadedModel::JinaBert(m) => Module::forward(m, &input_ids)?,
            LoadedModel::ModernBert(m) => m.forward(&input_ids, &attention_mask)?,
        };
        // Force GPU sync so we measure real forward time, not just command submission.
        let _ = token_embeddings.flatten_all()?.narrow(0, 0, 1)?.to_vec1::<f32>()?;
        debug!("[candle]   forward (synced): {:.3}s", t_fwd.elapsed().as_secs_f64());

        let t_pool = std::time::Instant::now();
        let mask_f32 = attention_mask.to_dtype(self.dtype)?;
        let pooled = self.pool(&token_embeddings, &mask_f32)?.contiguous()?;
        let normalized = l2_normalize(&pooled)?;

        let result = normalized.to_dtype(DType::F32)?.to_vec2::<f32>()?;
        debug!("[candle]   pool+normalize+download: {:.3}s", t_pool.elapsed().as_secs_f64());
        Ok(result)
    }

    fn pool(&self, token_embeddings: &Tensor, attention_mask: &Tensor) -> anyhow::Result<Tensor> {
        // token_embeddings: (B, S, H)
        // attention_mask:   (B, S)  — f32, 1.0 for real tokens, 0.0 for padding
        Ok(match self.pooling {
            PoolingStrategy::Cls => {
                // [CLS] is the first token.
                token_embeddings.narrow(1, 0, 1)?.squeeze(1)?
            }
            PoolingStrategy::Mean => {
                // Express masked mean as a matmul to use a single well-optimised
                // kernel instead of broadcast-multiply + sum (broadcast ops may
                // lack Metal kernels and silently fall back to CPU).
                //   mask_row: (B, 1, S)  ×  token_embeddings: (B, S, H)  →  (B, 1, H)
                let mask_row = attention_mask.unsqueeze(1)?; // (B, 1, S)
                let masked_sum = mask_row.matmul(token_embeddings)?; // (B, 1, H)
                let count = attention_mask.sum_keepdim(1)?.unsqueeze(2)?; // (B, 1, 1)
                masked_sum.broadcast_div(&count)?.squeeze(1)? // (B, H)
            }
            PoolingStrategy::Max => {
                // Fill padded positions with -∞ so they don't win the max.
                let neg_inf = Tensor::full::<f32, _>(
                    f32::NEG_INFINITY,
                    token_embeddings.shape(),
                    &self.device,
                )?
                .to_dtype(self.dtype)?;
                let mask_bool = attention_mask
                    .ne(0.0f32)?
                    .unsqueeze(2)?
                    .broadcast_as(token_embeddings.shape())?;
                mask_bool
                    .where_cond(token_embeddings, &neg_inf)?
                    .max(1)?
            }
        })
    }
}

fn l2_normalize(t: &Tensor) -> anyhow::Result<Tensor> {
    let norm = t.sqr()?.sum_keepdim(1)?.sqrt()?;
    Ok(t.broadcast_div(&norm)?)
}

impl Embedder for CandleEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_slices(texts)
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn preferred_batch_size(&self) -> Option<usize> {
        Some(EMBED_BATCH_SIZE)
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (qp, _) = self.prefixes();
        self.embed_with_prefix(texts, qp)
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (_, pp) = self.prefixes();
        self.embed_with_prefix(texts, pp)
    }
}

// ── CandleInstaller ───────────────────────────────────────────────────────────

pub struct CandleInstaller {
    pub model: EmbedderModel,
    pub manager: super::worker_manager::WorkerManager,
    pub device: String,
}

impl CandleInstaller {
    pub fn new(model: EmbedderModel, manager: super::worker_manager::WorkerManager, device: String) -> Self {
        Self { model, manager, device }
    }
}

#[async_trait]
impl EmbedderInstaller for CandleInstaller {
    fn is_available(&self, data_dir: &Path) -> bool {
        super::hf_hub::is_model_cached(data_dir, &self.model.0)
    }

    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()> {
        let model_id = self.model.0.clone();
        let data_dir = data_dir.to_path_buf();

        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: false,
            }))
            .await;

        tokio::task::spawn_blocking(move || {
            let api = ApiBuilder::new()
                .with_cache_dir(data_dir)
                .build()
                .context("Failed to initialise HF hub API")?;
            let repo = api.model(model_id.clone());

            for filename in MODEL_FILES {
                repo.get(filename)
                    .with_context(|| format!("Failed to download '{filename}' for '{model_id}'"))?;
            }

            // Pooling config is optional; ignore errors.
            let _ = repo.get("1_Pooling/config.json");

            Ok::<_, anyhow::Error>(())
        })
        .await??;

        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: true,
            }))
            .await;

        Ok(())
    }

    fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn build(&self, data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
        let model_id = &self.model.0;
        let dimension = read_dimension(data_dir, model_id)?;
        Ok(Arc::new(super::sbert::WorkerEmbedder::new(
            self.manager.clone(),
            model_id.clone(),
            dimension,
            self.device.clone(),
            crate::types::EmbeddingEngine::Candle,
            data_dir.to_path_buf(),
        )))
    }
}

/// Read the embedding dimension for a model without loading its weights.
/// Checks the static catalog first, then falls back to parsing config.json.
fn read_dimension(data_dir: &Path, model_id: &str) -> anyhow::Result<usize> {
    if let Some(m) = PREEXISTING_MODELS.iter().find(|m| m.model_id == model_id) {
        return Ok(m.dimension);
    }
    let config_path = cached_path(data_dir, model_id, "config.json")
        .ok_or_else(|| anyhow::anyhow!("config.json not cached for '{model_id}'"))?;
    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config.json for '{model_id}'"))?;
    let v: serde_json::Value = serde_json::from_str(&config_text)
        .with_context(|| format!("Failed to parse config.json for '{model_id}'"))?;
    v.get("hidden_size")
        .and_then(|v| v.as_u64())
        .map(|d| d as usize)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine embedding dimension for '{model_id}'"))
}

/// Load a `CandleEmbedder` directly in the calling process.
/// Only called from the worker subprocess, never from the main Tauri process.
pub fn load_embedder(model: &EmbedderModel, data_dir: &Path, device: &str) -> anyhow::Result<Arc<dyn Embedder>> {
    let model_id = &model.0;

    let config_path = cached_path(data_dir, model_id, "config.json")
        .ok_or_else(|| anyhow::anyhow!("config.json not cached for '{model_id}'"))?;
    let tokenizer_path = cached_path(data_dir, model_id, "tokenizer.json")
        .ok_or_else(|| anyhow::anyhow!("tokenizer.json not cached for '{model_id}'"))?;
    let weights_path = cached_path(data_dir, model_id, "model.safetensors")
        .ok_or_else(|| anyhow::anyhow!("model.safetensors not cached for '{model_id}'"))?;

    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config.json for '{model_id}'"))?;

    let peek: ModelTypePeek = serde_json::from_str(&config_text)
        .with_context(|| format!("Failed to peek model_type from config.json for '{model_id}'"))?;

    let device = select_device(device);
    let dtype = select_dtype(&device);

    // Safety: memory-mapping model weights from a local path we own.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[&weights_path], dtype, &device)
            .with_context(|| format!("Failed to load weights for '{model_id}'"))?
    };

    info!("[candle] dispatching '{model_id}' with model_type='{}'", peek.model_type);

    let (model_loaded, dimension) = match peek.model_type.as_str() {
        "jina_bert_v2" | "jina_bert" | "jina_bert_v3" | "qwen2" | "qwen" | "jina_embeddings_v5" => {
            let config: JinaBertConfig = serde_json::from_str(&config_text)
                .map_err(|e| anyhow::anyhow!("Failed to parse JinaBertConfig for '{model_id}': {e}. Config: {config_text}"))?;
            let dim = config.hidden_size;
            let m = JinaBertModel::new(vb, &config)
                .with_context(|| format!("Failed to build JinaBert model for '{model_id}'"))?;
            (LoadedModel::JinaBert(m), dim)
        }
        "modern_bert" => {
            let config: ModernBertConfig = serde_json::from_str(&config_text)
                .with_context(|| format!("Failed to parse ModernBertConfig for '{model_id}'"))?;
            let dim = config.hidden_size;
            let m = ModernBert::load(vb, &config)
                .with_context(|| format!("Failed to build ModernBert model for '{model_id}'"))?;
            (LoadedModel::ModernBert(m), dim)
        }
        _ => {
            let config: BertConfig = serde_json::from_str(&config_text)
                .with_context(|| format!("Failed to parse BertConfig for '{model_id}'"))?;
            let dim = config.hidden_size;
            let m = BertModel::load(vb, &config)
                .with_context(|| format!("Failed to build BERT model for '{model_id}'"))?;
            (LoadedModel::Bert(m), dim)
        }
    };

    let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| anyhow::anyhow!("Failed to load tokenizer for '{model_id}': {e}"))?;

    tokenizer.with_padding(Some(tokenizers::PaddingParams {
        strategy: tokenizers::PaddingStrategy::BatchLongest,
        ..Default::default()
    }));
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: MAX_SEQUENCE_LENGTH,
            ..Default::default()
        }))
        .map_err(|e| anyhow::anyhow!("Failed to configure truncation for '{model_id}': {e}"))?;

    let pooling = load_pooling_strategy(data_dir, model_id);

    info!(
        "[candle] loaded '{model_id}' dim={dimension} pooling={pooling:?} device={device:?} dtype={dtype:?}"
    );

    Ok(Arc::new(CandleEmbedder {
        model: model_loaded,
        tokenizer,
        device,
        dtype,
        model_id: model_id.clone(),
        dimension,
        pooling,
    }))
}
