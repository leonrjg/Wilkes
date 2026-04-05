use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::debug;

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::types::{EmbedderModel, ModelDescriptor};
use super::Embedder;
use super::installer::{EmbedProgress, DownloadProgress, EmbedderInstaller, ProgressTx};

// ── Model lookup ──────────────────────────────────────────────────────────────

fn find_model_info(model_id: &str) -> anyhow::Result<fastembed::ModelInfo<EmbeddingModel>> {
    TextEmbedding::list_supported_models()
        .into_iter()
        .find(|m| m.model_code == model_id)
        .ok_or_else(|| anyhow::anyhow!("Model '{}' is not supported by fastembed", model_id))
}

// ── Public list helper ────────────────────────────────────────────────────────

/// Return all fastembed-supported models, annotated with local cache status.
/// For cached models `size_bytes` is computed from disk; for uncached models it is `None`
/// and should be fetched on demand via [`fetch_model_size`].
pub fn list_supported_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    TextEmbedding::list_supported_models()
        .into_iter()
        .map(|info| {
            let all_files: Vec<&str> = std::iter::once(info.model_file.as_str())
                .chain(info.additional_files.iter().map(String::as_str))
                .collect();

            let (is_cached, size_bytes) = {
                let main = hf_hub::Cache::new(data_dir.to_path_buf())
                    .repo(hf_hub::Repo::model(info.model_code.clone()))
                    .get(&info.model_file);
                match main {
                    None => (false, None),
                    Some(_) => {
                        let total: u64 = all_files
                            .iter()
                            .filter_map(|f| {
                                hf_hub::Cache::new(data_dir.to_path_buf())
                                    .repo(hf_hub::Repo::model(info.model_code.clone()))
                                    .get(f)
                                    .and_then(|p| std::fs::metadata(p).ok())
                                    .map(|m| m.len())
                            })
                            .sum();
                        (true, Some(total).filter(|&s| s > 0))
                    }
                }
            };

            let display_name = info
                .model_code
                .split('/')
                .next_back()
                .unwrap_or(&info.model_code)
                .to_string();

            ModelDescriptor {
                model_id: info.model_code.clone(),
                display_name,
                description: info.description.clone(),
                dimension: info.dim,
                is_cached,
                is_default: info.model_code == "BAAI/bge-base-en-v1.5",
                is_recommended: info.model_code == "BAAI/bge-base-en-v1.5" || info.model_code == "sentence-transformers/all-MiniLM-L6-v2",
                size_bytes,
                preferred_batch_size: get_preferred_batch_size(&info.model_code, &info.description),
            }
        })
        .collect()
}

/// Fetch the total download size (in bytes) for `model_id` by querying the HuggingFace API.
pub fn fetch_model_size(model_id: &str) -> anyhow::Result<u64> {
    #[derive(serde::Deserialize)]
    struct Sibling {
        rfilename: String,
        size: Option<u64>,
    }
    #[derive(serde::Deserialize)]
    struct HfModelInfo {
        siblings: Vec<Sibling>,
    }

    let info = find_model_info(model_id)?;

    // fastembed stores bare filenames (e.g. "model.onnx") but some HF repos place
    // them in subdirectories (e.g. "onnx/model.onnx"). Match by the final path
    // component so both layouts are handled.
    let relevant: std::collections::HashSet<&str> = std::iter::once(info.model_file.as_str())
        .chain(info.additional_files.iter().map(String::as_str))
        .collect();

    let matches_relevant = |rfilename: &str| -> bool {
        relevant.contains(rfilename)
            || relevant
                .iter()
                .any(|f| rfilename.ends_with(&format!("/{f}")))
    };

    // ?blobs=true makes HF include actual byte sizes in the siblings listing.
    let url = format!("https://huggingface.co/api/models/{model_id}?blobs=true");
    let hf_info: HfModelInfo = ureq::get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!("HF API request failed: {e}"))?
        .into_json()
        .map_err(|e| anyhow::anyhow!("HF API response parse failed: {e}"))?;

    let total: u64 = hf_info
        .siblings
        .iter()
        .filter(|s| matches_relevant(&s.rfilename))
        .filter_map(|s| s.size)
        .sum();

    anyhow::ensure!(total > 0, "No model files found in HF repo for {model_id}");
    Ok(total)
}

// ── FastEmbedder ──────────────────────────────────────────────────────────────

/// ONNX batch size for CPU inference. Large batches (fastembed's default of 256)
/// exceed L3 cache and thrash; 32 keeps the working set cache-resident.
const DEFAULT_BATCH_SIZE: usize = 32;

fn get_preferred_batch_size(model_id: &str, description: &str) -> Option<usize> {
    // Dynamic quantization (e.g. Q4_K_M, Q4_0, etc) makes embeddings incompatible
    // across batches because the scale factor is adjusted per-batch. These models
    // must be processed as a single batch (None).
    let id_lower = model_id.to_lowercase();
    let desc_lower = description.to_lowercase();

    if id_lower.contains("-q")
        || id_lower.contains("-int8")
        || id_lower.contains("quantized")
        || desc_lower.contains("quantized")
    {
        None
    } else {
        Some(DEFAULT_BATCH_SIZE)
    }
}

pub struct FastEmbedder {
    inner: Mutex<TextEmbedding>,
    model_id: String,
    dimension: usize,
    preferred_batch_size: Option<usize>,
}

impl FastEmbedder {
    /// Returns (query_prefix, passage_prefix) for models that require them.
    fn prefixes(&self) -> (&'static str, &'static str) {
        // E5 family requires explicit prefixes; without them retrieval is unreliable.
        if self.model_id.contains("/multilingual-e5") || self.model_id.contains("/e5-") {
            ("query: ", "passage: ")
        } else {
            ("", "")
        }
    }

