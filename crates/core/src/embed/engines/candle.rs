use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

use anyhow::Context;
use async_trait::async_trait;
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use candle_transformers::models::jina_bert::{
    BertModel as JinaBertModel, Config as JinaBertConfig,
};
use candle_transformers::models::modernbert::{Config as ModernBertConfig, ModernBert};
use hf_hub::api::sync::ApiBuilder;
use tokenizers::Tokenizer;

use super::super::models::installer::{
    DownloadProgress, EmbedProgress, EmbedderInstaller, ProgressTx,
};
use super::super::Embedder;
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};

// ── Static model catalog ──────────────────────────────────────────────────────

struct ModelInfo {
    model_id: &'static str,
    display_name: &'static str,
    description: &'static str,
    dimension: usize,
    is_recommended: bool,
}

const PREEXISTING_MODELS: &[ModelInfo] = &[
    ModelInfo {
        model_id: "BAAI/bge-base-en-v1.5",
        display_name: "bge-base-en-v1.5",
        description: "BGE base English embeddings (768-dim)",
        dimension: 768,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-MiniLM-L12-v2",
        display_name: "all-MiniLM-L12-v2",
        description: "Speed: high, accuracy: medium (English)",
        dimension: 384,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "intfloat/multilingual-e5-large-instruct",
        display_name: "multilingual-e5-large-instruct",
        description: "Multilingual instruction-tuned E5 (1024-dim)",
        dimension: 1024,
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
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

// ── Public model list ─────────────────────────────────────────────────────────
pub fn list_supported_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    PREEXISTING_MODELS
        .iter()
        .map(|info| {
            let is_cached = super::super::models::hf_hub::is_model_cached(data_dir, info.model_id);

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
                is_default: false,
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
    query_prefix: String,
    passage_prefix: String,
}

impl CandleEmbedder {
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
            debug!(
                "[candle]   batch {i}: {} texts in {:.1}s",
                chunk.len(),
                tb.elapsed().as_secs_f64()
            );
        }
        debug!(
            "[candle] embed_slices total: {:.1}s",
            t0.elapsed().as_secs_f64()
        );
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
        debug!(
            "[candle]   tokenize: {batch_size}×{seq_len} in {:.3}s",
            t_tok.elapsed().as_secs_f64()
        );
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

        let input_ids = Tensor::from_vec(input_ids, (batch_size, seq_len), &self.device)?;
        let attention_mask = Tensor::from_vec(attention_mask, (batch_size, seq_len), &self.device)?;
        let token_type_ids = Tensor::from_vec(token_type_ids, (batch_size, seq_len), &self.device)?;

        let t_fwd = std::time::Instant::now();
        let token_embeddings = match &self.model {
            LoadedModel::Bert(m) => {
                m.forward(&input_ids, &token_type_ids, Some(&attention_mask))?
            }
            LoadedModel::JinaBert(m) => Module::forward(m, &input_ids)?,
            LoadedModel::ModernBert(m) => m.forward(&input_ids, &attention_mask)?,
        };
        // Force GPU sync so we measure real forward time, not just command submission.
        let _ = token_embeddings
            .flatten_all()?
            .narrow(0, 0, 1)?
            .to_vec1::<f32>()?;
        debug!(
            "[candle]   forward (synced): {:.3}s",
            t_fwd.elapsed().as_secs_f64()
        );

        let t_pool = std::time::Instant::now();
        let mask_f32 = attention_mask.to_dtype(self.dtype)?;
        let pooled = self.pool(&token_embeddings, &mask_f32)?.contiguous()?;
        let normalized = l2_normalize(&pooled)?;

        let result = normalized.to_dtype(DType::F32)?.to_vec2::<f32>()?;
        debug!(
            "[candle]   pool+normalize+download: {:.3}s",
            t_pool.elapsed().as_secs_f64()
        );
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
                mask_bool.where_cond(token_embeddings, &neg_inf)?.max(1)?
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

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Candle
    }

    fn preferred_batch_size(&self) -> Option<usize> {
        Some(EMBED_BATCH_SIZE)
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, &self.query_prefix)
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, &self.passage_prefix)
    }
}

// ── CandleInstaller ───────────────────────────────────────────────────────────

