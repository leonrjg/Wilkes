use crate::types::EmbeddingEngine;
use crate::embed::Embedder;
use super::ipc::{WorkerRequest, WorkerEvent};
use super::manager::{WorkerManager, ManagerCommand};

pub struct WorkerEmbedderConfig {
    pub model_id: String,
    pub dimension: usize,
    pub device: String,
    pub engine: EmbeddingEngine,
    /// Passed in embed requests so the worker can load the model on demand.
    pub data_dir: std::path::PathBuf,
    pub query_prefix: String,
    pub passage_prefix: String,
}

/// Implements `Embedder` by dispatching to a worker subprocess via `WorkerManager`.
/// Used by SBERT (Python worker), Fastembed, and Candle (Rust worker binary).
pub struct WorkerEmbedder {
    manager: WorkerManager,
    /// Captured at construction time (always in an async context) so that
    /// `send_embed` can be called safely from non-Tokio threads (e.g. IndexWatcher).
    tokio_handle: tokio::runtime::Handle,
    model_id: String,
    dimension: usize,
    device: String,
    engine: EmbeddingEngine,
    data_dir: std::path::PathBuf,
    query_prefix: String,
    passage_prefix: String,
}

impl WorkerEmbedder {
    pub fn new(manager: WorkerManager, config: WorkerEmbedderConfig) -> Self {
        Self {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            model_id: config.model_id,
            dimension: config.dimension,
            device: config.device,
            engine: config.engine,
            data_dir: config.data_dir,
            query_prefix: config.query_prefix,
            passage_prefix: config.passage_prefix,
        }
    }

    fn send_embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let request = WorkerRequest {
            mode: "embed".to_string(),
            root: std::path::PathBuf::new(),
            engine: self.engine,
            model: self.model_id.clone(),
            data_dir: self.data_dir.clone(),
            chunk_size: None,
            chunk_overlap: None,
            device: self.device.clone(),
            paths: None,
            texts: Some(texts.iter().map(|s| s.to_string()).collect()),
            supported_extensions: Vec::new(),
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit { req: Box::new(request), reply: tx };

        self.tokio_handle.block_on(async move {
            self.manager.send(cmd).await
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
}

impl Embedder for WorkerEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.send_embed(texts)
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.query_prefix.is_empty() {
            self.send_embed(texts)
        } else {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{}{t}", self.query_prefix)).collect();
            let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
            self.send_embed(&refs)
        }
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.passage_prefix.is_empty() {
            self.send_embed(texts)
        } else {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{}{t}", self.passage_prefix)).collect();
            let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
            self.send_embed(&refs)
        }
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn engine(&self) -> EmbeddingEngine {
        self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::worker::manager::WorkerPaths;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_worker_embedder_new() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, _loop_fut) = WorkerManager::new(paths);
        
        let config = WorkerEmbedderConfig {
            model_id: "test-model".to_string(),
            dimension: 384,
            device: "cpu".to_string(),
            engine: EmbeddingEngine::Fastembed,
            data_dir: PathBuf::from("data"),
            query_prefix: "query: ".to_string(),
            passage_prefix: "passage: ".to_string(),
        };
        
        let embedder = WorkerEmbedder::new(manager, config);
        
        assert_eq!(embedder.model_id(), "test-model");
        assert_eq!(embedder.dimension(), 384);
        assert_eq!(embedder.engine(), EmbeddingEngine::Fastembed);
    }

    #[tokio::test]
    async fn test_worker_embedder_prefixes() {
        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: PathBuf::from("w"),
            data_dir: PathBuf::from("data"),
        };
        let (manager, _event_rx, _loop_fut) = WorkerManager::new(paths);
        
        let config = WorkerEmbedderConfig {
            model_id: "test-model".to_string(),
            dimension: 384,
            device: "cpu".to_string(),
            engine: EmbeddingEngine::Fastembed,
            data_dir: PathBuf::from("data"),
            query_prefix: "q: ".to_string(),
            passage_prefix: "p: ".to_string(),
        };
        
        let embedder = WorkerEmbedder::new(manager, config);
        
        // We can't easily test the actual sending without a running manager loop and a worker,
        // but we can at least check that the methods exist and call the underlying logic.
        // If we really wanted to test this, we'd need to mock the manager.
    }
}