    fn embed_with_prefix(&self, texts: &[&str], prefix: &str) -> anyhow::Result<Vec<Vec<f32>>> {
        let total = texts.len();
        debug!("[fastembed] embed: {total} texts, batch_size={:?}", self.preferred_batch_size);
        let t0 = std::time::Instant::now();

        let mut inner = self.inner.lock().unwrap();
        let result = if prefix.is_empty() {
            inner
                .embed(texts.to_vec(), self.preferred_batch_size)
                .map_err(|e| anyhow::anyhow!("fastembed error: {e}"))
        } else {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
            let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
            inner
                .embed(refs, self.preferred_batch_size)
                .map_err(|e| anyhow::anyhow!("fastembed error: {e}"))
        };

        debug!("[fastembed] embed total: {:.1}s", t0.elapsed().as_secs_f64());
        result
    }
}

impl Embedder for FastEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_with_prefix(texts, "")
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn preferred_batch_size(&self) -> Option<usize> {
        self.preferred_batch_size
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (query_prefix, _) = self.prefixes();
        self.embed_with_prefix(texts, query_prefix)
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (_, passage_prefix) = self.prefixes();
        self.embed_with_prefix(texts, passage_prefix)
    }
}

// ── FastembedInstaller ────────────────────────────────────────────────────────

pub struct FastembedInstaller {
    pub model: EmbedderModel,
    pub manager: super::worker_manager::WorkerManager,
    pub device: String,
}

impl FastembedInstaller {
    pub fn new(model: EmbedderModel, manager: super::worker_manager::WorkerManager, device: String) -> Self {
        Self { model, manager, device }
    }
}

#[async_trait]
impl EmbedderInstaller for FastembedInstaller {
    fn is_available(&self, data_dir: &Path) -> bool {
        let Ok(info) = find_model_info(&self.model.0) else { return false; };
        hf_hub::Cache::new(data_dir.to_path_buf())
            .repo(hf_hub::Repo::model(info.model_code))
            .get(&info.model_file)
            .is_some()
    }

    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()> {
        let info = find_model_info(&self.model.0)?;

        // If the main model file is already cached, skip re-initialisation.
        // Calling TextEmbedding::try_new for an already-present model only
        // loads the ONNX runtime (and potentially CoreML), which can crash the
        // process on some configurations. The download step is the only reason
        // to call try_new here.
        if hf_hub::Cache::new(data_dir.to_path_buf())
            .repo(hf_hub::Repo::model(info.model_code.clone()))
            .get(&info.model_file)
            .is_some()
        {
            return Ok(());
        }

        let fm = info.model;
        let cache_dir = data_dir.to_path_buf();
        let device = self.device.clone();

        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: false,
            }))
            .await;

        tokio::task::spawn_blocking(move || {
            let device_clean = device.trim().to_lowercase();
            let providers = if device_clean == "cpu" {
                tracing::info!("[fastembed] install: forcing CPU execution provider");
                vec![ort::ep::CPUExecutionProvider::default().into()]
            } else {
                #[cfg(feature = "fastembed-coreml")]
                {
                    vec![
                        ort::ep::CoreMLExecutionProvider::default().into(),
                        ort::ep::CPUExecutionProvider::default().into(),
                    ]
                }
                #[cfg(not(feature = "fastembed-coreml"))]
                {
                    vec![ort::ep::CPUExecutionProvider::default().into()]
                }
            };
            let options = TextInitOptions::new(fm)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true)
                .with_execution_providers(providers);
            TextEmbedding::try_new(options)
        })
        .await?
        .map_err(|e| anyhow::anyhow!("fastembed install: {e}"))?;

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
        let info = find_model_info(&self.model.0)?;
        Ok(Arc::new(super::sbert::WorkerEmbedder::new(
            self.manager.clone(),
            self.model.0.clone(),
            info.dim,
            self.device.clone(),
            crate::types::EmbeddingEngine::Fastembed,
            data_dir.to_path_buf(),
        )))
    }
}

/// Load a `FastEmbedder` directly in the calling process.
/// Only called from the worker subprocess, never from the main Tauri process.
pub fn load_embedder(model: &EmbedderModel, data_dir: &Path, device: &str) -> anyhow::Result<Arc<dyn Embedder>> {
    let info = find_model_info(&model.0)?;
    let dimension = info.dim;
    let model_id = model.0.clone();
    let cache_dir = data_dir.to_path_buf();
    let preferred_batch_size = get_preferred_batch_size(&model_id, &info.description);

    let device_clean = device.trim().to_lowercase();
    let providers = if device_clean == "cpu" {
        tracing::info!("[fastembed] forcing CPU execution provider for {}", model_id);
        vec![ort::ep::CPUExecutionProvider::default().into()]
    } else {
        #[cfg(feature = "fastembed-coreml")]
        {
            vec![
                ort::ep::CoreMLExecutionProvider::default().into(),
                ort::ep::CPUExecutionProvider::default().into(),
            ]
        }
        #[cfg(not(feature = "fastembed-coreml"))]
        {
            vec![ort::ep::CPUExecutionProvider::default().into()]
        }
    };
    let options = TextInitOptions::new(info.model)
        .with_cache_dir(cache_dir)
        .with_show_download_progress(true)
        .with_execution_providers(providers);

    let inner = TextEmbedding::try_new(options)
        .map_err(|e| anyhow::anyhow!("fastembed load: {e}"))?;

    Ok(Arc::new(FastEmbedder {
        inner: Mutex::new(inner),
        model_id,
        dimension,
        preferred_batch_size,
    }))
}
