use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;

use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};
use super::super::Embedder;
use super::super::models::installer::{EmbedderInstaller, ProgressTx};
use super::super::worker::manager::{WorkerManager, ManagerCommand};
use super::super::worker::ipc::{WorkerRequest, WorkerEvent};
use super::super::worker::embedder::{WorkerEmbedder, WorkerEmbedderConfig};
use super::aux_config;

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
        model_id: "intfloat/e5-small-v2",
        display_name: "e5-small-v2",
        description: "Speed: high, accuracy: medium-high (English only)",
        dimension: 384,
        is_default: true,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "nomic-ai/nomic-embed-text-v1",
        display_name: "nomic-embed-text-v1",
        description: "Speed: high, accuracy: medium-high (English only)",
        dimension: 384,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "nomic-ai/nomic-embed-text-v1.5",
        display_name: "nomic-embed-text-v1.5",
        description: "Speed: medium, accuracy: medium-high (English only)",
        dimension: 768,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-MiniLM-L12-v2",
        display_name: "all-MiniLM-L12-v2",
        description: "Speed: high, accuracy: medium (English)",
        dimension: 384,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "jinaai/jina-embeddings-v5-text-small",
        display_name: "jina-embeddings-v5-text-small",
        description: "Speed: slow, accuracy: high (English only)",
        dimension: 1024,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "jinaai/jina-embeddings-v5-text-nano",
        display_name: "jina-embeddings-v5-text-nano",
        description: "Speed: slow, accuracy: medium (English only)",
        dimension: 768,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-mpnet-base-v2",
        display_name: "all-mpnet-base-v2",
        description: "Speed: medium, accuracy: medium (English only)",
        dimension: 768,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "minishlab/potion-multilingual-128M",
        display_name: "potion-multilingual-128M",
        description: "Speed: highest, accuracy: low",
        dimension: 256,
        is_default: false,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "thenlper/gte-small",
        display_name: "gte-small",
        description: "Speed: medium, accuracy: low (English only)",
        dimension: 384,
        is_default: false,
        is_recommended: false,
    },
];

/// Check if a model has been downloaded into `data_dir` by looking for the
/// HF cache snapshot directory (`models--org--repo/snapshots/<hash>/`).
fn is_sbert_model_cached(data_dir: &Path, model_id: &str) -> bool {
    let folder = format!("models--{}", model_id.replace('/', "--"));
    let snapshots = data_dir.join(folder).join("snapshots");
    snapshots.is_dir()
        && std::fs::read_dir(&snapshots)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
}

pub fn list_supported_models(data_dir: &Path) -> Vec<ModelDescriptor> {
    PREEXISTING_MODELS
        .iter()
        .map(|info| ModelDescriptor {
            model_id: info.model_id.to_string(),
            display_name: info.display_name.to_string(),
            description: info.description.to_string(),
            dimension: info.dimension,
            is_cached: is_sbert_model_cached(data_dir, info.model_id),
            is_default: info.is_default,
            is_recommended: info.is_recommended,
            size_bytes: None,
            preferred_batch_size: Some(32),
        })
        .collect()
}

// ── SBERTInstaller ───────────────────────────────────────────────────────────

pub struct SBERTInstaller {
    pub model: EmbedderModel,
    pub manager: WorkerManager,
    pub device: String,
    pub dimension: std::sync::Mutex<Option<usize>>,
}

impl SBERTInstaller {
    pub fn new(model: EmbedderModel, manager: WorkerManager, device: String) -> Self {
        Self { model, manager, device, dimension: std::sync::Mutex::new(None) }
    }
}

#[async_trait]
impl EmbedderInstaller for SBERTInstaller {
    fn is_available(&self, _data_dir: &Path) -> bool {
        // We consider it available if we have already probed the dimension
        // or if it's in our built-in list.
        let model_id = self.model.model_id();
        if PREEXISTING_MODELS.iter().any(|m| m.model_id == model_id) {
            return true;
        }
        self.dimension.lock().unwrap().is_some()
    }

    async fn install(&self, _data_dir: &Path, _tx: ProgressTx) -> anyhow::Result<()> {
        let model_id = self.model.model_id();

        // If it's a built-in model, we already know the dimension.
        if let Some(m) = PREEXISTING_MODELS.iter().find(|m| m.model_id == model_id) {
            *self.dimension.lock().unwrap() = Some(m.dimension);
            return Ok(());
        }

        // Otherwise, perform the Live Probe asynchronously.
        let request = WorkerRequest {
            mode: "info".to_string(),
            root: std::path::PathBuf::new(),
            engine: EmbeddingEngine::SBERT,
            model: model_id.to_string(),
            data_dir: std::path::PathBuf::new(),
            chunk_size: None,
            chunk_overlap: None,
            device: self.device.clone(),
            paths: None,
            texts: None,
            supported_extensions: Vec::new(),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        self.manager.send(cmd).await
            .map_err(|e| anyhow::anyhow!("Failed to send info command to manager: {e}"))?;

        let timeout_duration = std::time::Duration::from_secs(30);
        let result = tokio::time::timeout(timeout_duration, async {
            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::Info { dimension, .. } => {
                        return Ok(dimension);
                    }
                    WorkerEvent::Error(err) => return Err(anyhow::anyhow!("Worker error probing model: {}", err)),
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!("Worker finished without returning model info for {}", model_id))
        }).await;

        match result {
            Ok(Ok(dimension)) => {
                *self.dimension.lock().unwrap() = Some(dimension);
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow::anyhow!("Timeout probing model '{}' after 30 seconds", model_id)),
        }
    }

    fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
        let model_id = self.model.model_id();

        // Use the dimension discovered during install() or from the built-in list.
        let dimension = self.dimension.lock().unwrap()
            .or_else(|| {
                PREEXISTING_MODELS
                    .iter()
                    .find(|m| m.model_id == model_id)
                    .map(|m| m.dimension)
            })
            .ok_or_else(|| anyhow::anyhow!(
                "Dimension unknown for model '{}'. install() must be called before build().",
                model_id
            ))?;

        let prefixes = aux_config::load_prefixes(_data_dir, model_id);

        Ok(Arc::new(WorkerEmbedder::new(
            self.manager.clone(),
            WorkerEmbedderConfig {
                model_id: model_id.to_string(),
                dimension,
                device: self.device.clone(),
                engine: EmbeddingEngine::SBERT,
                data_dir: std::path::PathBuf::new(),
                query_prefix: prefixes.query_prefix,
                passage_prefix: prefixes.passage_prefix,
            },
        )))
    }
}