pub struct CandleInstaller {
    pub model: EmbedderModel,
    pub manager: super::super::worker::manager::WorkerManager,
    pub device: String,
}

impl CandleInstaller {
    pub fn new(
        model: EmbedderModel,
        manager: super::super::worker::manager::WorkerManager,
        device: String,
    ) -> Self {
        Self {
            model,
            manager,
            device,
        }
    }
}

#[async_trait]
impl EmbedderInstaller for CandleInstaller {
    fn is_available(&self, data_dir: &Path) -> bool {
        super::super::models::hf_hub::is_model_cached(data_dir, &self.model.0)
    }

    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()> {
        let model_id = self.model.0.clone();
        let data_dir = data_dir.to_path_buf();
        let download_cache_dir = data_dir.clone();

        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: false,
            }))
            .await;

        tokio::task::spawn_blocking(move || {
            let api = ApiBuilder::new()
                .with_cache_dir(download_cache_dir)
                .build()
                .context("Failed to initialise HF hub API")?;
            let repo = api.model(model_id.clone());

            for filename in MODEL_FILES {
                let url = repo.url(filename);
                repo.get(filename).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to download '{filename}' for '{model_id}' from {url}: {e:#}"
                    )
                })?;
            }

            // Pooling config is optional; ignore errors.
            let _ = repo.get("1_Pooling/config.json");

            Ok::<_, anyhow::Error>(())
        })
        .await??;

        let aux_model_id = self.model.0.clone();
        let aux_cache_dir = data_dir.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || {
            super::aux_config::fetch_aux_configs(&aux_cache_dir, &aux_model_id);
        })
        .await;

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
        let prefixes = super::aux_config::load_prefixes(data_dir, model_id);
        Ok(Arc::new(
            super::super::worker::embedder::WorkerEmbedder::new(
                self.manager.clone(),
                super::super::worker::embedder::WorkerEmbedderConfig {
                    model_id: model_id.clone(),
                    dimension,
                    device: self.device.clone(),
                    engine: EmbeddingEngine::Candle,
                    data_dir: data_dir.to_path_buf(),
                    query_prefix: prefixes.query_prefix,
                    passage_prefix: prefixes.passage_prefix,
                },
            ),
        ))
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
pub fn load_embedder(
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Embedder>> {
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
    // The `?` catches mmap() errors, but a page access on a file that is
    // truncated or corrupted on disk after mapping succeeds will raise SIGBUS
    // with no Rust error path. The embed_task guard prevents a concurrent
    // download from replacing this file while it is mapped; disk corruption
    // remains an unrecoverable crash risk.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[&weights_path], dtype, &device)
            .with_context(|| format!("Failed to load weights for '{model_id}'"))?
    };

    info!(
        "[candle] dispatching '{model_id}' with model_type='{}'",
        peek.model_type
    );

    let (model_loaded, dimension) = match peek.model_type.as_str() {
        "jina_bert_v2" | "jina_bert" | "jina_bert_v3" | "qwen2" | "qwen" | "jina_embeddings_v5" => {
            let config: JinaBertConfig = serde_json::from_str(&config_text).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse JinaBertConfig for '{model_id}': {e}. Config: {config_text}"
                )
            })?;
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

    let prefixes = super::aux_config::load_prefixes(data_dir, model_id);

    Ok(Arc::new(CandleEmbedder {
        model: model_loaded,
        tokenizer,
        device,
        dtype,
        model_id: model_id.clone(),
        dimension,
        pooling,
        query_prefix: prefixes.query_prefix,
        passage_prefix: prefixes.passage_prefix,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_list_supported_models() {
        let dir = tempdir().unwrap();
        let models = list_supported_models(dir.path());
        assert!(!models.is_empty());
        let default_id = crate::types::EmbeddingEngine::Candle.default_model();
        assert!(
            models.iter().any(|m| m.model_id == default_id),
            "Default model '{default_id}' must exist in the Candle catalog"
        );
    }

    #[test]
    fn test_select_device() {
        assert!(matches!(select_device("cpu"), Device::Cpu));
        // On non-metal systems or without feature, it should fallback to Cpu
        assert!(matches!(
            select_device("metal"),
            Device::Cpu | Device::Metal(_)
        ));
    }

    #[test]
    fn test_read_dimension_static() {
        let dir = tempdir().unwrap();
        let dim = read_dimension(dir.path(), "sentence-transformers/all-MiniLM-L12-v2").unwrap();
        assert_eq!(dim, 384);
    }

    #[test]
    fn test_load_pooling_strategy_default() {
        let dir = tempdir().unwrap();
        let strategy = load_pooling_strategy(dir.path(), "non-existent");
        assert!(matches!(strategy, PoolingStrategy::Mean));
    }

    #[test]
    fn test_l2_normalize() {
        let device = Device::Cpu;
        let t = Tensor::new(&[[3.0f32, 4.0f32]], &device).unwrap();
        let norm = l2_normalize(&t).unwrap();
        let expected = vec![vec![0.6f32, 0.8f32]];
        assert_eq!(norm.to_vec2::<f32>().unwrap(), expected);
    }

    #[test]
    fn test_select_dtype() {
        assert_eq!(select_dtype(&Device::Cpu), DType::F32);
    }

    #[test]
    fn test_read_dimension_from_config_hidden_size() {
        let dir = tempdir().unwrap();
        let model_id = "custom/model";
        let repo_dir = dir.path().join("models--custom--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), r#"{"hidden_size": 1024}"#).unwrap();

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let dim = read_dimension(dir.path(), model_id).unwrap();
        assert_eq!(dim, 1024);
    }

    #[test]
    fn test_read_dimension_missing_config() {
        let dir = tempdir().unwrap();
        let res = read_dimension(dir.path(), "non-existent");
        assert!(res.is_err());
    }

    #[test]
    fn test_read_dimension_invalid_json() {
        let dir = tempdir().unwrap();
        let model_id = "bad/json";
        let repo_dir = dir.path().join("models--bad--json");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), r#"{"hidden_size": "#).unwrap();

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let res = read_dimension(dir.path(), model_id);
        assert!(res.is_err());
    }

    #[test]
    fn test_load_pooling_strategy_cls_works() {
        let dir = tempdir().unwrap();
        let model_id = "cls/model";
        let repo_dir = dir.path().join("models--cls--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(snapshots.join("1_Pooling")).unwrap();
        fs::write(
            snapshots.join("1_Pooling/config.json"),
            r#"{"pooling_mode_cls_token": true}"#,
        )
        .unwrap();

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let strategy = load_pooling_strategy(dir.path(), model_id);
        assert!(matches!(strategy, PoolingStrategy::Cls));
    }

    #[test]
    fn test_load_pooling_strategy_max_works() {
        let dir = tempdir().unwrap();
        let model_id = "max/model";
        let repo_dir = dir.path().join("models--max--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(snapshots.join("1_Pooling")).unwrap();
        fs::write(
            snapshots.join("1_Pooling/config.json"),
            r#"{"pooling_mode_max_tokens": true}"#,
        )
        .unwrap();

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let strategy = load_pooling_strategy(dir.path(), model_id);
        assert!(matches!(strategy, PoolingStrategy::Max));
    }

    #[test]
    fn test_load_pooling_strategy_invalid_json() {
        let dir = tempdir().unwrap();
        let model_id = "bad/pooling";
        let repo_dir = dir.path().join("models--bad--pooling");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(snapshots.join("1_Pooling")).unwrap();
        fs::write(
            snapshots.join("1_Pooling/config.json"),
            r#"{"pooling_mode_cls_token": "#,
        )
        .unwrap();

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let strategy = load_pooling_strategy(dir.path(), model_id);
        assert!(matches!(strategy, PoolingStrategy::Mean));
    }

    #[test]
    fn test_cached_size_bytes() {
        let dir = tempdir().unwrap();
        let model_id = "size/model";
        let repo_dir = dir.path().join("models--size--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();

        fs::write(snapshots.join("config.json"), "abc").unwrap(); // 3 bytes
        fs::write(snapshots.join("tokenizer.json"), "defg").unwrap(); // 4 bytes
        fs::write(snapshots.join("model.safetensors"), "h").unwrap(); // 1 byte

        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let size = cached_size_bytes(dir.path(), model_id).unwrap();
        assert_eq!(size, 8);
    }

    #[test]
    fn test_candle_installer_basics() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer =
            CandleInstaller::new(EmbedderModel("m1".to_string()), manager, "cpu".to_string());
        assert_eq!(installer.model.0, "m1");

        assert!(!installer.is_available(dir.path()));

        // Mock it
        let repo_dir = dir.path().join("models--m1");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), "{}").unwrap();
        fs::write(snapshots.join("tokenizer.json"), "{}").unwrap();
        fs::write(snapshots.join("model.safetensors"), "{}").unwrap();
        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        assert!(installer.is_available(dir.path()));
    }

    #[tokio::test]
    async fn test_candle_installer_install_skip() {
        std::env::set_var("HF_HUB_OFFLINE", "1");
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer =
            CandleInstaller::new(EmbedderModel("m1".to_string()), manager, "cpu".to_string());

        // Mock it so it skips download
        let repo_dir = dir.path().join("models--m1");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), "{}").unwrap();
        fs::write(snapshots.join("tokenizer.json"), "{}").unwrap();
        fs::write(snapshots.join("model.safetensors"), "{}").unwrap();
        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        installer.install(dir.path(), tx).await.unwrap();
        std::env::remove_var("HF_HUB_OFFLINE");
    }

    #[test]
    fn test_cached_size_bytes_none() {
        let dir = tempdir().unwrap();
        let size = cached_size_bytes(dir.path(), "non-existent");
        assert!(size.is_none());
    }

    #[test]
    fn test_pool_cls() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        // (B=1, S=2, H=3)
        let embeddings_t = Tensor::new(
            &[[[1.0f32, 2.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]],
            &device,
        )
        .unwrap();
        let mask = Tensor::new(&[[1.0f32, 1.0f32]], &device).unwrap();

        let tokenizer_json = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{},"merges":[]}}"#;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json.as_bytes()).unwrap();

        // BertModel::load needs these tensors (minimal set)
        let mut tensors = std::collections::HashMap::new();
        tensors.insert(
            "embeddings.word_embeddings.weight".to_string(),
            Tensor::zeros((1, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.position_embeddings.weight".to_string(),
            Tensor::zeros((512, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.token_type_embeddings.weight".to_string(),
            Tensor::zeros((2, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.weight".to_string(),
            Tensor::ones(3, dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.bias".to_string(),
            Tensor::zeros(3, dtype, &device).unwrap(),
        );

        let config = BertConfig {
            num_hidden_layers: 0,
            hidden_size: 3,
            intermediate_size: 3,
            num_attention_heads: 1,
            vocab_size: 1, // MATCH the tensor shape [1, 3]
            ..BertConfig::default()
        };

        let vb = VarBuilder::from_tensors(tensors, dtype, &device);
        let model = BertModel::load(vb, &config).unwrap();

        // Mock embedder enough to call pool
        let embedder = CandleEmbedder {
            model: LoadedModel::Bert(model),
            tokenizer,
            device: device.clone(),
            dtype,
            model_id: "m".to_string(),
            dimension: 3,
            pooling: PoolingStrategy::Cls,
            query_prefix: "".to_string(),
            passage_prefix: "".to_string(),
        };

        let pooled = embedder.pool(&embeddings_t, &mask).unwrap();
        // Should take the first token: [1, 2, 3]
        assert_eq!(pooled.to_vec2::<f32>().unwrap(), vec![vec![1.0, 2.0, 3.0]]);
    }

    #[test]
    fn test_pool_mean() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        // (B=1, S=2, H=3)
        let embeddings_t = Tensor::new(
            &[[[1.0f32, 2.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]],
            &device,
        )
        .unwrap();
        let mask = Tensor::new(&[[1.0f32, 1.0f32]], &device).unwrap();

        let tokenizer_json = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{},"merges":[]}}"#;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json.as_bytes()).unwrap();

        let mut tensors = std::collections::HashMap::new();
        tensors.insert(
            "embeddings.word_embeddings.weight".to_string(),
            Tensor::zeros((1, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.position_embeddings.weight".to_string(),
            Tensor::zeros((512, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.token_type_embeddings.weight".to_string(),
            Tensor::zeros((2, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.weight".to_string(),
            Tensor::ones(3, dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.bias".to_string(),
            Tensor::zeros(3, dtype, &device).unwrap(),
        );
        let config = BertConfig {
            num_hidden_layers: 0,
            hidden_size: 3,
            intermediate_size: 3,
            num_attention_heads: 1,
            vocab_size: 1, // MATCH tensor [1, 3]
            ..BertConfig::default()
        };
        let vb = VarBuilder::from_tensors(tensors, dtype, &device);
        let model = BertModel::load(vb, &config).unwrap();

        let embedder = CandleEmbedder {
            model: LoadedModel::Bert(model),
            tokenizer,
            device: device.clone(),
            dtype,
            model_id: "m".to_string(),
            dimension: 3,
            pooling: PoolingStrategy::Mean,
            query_prefix: "".to_string(),
            passage_prefix: "".to_string(),
        };

        let pooled = embedder.pool(&embeddings_t, &mask).unwrap();
        // Mean of [1,2,3] and [4,5,6] is [2.5, 3.5, 4.5]
        assert_eq!(pooled.to_vec2::<f32>().unwrap(), vec![vec![2.5, 3.5, 4.5]]);
    }

    #[test]
    fn test_pool_max() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        // (B=1, S=2, H=3)
        let embeddings_t = Tensor::new(
            &[[[1.0f32, 10.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]],
            &device,
        )
        .unwrap();
        let mask = Tensor::new(&[[1.0f32, 1.0f32]], &device).unwrap();

        let tokenizer_json = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{},"merges":[]}}"#;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json.as_bytes()).unwrap();

        let mut tensors = std::collections::HashMap::new();
        tensors.insert(
            "embeddings.word_embeddings.weight".to_string(),
            Tensor::zeros((1, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.position_embeddings.weight".to_string(),
            Tensor::zeros((512, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.token_type_embeddings.weight".to_string(),
            Tensor::zeros((2, 3), dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.weight".to_string(),
            Tensor::ones(3, dtype, &device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.bias".to_string(),
            Tensor::zeros(3, dtype, &device).unwrap(),
        );
        let config = BertConfig {
            num_hidden_layers: 0,
            hidden_size: 3,
            intermediate_size: 3,
            num_attention_heads: 1,
            vocab_size: 1, // MATCH tensor [1, 3]
            ..BertConfig::default()
        };
        let vb = VarBuilder::from_tensors(tensors, dtype, &device);
        let model = BertModel::load(vb, &config).unwrap();

        let embedder = CandleEmbedder {
            model: LoadedModel::Bert(model),
            tokenizer,
            device: device.clone(),
            dtype,
            model_id: "m".to_string(),
            dimension: 3,
            pooling: PoolingStrategy::Max,
            query_prefix: "".to_string(),
            passage_prefix: "".to_string(),
        };

        let pooled = embedder.pool(&embeddings_t, &mask).unwrap();
        // Max of [1,10,3] and [4,5,6] is [4, 10, 6]
        assert_eq!(pooled.to_vec2::<f32>().unwrap(), vec![vec![4.0, 10.0, 6.0]]);
    }

    #[test]
    fn test_list_supported_models_cached_size() {
        let dir = tempdir().unwrap();
        let model_id = "BAAI/bge-base-en-v1.5";
        let repo_dir = dir
            .path()
            .join(format!("models--{}", model_id.replace('/', "--")));
        let snapshots = repo_dir.join("snapshots").join("main");
        std::fs::create_dir_all(&snapshots).unwrap();

        let files = [
            ("config.json", r#"{"hidden_size": 768}"#),
            ("tokenizer.json", r#"{"dummy":true}"#),
            ("model.safetensors", "weights"),
        ];
        let mut expected_size = 0u64;
        for (name, content) in files {
            let path = snapshots.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
            expected_size += content.len() as u64;
        }

        let refs = repo_dir.join("refs");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("main"), "main").unwrap();

        let models = list_supported_models(dir.path());
        let model = models
            .iter()
            .find(|m| m.model_id == model_id)
            .expect("expected cached Candle model");
        assert!(model.is_cached);
        assert_eq!(model.size_bytes, Some(expected_size));
    }
}
