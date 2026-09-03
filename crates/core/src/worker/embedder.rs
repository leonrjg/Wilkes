use super::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use super::manager::{ManagerCommand, WorkerManager};
use crate::embed::{Embedder, EmbeddingSpaceIdentity};
use crate::types::EmbeddingEngine;

pub struct WorkerEmbedderConfig {
    pub model_id: String,
    pub dimension: usize,
    pub device: String,
    pub engine: EmbeddingEngine,
    /// Passed in embed requests so the worker can load the model on demand.
    pub data_dir: std::path::PathBuf,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub embedding_space_identity: EmbeddingSpaceIdentity,
}

/// Implements `Embedder` by dispatching to a worker subprocess via `WorkerManager`.
/// Used by SBERT (Python worker), Fastembed, and Candle (Rust worker binary).
pub struct WorkerEmbedder {
    manager: WorkerManager,
    /// Captured at construction time (always in an async context) so that
    /// `send_embed` can be called safely from non-Tokio threads.
    tokio_handle: tokio::runtime::Handle,
    model_id: String,
    dimension: usize,
    device: String,
    engine: EmbeddingEngine,
    data_dir: std::path::PathBuf,
    query_prefix: String,
    passage_prefix: String,
    embedding_space_identity: EmbeddingSpaceIdentity,
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
            embedding_space_identity: config.embedding_space_identity,
        }
    }

    fn send_embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let request = WorkerRequest {
            mode: "embed".to_string(),
            role: WorkerRole::Embed(self.engine),
            model: self.model_id.clone(),
            model_dir: self.data_dir.clone(),
            device: self.device.clone(),
            texts: Some(texts.iter().map(|s| s.to_string()).collect()),
            generate: None,
            recognize: None,
            layout: None,
            table: None,
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        self.tokio_handle.block_on(async move {
            self.manager.send(cmd).await.map_err(|e| {
                crate::worker::fault::WorkerFault::gone(format!(
                    "failed to send command to manager: {e}"
                ))
            })?;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::Embeddings(vecs) => return Ok(vecs),
                    WorkerEvent::Error(err) => {
                        return Err(crate::worker::fault::WorkerFault::reported(err))
                    }
                    WorkerEvent::Gone(detail) => {
                        return Err(crate::worker::fault::WorkerFault::gone(detail))
                    }
                    WorkerEvent::Done => break,
                    _ => {}
                }
            }
            // The channel ended without an answer, which means the reply
            // sender was dropped: the worker is not coming back with these.
            Err(crate::worker::fault::WorkerFault::gone(
                "worker finished without returning embeddings",
            ))
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
            let prefixed: Vec<String> = texts
                .iter()
                .map(|t| format!("{}{t}", self.query_prefix))
                .collect();
            let refs: Vec<&str> = prefixed.iter().map(String::as_str).collect();
            self.send_embed(&refs)
        }
    }

    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.passage_prefix.is_empty() {
            self.send_embed(texts)
        } else {
            let prefixed: Vec<String> = texts
                .iter()
                .map(|t| format!("{}{t}", self.passage_prefix))
                .collect();
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

    fn embedding_space_identity(&self) -> EmbeddingSpaceIdentity {
        self.embedding_space_identity.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::manager::WorkerPaths;
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
        let (manager, _event_rx, _loop_fut) =
            WorkerManager::new(paths, crate::worker::ipc::WorkerKind::Embed);

        let config = WorkerEmbedderConfig {
            model_id: "test-model".to_string(),
            dimension: 384,
            device: "cpu".to_string(),
            engine: EmbeddingEngine::Fastembed,
            data_dir: PathBuf::from("data"),
            query_prefix: "query: ".to_string(),
            passage_prefix: "passage: ".to_string(),
            embedding_space_identity: EmbeddingSpaceIdentity::for_runtime(
                EmbeddingEngine::Fastembed,
                "test-model",
                384,
            ),
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
        let (manager, _event_rx, loop_fut) =
            WorkerManager::new(paths, crate::worker::ipc::WorkerKind::Embed);
        tokio::spawn(loop_fut);

        let config = WorkerEmbedderConfig {
            model_id: "test-model".to_string(),
            dimension: 384,
            device: "cpu".to_string(),
            engine: EmbeddingEngine::Fastembed,
            data_dir: PathBuf::from("data"),
            query_prefix: "q: ".to_string(),
            passage_prefix: "p: ".to_string(),
            embedding_space_identity: EmbeddingSpaceIdentity::for_runtime(
                EmbeddingEngine::Fastembed,
                "test-model",
                384,
            ),
        };

        let embedder = Arc::new(WorkerEmbedder::new(manager, config));

        // Use spawn_blocking or a separate thread to avoid "Cannot start a runtime from within a runtime"
        // because WorkerEmbedder::send_embed uses block_on.
        let embedder_c = Arc::clone(&embedder);
        let res = tokio::task::spawn_blocking(move || embedder_c.embed_query(&["hello"]))
            .await
            .unwrap();
        assert!(res.is_err());

        let embedder_c2 = Arc::clone(&embedder);
        let res2 = tokio::task::spawn_blocking(move || embedder_c2.embed_passages(&["world"]))
            .await
            .unwrap();
        assert!(res2.is_err());
    }

    #[tokio::test]
    async fn test_worker_embedder_error_path() {
        use std::fs;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let worker_bin = dir.path().join("worker.sh");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(
                &worker_bin,
                "#!/bin/sh\nread req\necho '{\"Error\":\"test error\"}'\n",
            )
            .unwrap();
            let mut perms = fs::metadata(&worker_bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&worker_bin, perms).unwrap();
        }
        #[cfg(windows)]
        {
            fs::write(
                &worker_bin,
                "@echo off\nset /p req=\necho {\"Error\":\"test error\"}\n",
            )
            .unwrap();
        }

        let paths = WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: worker_bin.clone(),
            data_dir: dir.path().to_path_buf(),
        };
        let (manager, _event_rx, loop_fut) =
            WorkerManager::new(paths, crate::worker::ipc::WorkerKind::Embed);
        tokio::spawn(loop_fut);

        let config = WorkerEmbedderConfig {
            model_id: "m".to_string(),
            dimension: 384,
            device: "cpu".to_string(),
            engine: EmbeddingEngine::Candle,
            data_dir: dir.path().to_path_buf(),
            query_prefix: "".to_string(),
            passage_prefix: "".to_string(),
            embedding_space_identity: EmbeddingSpaceIdentity::for_runtime(
                EmbeddingEngine::Fastembed,
                "test-model",
                384,
            ),
        };

        let embedder = WorkerEmbedder::new(manager, config);
        let res = tokio::task::spawn_blocking(move || embedder.embed(&["test"]))
            .await
            .unwrap();

        // The worker answered the request with a failure. That is a
        // `WorkerFault::reported` — typed, so the build's batch loop can stop
        // on it without reading the message — and the detail it carries is
        // still what the worker said.
        assert!(res.is_err());
        let error = res.unwrap_err();
        assert!(error.to_string().contains("test error"));
        assert_eq!(
            crate::worker::fault::fault_of(&error),
            Some(crate::worker::fault::Fault::Reported)
        );
    }
}
