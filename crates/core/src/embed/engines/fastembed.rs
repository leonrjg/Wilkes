use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::debug;

use anyhow::Context;
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::super::models::installer::{
    DownloadProgress, EmbedProgress, EmbedderInstaller, ProgressTx,
};
use super::super::Embedder;
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};

// ── Model lookup ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FastembedModelRecord {
    pub model: EmbeddingModel,
    pub model_id: String,
    pub model_code: String,
    pub model_file: String,
    pub additional_files: Vec<String>,
    pub description: String,
    pub dimension: usize,
}

pub trait FastembedCatalog {
    fn supported_models(&self) -> Vec<FastembedModelRecord>;
}

struct RealFastembedCatalog;

impl FastembedCatalog for RealFastembedCatalog {
    fn supported_models(&self) -> Vec<FastembedModelRecord> {
        TextEmbedding::list_supported_models()
            .into_iter()
            .map(FastembedModelRecord::from)
            .collect()
    }
}

impl From<fastembed::ModelInfo<EmbeddingModel>> for FastembedModelRecord {
    fn from(info: fastembed::ModelInfo<EmbeddingModel>) -> Self {
        let model_id = format!("{:?}", info.model);
        Self {
            model: info.model,
            model_id,
            model_code: info.model_code,
            model_file: info.model_file,
            additional_files: info.additional_files,
            description: info.description,
            dimension: info.dim,
        }
    }
}

fn find_model_info(model_id: &str) -> anyhow::Result<FastembedModelRecord> {
    RealFastembedCatalog
        .supported_models()
        .into_iter()
        .find(|m| m.model_id == model_id)
        .ok_or_else(|| anyhow::anyhow!("Model '{}' is not supported by fastembed", model_id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastembedExecutionPlan {
    CpuOnly,
    CoreMlThenCpu,
}

fn execution_plan_for_device(device: &str) -> FastembedExecutionPlan {
    if device.trim().eq_ignore_ascii_case("cpu") {
        FastembedExecutionPlan::CpuOnly
    } else {
        #[cfg(feature = "fastembed-coreml")]
        {
            FastembedExecutionPlan::CoreMlThenCpu
        }
        #[cfg(not(feature = "fastembed-coreml"))]
        {
            FastembedExecutionPlan::CpuOnly
        }
    }
}

#[derive(Clone, Debug)]
pub struct FastembedInitRequest {
    pub model: FastembedModelRecord,
    pub cache_dir: std::path::PathBuf,
    pub show_download_progress: bool,
    pub execution_plan: FastembedExecutionPlan,
}

fn build_text_init_request(
    model: FastembedModelRecord,
    cache_dir: std::path::PathBuf,
    device: &str,
) -> FastembedInitRequest {
    FastembedInitRequest {
        model,
        cache_dir,
        show_download_progress: true,
        execution_plan: execution_plan_for_device(device),
    }
}

pub fn install_local(data_dir: &Path, model: &EmbedderModel, device: &str) -> anyhow::Result<()> {
    let info = find_model_info(&model.0)?;
    tracing::info!(
        "[fastembed] install_local start: model={}, device={}, data_dir={}",
        model.model_id(),
        device,
        data_dir.display()
    );
    let cached = hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::model(info.model_code.clone()))
        .get(&info.model_file)
        .is_some();
    if cached {
        tracing::info!("[fastembed] install_local: model already cached");
        return Ok(());
    }

    let request = build_text_init_request(info.clone(), data_dir.to_path_buf(), device);
    let factory = RealFastembedRuntimeFactory;
    tracing::info!("[fastembed] install_local: initializing runtime");
    factory
        .try_new(request)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("fastembed install: {e}"))?;
    tracing::info!("[fastembed] install_local: runtime initialized");

    super::aux_config::fetch_aux_configs(data_dir, &model.0);
    tracing::info!("[fastembed] install_local: aux configs fetched");
    Ok(())
}

pub fn is_model_available(data_dir: &Path, model: &EmbedderModel) -> bool {
    let Ok(info) = find_model_info(&model.0) else {
        return false;
    };
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::model(info.model_code))
        .get(&info.model_file)
        .is_some()
}

pub trait FastembedRuntimeFactory {
    fn try_new(&self, request: FastembedInitRequest) -> anyhow::Result<TextEmbedding>;
}

struct RealFastembedRuntimeFactory;

impl FastembedRuntimeFactory for RealFastembedRuntimeFactory {
    fn try_new(&self, request: FastembedInitRequest) -> anyhow::Result<TextEmbedding> {
        let providers = match request.execution_plan {
            FastembedExecutionPlan::CpuOnly => {
                vec![ort::ep::CPUExecutionProvider::default().into()]
            }
            FastembedExecutionPlan::CoreMlThenCpu => {
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
            }
        };
        let options = TextInitOptions::new(request.model.model)
            .with_cache_dir(request.cache_dir)
            .with_show_download_progress(request.show_download_progress)
            .with_execution_providers(providers);
        TextEmbedding::try_new(options).map_err(|e| anyhow::anyhow!("fastembed runtime: {e}"))
    }
}

