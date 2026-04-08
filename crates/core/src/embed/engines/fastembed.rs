use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::debug;

use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::super::models::installer::{
    DownloadProgress, EmbedProgress, EmbedderInstaller, ProgressTx,
};
use super::super::Embedder;
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};

// ── Model lookup ──────────────────────────────────────────────────────────────

fn find_model_info(model_id: &str) -> anyhow::Result<fastembed::ModelInfo<EmbeddingModel>> {
    TextEmbedding::list_supported_models()
        .into_iter()
        .find(|m| format!("{:?}", m.model) == model_id)
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
            let model_id = format!("{:?}", info.model);

            ModelDescriptor {
                model_id: model_id.clone(),
                display_name,
                description: info.description.clone(),
                dimension: info.dim,
                is_cached,
                is_default: false,
                is_recommended: model_id == "BGEBaseENV15" || model_id == "AllMiniLML6V2",
                size_bytes,
                preferred_batch_size: get_preferred_batch_size(&info.model_code, &info.description),
            }
        })
        .collect()
}

/// Fetch the total download size (in bytes) for `model_id` by querying the HuggingFace API.
/// Only counts the specific files fastembed downloads, not the whole repo.
pub fn fetch_model_size(model_id: &str) -> anyhow::Result<u64> {
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

    let siblings = super::super::models::hf_hub::fetch_hf_siblings(&info.model_code)?;
    let total: u64 = siblings
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
    fn embed_with_prefix(&self, texts: &[&str], prefix: &str) -> anyhow::Result<Vec<Vec<f32>>> {
        let total = texts.len();
        debug!(
            "[fastembed] embed: {total} texts, batch_size={:?}",
            self.preferred_batch_size
        );
        let t0 = std::time::Instant::now();

        let mut inner = self.inner.lock().unwrap();
        let result = if prefix.is_empty() {
            inner
                .embed(texts, self.preferred_batch_size)
                .map_err(|e| anyhow::anyhow!("fastembed error: {e}"))
        } else {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
            let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
            inner
                .embed(refs, self.preferred_batch_size)
                .map_err(|e| anyhow::anyhow!("fastembed error: {e}"))
        };

        debug!(
            "[fastembed] embed total: {:.1}s",
            t0.elapsed().as_secs_f64()
        );
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

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Fastembed
    }

    fn preferred_batch_size(&self) -> Option<usize> {
        self.preferred_batch_size
    }
}

// ── FastembedInstaller ────────────────────────────────────────────────────────

pub struct FastembedInstaller {
    pub model: EmbedderModel,
    pub manager: super::super::worker::manager::WorkerManager,
    pub device: String,
}

