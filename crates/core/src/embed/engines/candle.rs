use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use tracing::warn;
use tracing::{debug, info};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandleArtifacts {
    pub config_path: PathBuf,
    pub tokenizer_path: PathBuf,
    pub weights_path: PathBuf,
    pub pooling_config_path: Option<PathBuf>,
}

pub fn resolve_cached_artifacts(
    data_dir: &Path,
    model_id: &str,
) -> anyhow::Result<CandleArtifacts> {
    Ok(CandleArtifacts {
        config_path: cached_path(data_dir, model_id, "config.json")
            .ok_or_else(|| anyhow::anyhow!("config.json not cached for '{model_id}'"))?,
        tokenizer_path: cached_path(data_dir, model_id, "tokenizer.json")
            .ok_or_else(|| anyhow::anyhow!("tokenizer.json not cached for '{model_id}'"))?,
        weights_path: cached_path(data_dir, model_id, "model.safetensors")
            .ok_or_else(|| anyhow::anyhow!("model.safetensors not cached for '{model_id}'"))?,
        pooling_config_path: cached_path(data_dir, model_id, "1_Pooling/config.json"),
    })
}

pub fn read_config_text(path: &Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct ModelTypePeek {
    pub model_type: String,
}

pub fn parse_model_type(config_text: &str) -> anyhow::Result<ModelTypePeek> {
    Ok(serde_json::from_str(config_text)?)
}

pub fn parse_dimension_from_config(config_text: &str) -> anyhow::Result<usize> {
    let v: serde_json::Value = serde_json::from_str(config_text)?;
    v.get("hidden_size")
        .and_then(|v| v.as_u64())
        .map(|d| d as usize)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine embedding dimension from config"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandleDevicePlan {
    Cpu,
    MetalPreferred,
}

pub fn select_device_plan(device: &str) -> CandleDevicePlan {
    if device.trim().eq_ignore_ascii_case("cpu") {
        CandleDevicePlan::Cpu
    } else {
        CandleDevicePlan::MetalPreferred
    }
}

pub fn select_dtype_for_plan(_plan: &CandleDevicePlan) -> DType {
    DType::F32
}

pub fn realize_device(plan: CandleDevicePlan) -> Device {
    match plan {
        CandleDevicePlan::Cpu => Device::Cpu,
        CandleDevicePlan::MetalPreferred => {
            #[cfg(target_os = "macos")]
            {
                if candle_core::utils::metal_is_available() {
                    match std::panic::catch_unwind(|| Device::new_metal(0)) {
                        Ok(Ok(d)) => return d,
                        Ok(Err(e)) => {
                            warn!("Metal device init failed ({e:#}), falling back to CPU")
                        }
                        Err(_) => warn!("Metal device init panicked, falling back to CPU"),
                    }
                }
            }
            Device::Cpu
        }
    }
}

pub type CandleVarBuilder<'a> = VarBuilder<'a>;

pub(crate) trait CandleRuntimeFactory {
    fn load_var_builder<'a>(
        &self,
        weights_path: &'a Path,
        dtype: DType,
        device: &'a Device,
    ) -> anyhow::Result<CandleVarBuilder<'a>>;

    fn build_loaded_model<'a>(
        &self,
        model_type: &str,
        config_text: &str,
        vb: CandleVarBuilder<'a>,
    ) -> anyhow::Result<(LoadedModel, usize)>;

    fn load_tokenizer(&self, tokenizer_path: &Path) -> anyhow::Result<Tokenizer>;
}

struct RealCandleRuntimeFactory;

