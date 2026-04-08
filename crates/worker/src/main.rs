use std::io::BufRead;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use wilkes_core::embed::dispatch;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use wilkes_core::types::EmbedderModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging_stderr();

    tracing::info!("[worker] starting up...");

    let stdin = std::io::stdin();
    let mut active_embedder: Option<Arc<dyn wilkes_core::embed::Embedder>> = None;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<WorkerEvent>(128);
    
    // Background task to print events to stdout
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            emit(event);
        }
    });

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            break;
        }

        let trimmed = line.trim();

        let req: WorkerRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                emit(WorkerEvent::Error(format!("Failed to parse worker config: {e}")));
                continue;
            }
        };

        let mut log_req = req.clone();
        log_req.texts = None;
        tracing::info!("[worker] received request: {}", serde_json::to_string(&log_req).unwrap_or_default());

        if let Err(e) = handle_worker_request(req, &mut active_embedder, event_tx.clone()).await {
            emit(WorkerEvent::Error(e.to_string()));
        }
    }

    Ok(())
}

async fn handle_worker_request(
    req: WorkerRequest,
    active_embedder: &mut Option<Arc<dyn wilkes_core::embed::Embedder>>,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
) -> anyhow::Result<()> {
    match req.mode.as_str() {
        "build" => {
            let embedder = get_or_load_embedder(active_embedder, &req)?;

            let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);
            let tx_c = event_tx.clone();
            let forward = tokio::spawn(async move {
                while let Some(progress) = rx.recv().await {
                    let _ = tx_c.send(WorkerEvent::Progress(progress)).await;
                }
            });

            let options = wilkes_api::commands::embed::BuildIndexOptions {
                manager: None,
                device: None,
                data_dir: req.data_dir,
                tx,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                chunk_size: req.chunk_size.ok_or_else(|| anyhow::anyhow!("build request missing chunk_size"))?,
                chunk_overlap: req.chunk_overlap.ok_or_else(|| anyhow::anyhow!("build request missing chunk_overlap"))?,
                supported_extensions: req.supported_extensions,
            };

            let result = wilkes_api::commands::embed::build_index_with_embedder(
                req.root,
                embedder,
                options,
            )
            .await;

            forward.await?;

            match result {
                Ok(_) => { let _ = event_tx.send(WorkerEvent::Done).await; }
                Err(e) => { let _ = event_tx.send(WorkerEvent::Error(e.to_string())).await; }
            }
        }

        "embed" => {
            let embedder = get_or_load_embedder(active_embedder, &req)?;
            let texts = req.texts.unwrap_or_default();
            let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            match embedder.embed(&text_refs) {
                Ok(embeddings) => {
                    let _ = event_tx.send(WorkerEvent::Embeddings(embeddings)).await;
                    let _ = event_tx.send(WorkerEvent::Done).await;
                }
                Err(e) => { let _ = event_tx.send(WorkerEvent::Error(format!("Embed error: {e}"))).await; }
            }
        }

        "info" => {
            let embedder = get_or_load_embedder(active_embedder, &req)?;
            let _ = event_tx.send(WorkerEvent::Info {
                dimension: embedder.dimension(),
                max_seq_length: 512,
            }).await;
            let _ = event_tx.send(WorkerEvent::Done).await;
        }

        other => { let _ = event_tx.send(WorkerEvent::Error(format!("Unknown mode: {other}"))).await; }
    }
    Ok(())
}


