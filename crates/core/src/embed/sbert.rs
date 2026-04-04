use std::path::Path;
use std::sync::Arc;
use async_trait::async_trait;

use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};
use super::Embedder;
use super::installer::{EmbedderInstaller, ProgressTx};
use super::worker_manager::{WorkerManager, ManagerCommand};
use super::worker_ipc::{WorkerRequest, WorkerEvent};

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
        description: "Speed: slow, accuracy: medium-high (English only)",
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

pub fn list_supported_models() -> Vec<ModelDescriptor> {
    let mut models: std::collections::HashMap<String, ModelDescriptor> = PREEXISTING_MODELS
        .iter()
        .map(|info| {
            (
                info.model_id.to_string(),
                ModelDescriptor {
                    model_id: info.model_id.to_string(),
                    display_name: info.display_name.to_string(),
                    description: info.description.to_string(),
                    dimension: info.dimension,
                    is_cached: false,
                    is_default: info.is_default,
                    is_recommended: info.is_recommended,
                    size_bytes: None,
                    preferred_batch_size: Some(32),
                },
            )
        })
        .collect();

    let cache_root = super::hf_cache::get_hf_cache_root();
    if cache_root.exists() {
        if let Ok(entries) = std::fs::read_dir(cache_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }

                let folder_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(name) if name.starts_with("models--") => name,
                    _ => continue,
                };

                // models--org--name -> org/name
                let model_id_encoded = &folder_name[8..];
                let model_id = if let Some(pos) = model_id_encoded.find("--") {
                    let (org, name) = model_id_encoded.split_at(pos);
                    format!("{}/{}", org, &name[2..])
                } else {
                    model_id_encoded.to_string()
                };

                if let Some(desc) = super::hf_cache::get_model_descriptor_from_path(&path, &model_id) {
                    if let Some(existing) = models.get_mut(&model_id) {
                        existing.is_cached = true;
                        existing.size_bytes = desc.size_bytes;
                    } else {
                        models.insert(model_id, desc);
                    }
                }
            }
        }
    }

    let mut result: Vec<ModelDescriptor> = models.into_values().collect();
    result.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    result
}

// ── SBERTEmbedder ────────────────────────────────────────────────────────────

/// Implements `Embedder` for the SBERT engine by dispatching to the `WorkerManager`.
pub struct SBERTEmbedder {
    manager: WorkerManager,
    model_id: String,
    dimension: usize,
    device: String,
}

impl SBERTEmbedder {
    pub fn new(manager: WorkerManager, model_id: String, dimension: usize, device: String) -> Self {
        Self { manager, model_id, dimension, device }
    }
}

impl Embedder for SBERTEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let request = WorkerRequest {
            mode: "embed".to_string(),
            root: std::path::PathBuf::new(),
            engine: EmbeddingEngine::SBERT,
            model: self.model_id.clone(),
            data_dir: std::path::PathBuf::new(),
            chunk_size: 0,
            chunk_overlap: 0,
            device: self.device.clone(),
            paths: None,
            texts: Some(texts.iter().map(|s| s.to_string()).collect()),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        
        let cmd = ManagerCommand::Submit {
            req: request,
            reply: tx,
        };

        // Since embed() is synchronous, we block on sending and receiving.
        let handle = tokio::runtime::Handle::current();
        
        handle.block_on(async move {
            self.manager.sender().send(cmd).await
                .map_err(|e| anyhow::anyhow!("Failed to send command to manager: {e}"))?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::Embeddings(vecs) => return Ok(vecs),
                    WorkerEvent::Error(err) => return Err(anyhow::anyhow!("Worker error: {}", err)),
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            Err(anyhow::anyhow!("Worker finished without returning embeddings"))
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
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
            chunk_size: 0,
            chunk_overlap: 0,
            device: self.device.clone(),
            paths: None,
            texts: None,
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: request,
            reply: tx,
        };

        self.manager.sender().send(cmd).await
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

        Ok(Arc::new(SBERTEmbedder::new(
            self.manager.clone(),
            model_id.to_string(),
            dimension,
            self.device.clone(),
        )))
    }
}