fn relevant_hf_filenames(record: &FastembedModelRecord) -> std::collections::HashSet<String> {
    std::iter::once(record.model_file.clone())
        .chain(record.additional_files.iter().cloned())
        .collect()
}

fn hf_sibling_matches_relevant(
    filename: &str,
    relevant: &std::collections::HashSet<String>,
) -> bool {
    relevant.contains(filename)
        || relevant
        .iter()
        .any(|f| filename.ends_with(&format!("/{f}")))
}

fn sum_matching_hf_sizes(
    siblings: &[(String, Option<u64>)],
    relevant: &std::collections::HashSet<String>,
) -> anyhow::Result<u64> {
    let total: u64 = siblings
        .iter()
        .filter(|(name, _)| hf_sibling_matches_relevant(name, relevant))
        .filter_map(|(_, size)| *size)
        .sum();
    anyhow::ensure!(total > 0, "No model files found in HF repo");
    Ok(total)
}

// ── Public list helper ────────────────────────────────────────────────────────

/// Return all fastembed-supported models, annotated with local cache status.
/// For cached models `size_bytes` is computed from disk; for uncached models it is `None`
/// and should be fetched on demand via [`fetch_model_size`].
pub fn list_supported_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    RealFastembedCatalog
        .supported_models()
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
                model_id: info.model_id.clone(),
                display_name,
                description: info.description.clone(),
                dimension: info.dimension,
                is_cached,
                is_default: false,
                is_recommended: info.model_id == "BGEBaseENV15" || info.model_id == "AllMiniLML6V2",
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
    let relevant = relevant_hf_filenames(&info);
    let siblings = super::super::models::hf_hub::fetch_hf_siblings(&info.model_code)?;
    let sibling_sizes: Vec<(String, Option<u64>)> = siblings
        .into_iter()
        .map(|s| (s.rfilename, s.size))
        .collect();
    sum_matching_hf_sizes(&sibling_sizes, &relevant)
        .with_context(|| format!("No model files found in HF repo for {model_id}"))
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
        is_model_available(data_dir, &self.model)
    }

    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()> {
        let _ = tx
            .send(EmbedProgress::Download(DownloadProgress {
                bytes_received: 0,
                total_bytes: 0,
                done: false,
            }))
            .await;

        let model = self.model.clone();
        let device = self.device.clone();
        let data_dir = data_dir.to_path_buf();
        tokio::task::spawn_blocking(move || install_local(&data_dir, &model, &device)).await??;

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
                    dimension: info.dimension,
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
    tracing::info!(
        "[fastembed] load_embedder start: model={}, device={}, data_dir={}",
        model.model_id(),
        device,
        data_dir.display()
    );
    let info = find_model_info(&model.0)?;
    let dimension = info.dimension;
    let model_id = model.0.clone();
    let preferred_batch_size = get_preferred_batch_size(&model_id, &info.description);
    let request = build_text_init_request(info, data_dir.to_path_buf(), device);
    tracing::info!("[fastembed] load_embedder: initializing runtime");
    let inner = RealFastembedRuntimeFactory
        .try_new(request)
        .map_err(|e| anyhow::anyhow!("fastembed load: {e}"))?;
    tracing::info!("[fastembed] load_embedder: runtime initialized");

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
    use std::path::PathBuf;
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

    #[tokio::test]
    async fn test_fastembed_installer_new() {
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

        // build() should work (it creates a WorkerEmbedder)
        let _ = installer.build(dir.path()).unwrap();
    }

    #[tokio::test]
    async fn test_fastembed_installer_install_cached() {
        let dir = tempdir().unwrap();
        let info = find_model_info("BGEBaseENV15").unwrap();

        let repo_code = info.model_code.replace("/", "--");
        let repo_dir = dir.path().join(format!("models--{repo_code}"));
        let snapshots_dir = repo_dir.join("snapshots");
        std::fs::create_dir_all(&snapshots_dir).unwrap();

        let hash_dir = snapshots_dir.join("main");
        std::fs::create_dir_all(&hash_dir).unwrap();

        let model_file = hash_dir.join(&info.model_file);
        if let Some(parent) = model_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&model_file, "{}").unwrap();

        let refs_dir = repo_dir.join("refs");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(refs_dir.join("main"), "main").unwrap();

        let (manager, _, _) = crate::embed::worker::manager::WorkerManager::new(
            crate::embed::worker::manager::WorkerPaths::resolve(dir.path()),
        );
        let installer = FastembedInstaller::new(
            EmbedderModel("BGEBaseENV15".to_string()),
            manager,
            "cpu".to_string(),
        );

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        installer.install(dir.path(), tx).await.unwrap();
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
    fn test_list_supported_models_cached_size() {
        let dir = tempdir().unwrap();
        let info = TextEmbedding::list_supported_models()
            .into_iter()
            .find(|m| format!("{:?}", m.model) == "BGEBaseENV15")
            .expect("expected built-in fastembed model");

        let repo_dir = dir
            .path()
            .join(format!("models--{}", info.model_code.replace('/', "--")));
        let snapshots = repo_dir.join("snapshots").join("main");
        std::fs::create_dir_all(&snapshots).unwrap();

        let mut expected_size = 0u64;
        for name in std::iter::once(info.model_file.as_str())
            .chain(info.additional_files.iter().map(String::as_str))
        {
            let content = format!("cached-{name}");
            let file_path = snapshots.join(name);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&file_path, content.as_bytes()).unwrap();
            expected_size += content.len() as u64;
        }

        let refs = repo_dir.join("refs");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("main"), "main").unwrap();

        let models = list_supported_models(dir.path());
        let model = models
            .iter()
            .find(|m| m.model_id == "BGEBaseENV15")
            .expect("expected cached model descriptor");
        assert!(model.is_cached);
        assert_eq!(model.size_bytes, Some(expected_size));
    }

    #[test]
    fn test_execution_plan_and_init_request_helpers() {
        let model = FastembedModelRecord {
            model: EmbeddingModel::BGEBaseENV15,
            model_id: "BGEBaseENV15".to_string(),
            model_code: "Xenova/bge-base-en-v1.5".to_string(),
            model_file: "model.onnx".to_string(),
            additional_files: vec!["config.json".to_string()],
            description: "desc".to_string(),
            dimension: 768,
        };

        assert_eq!(
            execution_plan_for_device("cpu"),
            FastembedExecutionPlan::CpuOnly
        );

        let gpu_plan = execution_plan_for_device("gpu");
        if cfg!(feature = "fastembed-coreml") {
            assert_eq!(gpu_plan, FastembedExecutionPlan::CoreMlThenCpu);
        } else {
            assert_eq!(gpu_plan, FastembedExecutionPlan::CpuOnly);
        }

        let init = build_text_init_request(
            model.clone(),
            tempdir().unwrap().path().to_path_buf(),
            "cpu",
        );
        assert!(init.show_download_progress);
        assert_eq!(init.model, model);
        assert_eq!(init.execution_plan, FastembedExecutionPlan::CpuOnly);
    }

    #[test]
    fn test_relevant_hf_filenames_and_matching_sizes() {
        let record = FastembedModelRecord {
            model: EmbeddingModel::BGEBaseENV15,
            model_id: "BGEBaseENV15".to_string(),
            model_code: "Xenova/bge-base-en-v1.5".to_string(),
            model_file: "model.onnx".to_string(),
            additional_files: vec!["tokenizer.json".to_string(), "config.json".to_string()],
            description: "desc".to_string(),
            dimension: 768,
        };
        let relevant = relevant_hf_filenames(&record);
        assert!(hf_sibling_matches_relevant("model.onnx", &relevant));
        assert!(hf_sibling_matches_relevant("nested/model.onnx", &relevant));
        assert!(!hf_sibling_matches_relevant("other.txt", &relevant));

        let siblings = vec![
            ("model.onnx".to_string(), Some(5)),
            ("nested/tokenizer.json".to_string(), Some(7)),
            ("other.txt".to_string(), Some(11)),
        ];
        assert_eq!(sum_matching_hf_sizes(&siblings, &relevant).unwrap(), 12);
    }

    #[test]
    fn test_sum_matching_hf_sizes_errors_when_nothing_matches() {
        let relevant = std::collections::HashSet::from(["missing.onnx".to_string()]);
        let siblings = vec![("other.txt".to_string(), Some(11))];
        assert!(sum_matching_hf_sizes(&siblings, &relevant).is_err());
    }

    #[test]
    fn test_find_model_info_error() {
        let err = find_model_info("NonExistentModel");
        assert!(err.is_err());
        assert!(err
            .unwrap_err()
            .to_string()
            .contains("is not supported by fastembed"));
    }

    #[test]
    fn test_real_fastembed_runtime_factory_error() {
        let factory = RealFastembedRuntimeFactory;
        let model = FastembedModelRecord {
            model: EmbeddingModel::BGEBaseENV15,
            model_id: "BGEBaseENV15".to_string(),
            model_code: "Xenova/bge-base-en-v1.5".to_string(),
            model_file: "model.onnx".to_string(),
            additional_files: vec![],
            description: "desc".to_string(),
            dimension: 768,
        };
        let request = FastembedInitRequest {
            model,
            cache_dir: PathBuf::from("/non/existent/cache/dir"),
            show_download_progress: false,
            execution_plan: FastembedExecutionPlan::CpuOnly,
        };
        // This should fail because the cache dir doesn't have the model
        let res = factory.try_new(request);
        assert!(res.is_err());
    }
}