fn get_or_load_embedder(
    active: &mut Option<Arc<dyn wilkes_core::embed::Embedder>>,
    req: &WorkerRequest,
) -> anyhow::Result<Arc<dyn wilkes_core::embed::Embedder>> {
    if let Some(ref e) = active {
        return Ok(Arc::clone(e));
    }
    let model = EmbedderModel(req.model.clone());
    let embedder = dispatch::load_embedder_local(req.engine, &model, &req.data_dir, &req.device)?;
    *active = Some(Arc::clone(&embedder));
    Ok(embedder)
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::path::PathBuf;
    use wilkes_core::types::EmbeddingEngine;
    use wilkes_core::embed::worker::ipc::WorkerRequest;
    use wilkes_core::embed::MockEmbedder;

    #[test]
    fn test_get_or_load_embedder_caching() {
        let dir = tempdir().unwrap();
        let mut active: Option<Arc<dyn wilkes_core::embed::Embedder>> = Some(Arc::new(MockEmbedder::default()));
        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "any-model".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec![],
        };

        // Should return the cached MockEmbedder immediately
        let res = get_or_load_embedder(&mut active, &req).unwrap();
        assert_eq!(res.model_id(), "mock-model");
    }

    #[test]
    fn test_get_or_load_embedder_failure() {
        let dir = tempdir().unwrap();
        let mut active = None;
        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "non-existent-model".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec![],
        };

        // Should fail to load non-existent model from empty temp dir
        let res = get_or_load_embedder(&mut active, &req);
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_handle_worker_request_info() {
        let dir = tempdir().unwrap();
        let mut active: Option<Arc<dyn wilkes_core::embed::Embedder>> = Some(Arc::new(MockEmbedder::default()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        
        let req = WorkerRequest {
            mode: "info".to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "any".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec![],
        };

        handle_worker_request(req, &mut active, tx).await.unwrap();
        
        let ev1 = rx.recv().await.unwrap();
        if let WorkerEvent::Info { dimension, .. } = ev1 {
            assert_eq!(dimension, 384);
        } else {
            panic!("Expected Info event, got {:?}", ev1);
        }
        
        let ev2 = rx.recv().await.unwrap();
        assert!(matches!(ev2, WorkerEvent::Done));
    }

    #[tokio::test]
    async fn test_handle_worker_request_embed() {
        let dir = tempdir().unwrap();
        let mut active: Option<Arc<dyn wilkes_core::embed::Embedder>> = Some(Arc::new(MockEmbedder::default()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        
        let req = WorkerRequest {
            mode: "embed".to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "any".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: Some(vec!["hello".to_string()]),
            supported_extensions: vec![],
        };

        handle_worker_request(req, &mut active, tx).await.unwrap();
        
        let ev1 = rx.recv().await.unwrap();
        assert!(matches!(ev1, WorkerEvent::Embeddings(_)));
        
        let ev2 = rx.recv().await.unwrap();
        assert!(matches!(ev2, WorkerEvent::Done));
    }

    #[tokio::test]
    async fn test_handle_worker_request_unknown() {
        let dir = tempdir().unwrap();
        let mut active = None;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        
        let req = WorkerRequest {
            mode: "unknown".to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "any".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec![],
        };

        handle_worker_request(req, &mut active, tx).await.unwrap();
        
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, WorkerEvent::Error(_)));
    }

    #[tokio::test]
    async fn test_handle_worker_request_build() {
        let dir = tempdir().unwrap();
        let mut active: Option<Arc<dyn wilkes_core::embed::Embedder>> = Some(Arc::new(MockEmbedder::default()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        
        let req = WorkerRequest {
            mode: "build".to_string(),
            root: dir.path().to_path_buf(),
            engine: EmbeddingEngine::Candle,
            model: "any".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: Some(100),
            chunk_overlap: Some(10),
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec!["rs".to_string()],
        };

        handle_worker_request(req, &mut active, tx).await.unwrap();
        
        // Should eventually get Done
        let mut found_done = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, WorkerEvent::Done) {
                found_done = true;
                break;
            }
        }
        assert!(found_done);
    }

    #[tokio::test]
    async fn test_handle_worker_request_build_missing_options() {
        let dir = tempdir().unwrap();
        let mut active: Option<Arc<dyn wilkes_core::embed::Embedder>> = Some(Arc::new(MockEmbedder::default()));
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        
        let req = WorkerRequest {
            mode: "build".to_string(),
            root: dir.path().to_path_buf(),
            engine: EmbeddingEngine::Candle,
            model: "any".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: None,
            chunk_overlap: None,
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec![],
        };

        let res = handle_worker_request(req, &mut active, tx).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("missing chunk_size"));
    }

    #[test]
    fn test_emit() {
        emit(WorkerEvent::Done);
    }
}