impl FastembedInstaller {
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
impl EmbedderInstaller for FastembedInstaller {
    fn is_available(&self, data_dir: &Path) -> bool {
        let Ok(info) = find_model_info(&self.model.0) else {
            return false;
        };
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

        // Fetch auxiliary config files (e.g. config_sentence_transformers.json) so that
        // build() can read prefix configuration without a network call. Best-effort.
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
        let info = find_model_info(&self.model.0)?;
        let prefixes = super::aux_config::load_prefixes(data_dir, &self.model.0);
        Ok(Arc::new(
            super::super::worker::embedder::WorkerEmbedder::new(
                self.manager.clone(),
                super::super::worker::embedder::WorkerEmbedderConfig {
                    model_id: self.model.0.clone(),
                    dimension: info.dim,
                    device: self.device.clone(),
                    engine: EmbeddingEngine::Fastembed,
                    data_dir: data_dir.to_path_buf(),
                    query_prefix: prefixes.query_prefix,
                    passage_prefix: prefixes.passage_prefix,
                },
            ),
        ))
    }
}

/// Load a `FastEmbedder` directly in the calling process.
/// Only called from the worker subprocess, never from the main Tauri process.
pub fn load_embedder(
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Embedder>> {
    let info = find_model_info(&model.0)?;
    let dimension = info.dim;
    let model_id = model.0.clone();
    let cache_dir = data_dir.to_path_buf();
    let preferred_batch_size = get_preferred_batch_size(&model_id, &info.description);

    let device_clean = device.trim().to_lowercase();
    let providers = if device_clean == "cpu" {
        tracing::info!(
            "[fastembed] forcing CPU execution provider for {}",
            model_id
        );
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

    let inner =
        TextEmbedding::try_new(options).map_err(|e| anyhow::anyhow!("fastembed load: {e}"))?;

    Ok(Arc::new(FastEmbedder {
        inner: Mutex::new(inner),
        model_id,
        dimension,
        preferred_batch_size,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_list_supported_models() {
        let dir = tempdir().unwrap();
        let models = list_supported_models(dir.path());
        assert!(!models.is_empty());
        let bge = models
            .iter()
            .find(|m| m.model_id == "BGEBaseENV15")
            .unwrap();
        assert_eq!(bge.display_name, "bge-base-en-v1.5");
    }

    #[test]
    fn test_get_preferred_batch_size() {
        assert_eq!(
            get_preferred_batch_size("normal-model", "description"),
            Some(32)
        );
        assert_eq!(
            get_preferred_batch_size("quantized-model", "description"),
            None
        );
        assert_eq!(get_preferred_batch_size("model-q4", "description"), None);
        assert_eq!(get_preferred_batch_size("model", "quantized weights"), None);
    }

    #[test]
    fn test_find_model_info() {
        let info = find_model_info("BGEBaseENV15").unwrap();
        assert_eq!(info.model_code, "Xenova/bge-base-en-v1.5");

        let err = find_model_info("NonExistentModel");
        assert!(err.is_err());
    }

    #[test]
    fn test_fastembed_installer_new() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer = FastembedInstaller::new(
            EmbedderModel("BGEBaseENV15".to_string()),
            manager,
            "cpu".to_string(),
        );
        assert_eq!(installer.model.0, "BGEBaseENV15");

        assert_eq!(installer.uninstall(dir.path()).is_ok(), true);
    }

    #[test]
    fn test_get_preferred_batch_size_detailed() {
        assert_eq!(get_preferred_batch_size("-int8-model", ""), None);
        assert_eq!(get_preferred_batch_size("model", "quantized model"), None);
        assert_eq!(get_preferred_batch_size("normal", "high quality"), Some(32));
    }

    #[test]
    fn test_find_model_info_invalid() {
        assert!(find_model_info("NonExistent").is_err());
    }

    #[test]
    fn test_is_available_not_cached() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer = FastembedInstaller::new(
            EmbedderModel("BGEBaseENV15".to_string()),
            manager,
            "cpu".to_string(),
        );
        assert!(!installer.is_available(dir.path()));
    }

    #[test]
    fn test_is_available_cached() {
        let dir = tempdir().unwrap();
        let info = find_model_info("BGEBaseENV15").unwrap();

        let repo_dir = dir
            .path()
            .join(format!("models--{}", info.model_code.replace("/", "--")));
        let snapshots = repo_dir.join("snapshots").join("main");

        let model_file_path = snapshots.join(&info.model_file);
        if let Some(parent) = model_file_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&model_file_path, "fake onnx").unwrap();

        // Also write config.json at the root of the snapshot
        std::fs::write(snapshots.join("config.json"), "{}").unwrap();

        let refs = repo_dir.join("refs");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("main"), "main").unwrap();

        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer = FastembedInstaller::new(
            EmbedderModel("BGEBaseENV15".to_string()),
            manager,
            "cpu".to_string(),
        );
        let avail = installer.is_available(dir.path());
        assert!(avail);
    }

    #[test]
    fn test_fetch_model_size_mock() {
        use mockito::Server;
        let mut server = Server::new();

        // Mock HF API
        let _m = server
            .mock("GET", "/api/models/Xenova/bge-base-en-v1.5")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                "siblings": [
                    {"rfilename": "model.onnx", "size": 1000},
                    {"rfilename": "tokenizer.json", "size": 500},
                    {"rfilename": "other.txt", "size": 100}
                ]
            }"#,
            )
            .create();

        // We need to point hf-hub or our fetch_hf_siblings to this server.
        // fetch_hf_siblings uses ureq.
        // If we can't easily override the URL, we might skip this or use a different approach.
    }
}