impl CandleRuntimeFactory for RealCandleRuntimeFactory {
    fn load_var_builder<'a>(
        &self,
        weights_path: &'a Path,
        dtype: DType,
        device: &'a Device,
    ) -> anyhow::Result<CandleVarBuilder<'a>> {
        // Safety: memory-mapping model weights from a local path we own.
        Ok(unsafe { VarBuilder::from_mmaped_safetensors(&[weights_path], dtype, device)? })
    }

    fn build_loaded_model<'a>(
        &self,
        model_type: &str,
        config_text: &str,
        vb: CandleVarBuilder<'a>,
    ) -> anyhow::Result<(LoadedModel, usize)> {
        match model_type {
            "jina_bert_v2" | "jina_bert" | "jina_bert_v3" | "qwen2" | "qwen"
            | "jina_embeddings_v5" => {
                let config: JinaBertConfig = serde_json::from_str(config_text)
                    .with_context(|| format!("Failed to parse JinaBertConfig: {config_text}"))?;
                let dim = config.hidden_size;
                let m =
                    JinaBertModel::new(vb, &config).context("Failed to build JinaBert model")?;
                Ok((LoadedModel::JinaBert(m), dim))
            }
            "modern_bert" => {
                let config: ModernBertConfig = serde_json::from_str(config_text)
                    .with_context(|| format!("Failed to parse ModernBertConfig: {config_text}"))?;
                let dim = config.hidden_size;
                let m =
                    ModernBert::load(vb, &config).context("Failed to build ModernBert model")?;
                Ok((LoadedModel::ModernBert(m), dim))
            }
            _ => {
                let config: BertConfig = serde_json::from_str(config_text)
                    .with_context(|| format!("Failed to parse BertConfig: {config_text}"))?;
                let dim = config.hidden_size;
                let m = BertModel::load(vb, &config).context("Failed to build BERT model")?;
                Ok((LoadedModel::Bert(m), dim))
            }
        }
    }

    fn load_tokenizer(&self, tokenizer_path: &Path) -> anyhow::Result<Tokenizer> {
        Tokenizer::from_file(tokenizer_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to load tokenizer from {}: {e}",
                tokenizer_path.display()
            )
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CandleEmbedderBuildPlan {
    pub model_id: String,
    pub dimension: usize,
    pub(crate) pooling: PoolingStrategy,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub device_plan: CandleDevicePlan,
    pub dtype: DType,
}

pub(crate) fn build_embedder_plan(
    data_dir: &Path,
    model_id: &str,
    config_text: &str,
    device: &str,
) -> anyhow::Result<CandleEmbedderBuildPlan> {
    let dimension = parse_dimension_from_config(config_text)?;
    let pooling = load_pooling_strategy(data_dir, model_id);
    let prefixes = super::aux_config::load_prefixes(data_dir, model_id);
    let device_plan = select_device_plan(device);
    let dtype = select_dtype_for_plan(&device_plan);
    Ok(CandleEmbedderBuildPlan {
        model_id: model_id.to_string(),
        dimension,
        pooling,
        query_prefix: prefixes.query_prefix,
        passage_prefix: prefixes.passage_prefix,
        device_plan,
        dtype,
    })
}

pub(crate) fn assemble_candle_embedder(
    plan: CandleEmbedderBuildPlan,
    model: LoadedModel,
    tokenizer: Tokenizer,
    device: Device,
) -> CandleEmbedder {
    CandleEmbedder {
        model,
        tokenizer,
        device,
        dtype: plan.dtype,
        model_id: plan.model_id,
        dimension: plan.dimension,
        pooling: plan.pooling,
        query_prefix: plan.query_prefix,
        passage_prefix: plan.passage_prefix,
    }
}

pub(crate) trait HfModelFetcher {
    fn download_required_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()>;
    fn fetch_optional_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()>;
}

struct RealHfModelFetcher {
    cache_dir: PathBuf,
}

impl HfModelFetcher for RealHfModelFetcher {
    fn download_required_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()> {
        let api = ApiBuilder::new()
            .with_cache_dir(self.cache_dir.clone())
            .build()
            .context("Failed to initialise HF hub API")?;
        let repo = api.model(model_id.to_string());
        for filename in files {
            let url = repo.url(filename);
            repo.get(filename).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to download '{filename}' for '{model_id}' from {url}: {e:#}"
                )
            })?;
        }
        Ok(())
    }

    fn fetch_optional_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()> {
        let api = ApiBuilder::new()
            .with_cache_dir(self.cache_dir.clone())
            .build()
            .context("Failed to initialise HF hub API")?;
        let repo = api.model(model_id.to_string());
        for filename in files {
            let _ = repo.get(filename);
        }
        Ok(())
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
pub(crate) enum PoolingStrategy {
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

#[cfg_attr(not(test), allow(dead_code))]
fn select_device(device: &str) -> Device {
    realize_device(select_device_plan(device))
}

/// F32 everywhere. Candle 0.9's Metal backend lacks F16 kernels for several ops
/// (layer norm, softmax, GELU), causing silent GPU→CPU→GPU roundtrips that
/// dominate runtime. F32 has full Metal kernel coverage, keeping all operations
/// on the GPU. The 2× memory increase is negligible for embedding-sized models.
#[cfg_attr(not(test), allow(dead_code))]
fn select_dtype(_device: &Device) -> DType {
    select_dtype_for_plan(&CandleDevicePlan::Cpu)
}

// ── CandleEmbedder ────────────────────────────────────────────────────────────

/// Batches of 32 balance Metal dispatch overhead against intermediate tensor
/// memory (attention matrices scale with batch × seq_len²). Very large batches
/// can stall Metal due to memory pressure.
const EMBED_BATCH_SIZE: usize = 32;
const MAX_SEQUENCE_LENGTH: usize = 512;

pub(crate) enum LoadedModel {
    Bert(BertModel),
    JinaBert(JinaBertModel),
    ModernBert(ModernBert),
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

        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: false,
            }))
            .await;

        let fetcher = RealHfModelFetcher {
            cache_dir: data_dir.clone(),
        };
        let model_id_required = model_id.clone();
        tokio::task::spawn_blocking(move || {
            fetcher.download_required_files(&model_id_required, MODEL_FILES)
        })
        .await??;

        let fetcher = RealHfModelFetcher {
            cache_dir: data_dir.clone(),
        };
        let aux_model_id = model_id.clone();
        tokio::task::spawn_blocking(move || {
            fetcher.fetch_optional_files(&aux_model_id, &["1_Pooling/config.json"])
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
    parse_dimension_from_config(&read_config_text(&config_path)?)
}

/// Load a `CandleEmbedder` directly in the calling process.
/// Only called from the worker subprocess, never from the main Tauri process.
pub fn load_embedder(
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Embedder>> {
    load_embedder_with_factory(&RealCandleRuntimeFactory, model, data_dir, device)
}

fn load_embedder_with_factory<F: CandleRuntimeFactory>(
    factory: &F,
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Embedder>> {
    let artifacts = resolve_cached_artifacts(data_dir, &model.0)?;
    let config_text = read_config_text(&artifacts.config_path)?;
    let peek = parse_model_type(&config_text)?;
    let plan = build_embedder_plan(data_dir, &model.0, &config_text, device)?;
    let device = realize_device(plan.device_plan);
    let dtype = plan.dtype;

    let vb = factory.load_var_builder(&artifacts.weights_path, dtype, &device)?;
    info!(
        "[candle] dispatching '{}' with model_type='{}'",
        model.0, peek.model_type
    );
    let (model_loaded, dimension) =
        factory.build_loaded_model(&peek.model_type, &config_text, vb)?;
    let mut tokenizer = factory.load_tokenizer(&artifacts.tokenizer_path)?;

    tokenizer.with_padding(Some(tokenizers::PaddingParams {
        strategy: tokenizers::PaddingStrategy::BatchLongest,
        ..Default::default()
    }));
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: MAX_SEQUENCE_LENGTH,
            ..Default::default()
        }))
        .map_err(|e| anyhow::anyhow!("Failed to configure truncation for '{}': {e}", model.0))?;

    info!(
        "[candle] loaded '{}' dim={dimension} pooling={:?} device={device:?} dtype={dtype:?}",
        model.0, plan.pooling
    );
    Ok(Arc::new(assemble_candle_embedder(
        CandleEmbedderBuildPlan { dimension, ..plan },
        model_loaded,
        tokenizer,
        device,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    fn minimal_bert_config_json() -> &'static str {
        r#"{
            "vocab_size": 1,
            "hidden_size": 3,
            "num_hidden_layers": 0,
            "num_attention_heads": 1,
            "intermediate_size": 3,
            "hidden_act": "gelu",
            "hidden_dropout_prob": 0.1,
            "max_position_embeddings": 512,
            "type_vocab_size": 2,
            "initializer_range": 0.02,
            "layer_norm_eps": 1e-12,
            "pad_token_id": 0,
            "position_embedding_type": "absolute",
            "use_cache": false,
            "classifier_dropout": null,
            "model_type": "bert"
        }"#
    }

    fn minimal_jina_config_json() -> &'static str {
        r#"{
            "vocab_size": 1,
            "hidden_size": 3,
            "num_hidden_layers": 1,
            "num_attention_heads": 1,
            "intermediate_size": 3,
            "hidden_act": "gelu",
            "hidden_dropout_prob": 0.1,
            "attention_probs_dropout_prob": 0.1,
            "max_position_embeddings": 512,
            "type_vocab_size": 2,
            "initializer_range": 0.02,
            "layer_norm_eps": 1e-12,
            "position_embedding_type": "alibi",
            "pad_token_id": 0
        }"#
    }

    fn minimal_modern_config_json() -> &'static str {
        r#"{
            "vocab_size": 1,
            "hidden_size": 3,
            "num_hidden_layers": 1,
            "num_attention_heads": 1,
            "intermediate_size": 3,
            "hidden_act": "gelu",
            "max_position_embeddings": 512,
            "initializer_range": 0.02,
            "norm_eps": 1e-12,
            "layer_norm_eps": 1e-12,
            "model_type": "modern_bert",
            "pad_token_id": 0,
            "global_attn_every_n_layers": 1,
            "global_rope_theta": 1.0,
            "local_attention": 1,
            "local_rope_theta": 1.0
        }"#
    }

    fn minimal_bert_tensors(dtype: DType, device: &Device) -> HashMap<String, Tensor> {
        let mut tensors = HashMap::new();
        tensors.insert(
            "embeddings.word_embeddings.weight".to_string(),
            Tensor::zeros((1, 3), dtype, device).unwrap(),
        );
        tensors.insert(
            "embeddings.position_embeddings.weight".to_string(),
            Tensor::zeros((512, 3), dtype, device).unwrap(),
        );
        tensors.insert(
            "embeddings.token_type_embeddings.weight".to_string(),
            Tensor::zeros((2, 3), dtype, device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.weight".to_string(),
            Tensor::ones(3, dtype, device).unwrap(),
        );
        tensors.insert(
            "embeddings.LayerNorm.bias".to_string(),
            Tensor::zeros(3, dtype, device).unwrap(),
        );
        tensors
    }

    fn write_cached_candle_artifacts(dir: &tempfile::TempDir, model_id: &str, config: &str) {
        let repo_dir = dir
            .path()
            .join(format!("models--{}", model_id.replace('/', "--")));
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), config).unwrap();
        fs::write(
            snapshots.join("tokenizer.json"),
            r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{},"merges":[]}}"#,
        )
        .unwrap();
        fs::write(snapshots.join("model.safetensors"), "weights").unwrap();
        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();
    }

    struct MockCandleRuntimeFactory;

    impl CandleRuntimeFactory for MockCandleRuntimeFactory {
        fn load_var_builder<'a>(
            &self,
            _weights_path: &'a Path,
            dtype: DType,
            device: &'a Device,
        ) -> anyhow::Result<CandleVarBuilder<'a>> {
            Ok(VarBuilder::from_tensors(
                minimal_bert_tensors(dtype, device),
                dtype,
                device,
            ))
        }

        fn build_loaded_model<'a>(
            &self,
            _model_type: &str,
            config_text: &str,
            vb: CandleVarBuilder<'a>,
        ) -> anyhow::Result<(LoadedModel, usize)> {
            let config: BertConfig = serde_json::from_str(config_text)?;
            let dim = config.hidden_size;
            let model = BertModel::load(vb, &config)?;
            Ok((LoadedModel::Bert(model), dim))
        }

        fn load_tokenizer(&self, tokenizer_path: &Path) -> anyhow::Result<Tokenizer> {
            Tokenizer::from_file(tokenizer_path).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to load tokenizer from {}: {e}",
                    tokenizer_path.display()
                )
            })
        }
    }

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
        // On systems without a usable Metal device, it should fallback to Cpu.
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
    fn test_candle_embedder_prefixes() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        let tokenizer_json = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{"[PAD]":0,"[CLS]":1,"[SEP]":2,"q":3,"p":4,"t":5},"merges":[]}}"#;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json.as_bytes()).unwrap();

        let mut tensors = std::collections::HashMap::new();
        tensors.insert(
            "embeddings.word_embeddings.weight".to_string(),
            Tensor::zeros((6, 3), dtype, &device).unwrap(),
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
            vocab_size: 6,
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
            query_prefix: "q".to_string(),
            passage_prefix: "p".to_string(),
        };

        // These call embed_with_prefix
        let q = embedder.embed_query(&["t"]).unwrap();
        assert_eq!(q.len(), 1);
        let p = embedder.embed_passages(&["t"]).unwrap();
        assert_eq!(p.len(), 1);
        
        assert_eq!(embedder.model_id(), "m");
        assert_eq!(embedder.dimension(), 3);
        assert_eq!(embedder.preferred_batch_size(), Some(32));
    }

    #[test]
    fn test_candle_embedder_empty_seq() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        let tokenizer_json = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":null,"post_processor":null,"decoder":null,"model":{"type":"BPE","dropout":null,"unk_token":null,"continuing_subword_prefix":null,"end_of_word_suffix":null,"fuse_unk":null,"byte_fallback":null,"vocab":{},"merges":[]}}"#;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json.as_bytes()).unwrap();

        let mut tensors = std::collections::HashMap::new();
        tensors.insert("embeddings.word_embeddings.weight".to_string(), Tensor::zeros((1, 3), dtype, &device).unwrap());
        tensors.insert("embeddings.position_embeddings.weight".to_string(), Tensor::zeros((512, 3), dtype, &device).unwrap());
        tensors.insert("embeddings.token_type_embeddings.weight".to_string(), Tensor::zeros((2, 3), dtype, &device).unwrap());
        tensors.insert("embeddings.LayerNorm.weight".to_string(), Tensor::ones(3, dtype, &device).unwrap());
        tensors.insert("embeddings.LayerNorm.bias".to_string(), Tensor::zeros(3, dtype, &device).unwrap());
        let config = BertConfig { num_hidden_layers: 0, hidden_size: 3, intermediate_size: 3, num_attention_heads: 1, vocab_size: 1, ..BertConfig::default() };
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

        // embed_batch should return zeros if seq_len is 0
        let res = embedder.embed(&[]).unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn test_fetch_aux_configs_basic() {
        let dir = tempdir().unwrap();
        // Returns () and logs errors, just verify it doesn't panic
        crate::embed::engines::aux_config::fetch_aux_configs(dir.path(), "non-existent");
    }

    #[test]
    fn test_fetch_model_size_with_empty_result() {
        let res = crate::embed::models::hf_hub::fetch_model_size("non-existent");
        assert!(res.is_err());
    }

    #[test]
    fn test_read_dimension_static_variants() {
        // Test a few from the static list
        assert_eq!(read_dimension(Path::new("."), "BAAI/bge-base-en-v1.5").unwrap(), 768);
        assert_eq!(read_dimension(Path::new("."), "sentence-transformers/all-MiniLM-L12-v2").unwrap(), 384);
        assert_eq!(read_dimension(Path::new("."), "intfloat/multilingual-e5-large-instruct").unwrap(), 1024);
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

    #[test]
    fn test_resolve_cached_artifacts_and_build_embedder_plan() {
        let dir = tempdir().unwrap();
        let model_id = "custom/model";
        let repo_dir = dir.path().join("models--custom--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(snapshots.join("1_Pooling")).unwrap();
        fs::write(snapshots.join("config.json"), r#"{"hidden_size": 1024}"#).unwrap();
        fs::write(snapshots.join("tokenizer.json"), "{}").unwrap();
        fs::write(snapshots.join("model.safetensors"), "weights").unwrap();
        fs::write(
            snapshots.join("1_Pooling/config.json"),
            r#"{"pooling_mode_cls_token": true}"#,
        )
        .unwrap();
        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        let artifacts = resolve_cached_artifacts(dir.path(), model_id).unwrap();
        assert!(artifacts.config_path.ends_with("config.json"));
        assert!(artifacts.pooling_config_path.is_some());

        let plan =
            build_embedder_plan(dir.path(), model_id, r#"{"hidden_size":1024}"#, "cpu").unwrap();
        assert_eq!(plan.model_id, model_id);
        assert_eq!(plan.dimension, 1024);
        assert_eq!(plan.device_plan, CandleDevicePlan::Cpu);
        assert_eq!(plan.dtype, DType::F32);
    }

    #[test]
    fn test_resolve_cached_artifacts_missing_files() {
        let dir = tempdir().unwrap();
        let model_id = "custom/model";
        let repo_dir = dir.path().join("models--custom--model");
        let snapshots = repo_dir.join("snapshots").join("main");
        fs::create_dir_all(&snapshots).unwrap();
        fs::write(snapshots.join("config.json"), "{}").unwrap();
        fs::write(snapshots.join("tokenizer.json"), "{}").unwrap();
        fs::write(snapshots.join("model.safetensors"), "{}").unwrap();
        let refs = repo_dir.join("refs");
        fs::create_dir_all(&refs).unwrap();
        fs::write(refs.join("main"), "main").unwrap();

        fs::remove_file(snapshots.join("config.json")).unwrap();
        assert!(resolve_cached_artifacts(dir.path(), model_id).is_err());

        fs::write(snapshots.join("config.json"), "{}").unwrap();
        fs::remove_file(snapshots.join("tokenizer.json")).unwrap();
        assert!(resolve_cached_artifacts(dir.path(), model_id).is_err());

        fs::write(snapshots.join("tokenizer.json"), "{}").unwrap();
        fs::remove_file(snapshots.join("model.safetensors")).unwrap();
        assert!(resolve_cached_artifacts(dir.path(), model_id).is_err());
    }

    #[test]
    fn test_read_config_text_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let err = read_config_text(&path).unwrap_err();
        assert!(err.to_string().contains("Failed to read"));
    }

    #[test]
    fn test_parse_model_type_and_dimension_helpers() {
        let peek = parse_model_type(r#"{"model_type":"bert"}"#).unwrap();
        assert_eq!(peek.model_type, "bert");
        assert_eq!(
            parse_dimension_from_config(r#"{"hidden_size":384}"#).unwrap(),
            384
        );
        assert!(parse_dimension_from_config(r#"{"no_hidden_size":true}"#).is_err());
    }

    #[test]
    fn test_select_device_plan_and_realize_device() {
        assert_eq!(select_device_plan("cpu"), CandleDevicePlan::Cpu);
        assert_eq!(select_device_plan("  CPU  "), CandleDevicePlan::Cpu);
        assert_eq!(select_device_plan("gpu"), CandleDevicePlan::MetalPreferred);
        assert_eq!(select_dtype_for_plan(&CandleDevicePlan::Cpu), DType::F32);
        assert!(matches!(realize_device(CandleDevicePlan::Cpu), Device::Cpu));

        let metal = realize_device(CandleDevicePlan::MetalPreferred);
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(metal, Device::Cpu));
        #[cfg(target_os = "macos")]
        assert!(matches!(metal, Device::Cpu | Device::Metal(_)));
    }

    #[test]
    fn test_real_candle_runtime_factory_dispatches_model_variants() {
        let device = Device::Cpu;
        let dtype = DType::F32;
        let factory = RealCandleRuntimeFactory;

        let bert_vb =
            VarBuilder::from_tensors(minimal_bert_tensors(dtype, &device), dtype, &device);
        let (model, dim) = factory
            .build_loaded_model("bert", minimal_bert_config_json(), bert_vb)
            .unwrap();
        assert_eq!(dim, 3);
        match model {
            LoadedModel::Bert(_) => {}
            _ => panic!("expected Bert model"),
        }

        let jina_vb = VarBuilder::from_tensors(HashMap::new(), dtype, &device);
        assert!(factory
            .build_loaded_model("jina_bert_v2", minimal_jina_config_json(), jina_vb)
            .is_err());

        let modern_vb = VarBuilder::from_tensors(HashMap::new(), dtype, &device);
        assert!(factory
            .build_loaded_model("modern_bert", minimal_modern_config_json(), modern_vb)
            .is_err());
    }

    #[tokio::test]
    async fn test_load_embedder_with_factory_happy_path() {
        let dir = tempdir().unwrap();
        let model_id = "custom/model";
        write_cached_candle_artifacts(&dir, model_id, minimal_bert_config_json());

        let model = EmbedderModel(model_id.to_string());
        let embedder =
            load_embedder_with_factory(&MockCandleRuntimeFactory, &model, dir.path(), "cpu")
                .unwrap();

        assert_eq!(embedder.model_id(), model_id);
        assert_eq!(embedder.dimension(), 3);
        assert!(matches!(embedder.engine(), EmbeddingEngine::Candle));
    }

    #[tokio::test]
    async fn test_load_embedder_with_factory_reports_tokenizer_error() {
        struct FailingTokenizerFactory;

        impl CandleRuntimeFactory for FailingTokenizerFactory {
            fn load_var_builder<'a>(
                &self,
                _weights_path: &'a Path,
                dtype: DType,
                device: &'a Device,
            ) -> anyhow::Result<CandleVarBuilder<'a>> {
                Ok(VarBuilder::from_tensors(
                    minimal_bert_tensors(dtype, device),
                    dtype,
                    device,
                ))
            }

            fn build_loaded_model<'a>(
                &self,
                _model_type: &str,
                _config_text: &str,
                _vb: CandleVarBuilder<'a>,
            ) -> anyhow::Result<(LoadedModel, usize)> {
                let device = Device::Cpu;
                let dtype = DType::F32;
                let tensors = minimal_bert_tensors(dtype, &device);
                let vb = VarBuilder::from_tensors(tensors, dtype, &device);
                let config = BertConfig {
                    vocab_size: 1,
                    hidden_size: 3,
                    num_hidden_layers: 0,
                    num_attention_heads: 1,
                    intermediate_size: 3,
                    hidden_act: candle_transformers::models::bert::HiddenAct::Gelu,
                    hidden_dropout_prob: 0.1,
                    max_position_embeddings: 512,
                    type_vocab_size: 2,
                    initializer_range: 0.02,
                    layer_norm_eps: 1e-12,
                    pad_token_id: 0,
                    position_embedding_type:
                        candle_transformers::models::bert::PositionEmbeddingType::Absolute,
                    use_cache: false,
                    classifier_dropout: None,
                    model_type: Some("bert".to_string()),
                };
                let model = BertModel::load(vb, &config)?;
                Ok((LoadedModel::Bert(model), 3))
            }

            fn load_tokenizer(&self, _tokenizer_path: &Path) -> anyhow::Result<Tokenizer> {
                anyhow::bail!("tokenizer exploded")
            }
        }

        let dir = tempdir().unwrap();
        let model_id = "custom/tokenizer-failure";
        write_cached_candle_artifacts(&dir, model_id, minimal_bert_config_json());

        let model = EmbedderModel(model_id.to_string());
        let err =
            match load_embedder_with_factory(&FailingTokenizerFactory, &model, dir.path(), "cpu") {
                Ok(_) => panic!("expected tokenizer failure"),
                Err(err) => err,
            };
        assert!(err.to_string().contains("tokenizer exploded"));
    }

    #[test]
    fn test_real_hf_model_fetcher_basic() {
        let dir = tempdir().unwrap();
        let fetcher = RealHfModelFetcher {
            cache_dir: dir.path().to_path_buf(),
        };
        // Should not panic, might fail to download but we test the interface
        let _ = fetcher.fetch_optional_files("BAAI/bge-small-en-v1.5", &["README.md"]);
    }
}
