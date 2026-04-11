use std::io::BufRead;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use wilkes_core::embed::dispatch;
use wilkes_core::embed::installer::EmbedProgress;
use wilkes_core::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use wilkes_core::types::{EmbedderModel, EmbeddingEngine};

#[derive(Clone, Debug, PartialEq, Eq)]
struct LoadedEmbedderKey {
    engine: EmbeddingEngine,
    model: String,
    data_dir: std::path::PathBuf,
    device: String,
}

struct LoadedEmbedder {
    key: LoadedEmbedderKey,
    embedder: Arc<dyn wilkes_core::embed::Embedder>,
    background_task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug)]
enum WorkerLoopAction {
    Stop,
    ParseError(String),
    Dispatch(WorkerRequest),
}

#[derive(Debug, PartialEq, Eq)]
enum WorkerRequestKind {
    Build,
    Embed,
    Info,
    Unknown(String),
}

trait WorkerEventSink {
    fn emit(&self, event: WorkerEvent);
}

#[derive(Clone, Copy)]
struct StdoutEventSink;

impl WorkerEventSink for StdoutEventSink {
    fn emit(&self, event: WorkerEvent) {
        emit(event);
    }
}

trait EmbedderLoader: Send + Sync {
    async fn load(
        &self,
        key: &LoadedEmbedderKey,
        event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
    ) -> anyhow::Result<LoadedEmbedder>;
}

struct RealEmbedderLoader;

impl EmbedderLoader for RealEmbedderLoader {
    async fn load(
        &self,
        key: &LoadedEmbedderKey,
        event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
    ) -> anyhow::Result<LoadedEmbedder> {
        let model = EmbedderModel(key.model.clone());
        let prepared =
            dispatch::prepare_embedder(key.engine, &model, &key.data_dir, &key.device, event_tx)
                .await?;
        Ok(LoadedEmbedder {
            key: key.clone(),
            embedder: prepared.embedder,
            background_task: prepared.background_task,
        })
    }
}

impl LoadedEmbedderKey {
    fn from_request(req: &WorkerRequest) -> Self {
        Self {
            engine: req.engine,
            model: req.model.clone(),
            data_dir: req.data_dir.clone(),
            device: req.device.clone(),
        }
    }
}

fn classify_input_line(line: &str) -> WorkerLoopAction {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return WorkerLoopAction::Stop;
    }

    match serde_json::from_str::<WorkerRequest>(trimmed) {
        Ok(req) => WorkerLoopAction::Dispatch(req),
        Err(e) => WorkerLoopAction::ParseError(format!("Failed to parse worker config: {e}")),
    }
}

fn classify_worker_request(req: &WorkerRequest) -> WorkerRequestKind {
    match req.mode.as_str() {
        "build" => WorkerRequestKind::Build,
        "embed" => WorkerRequestKind::Embed,
        "info" => WorkerRequestKind::Info,
        other => WorkerRequestKind::Unknown(other.to_string()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wilkes_core::logging::init_logging_stderr();

    tracing::info!("[worker] starting up...");

    let stdin = std::io::stdin();
    let mut active_embedder: Option<LoadedEmbedder> = None;
    let loader = RealEmbedderLoader;
    let sink = StdoutEventSink;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<WorkerEvent>(128);

    // Background task to print events to stdout
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            sink.emit(event);
        }
    });

    for line in stdin.lock().lines() {
        let line = line?;
        match classify_input_line(&line) {
            WorkerLoopAction::Stop => break,
            WorkerLoopAction::ParseError(message) => sink.emit(WorkerEvent::Error(message)),
            WorkerLoopAction::Dispatch(req) => {
                let mut log_req = req.clone();
                log_req.texts = None;
                tracing::info!(
                    "[worker] received request: {}",
                    serde_json::to_string(&log_req).unwrap_or_default()
                );

                if let Err(e) =
                    handle_worker_request(req, &mut active_embedder, event_tx.clone(), &loader)
                        .await
                {
                    sink.emit(WorkerEvent::Error(e.to_string()));
                }
            }
        }
    }

    Ok(())
}

async fn handle_worker_request(
    req: WorkerRequest,
    active_embedder: &mut Option<LoadedEmbedder>,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl EmbedderLoader,
) -> anyhow::Result<()> {
    match classify_worker_request(&req) {
        WorkerRequestKind::Build => {
            handle_build_plan(req, active_embedder, event_tx, loader).await?;
        }
        WorkerRequestKind::Embed => {
            handle_embed_plan(req, active_embedder, event_tx, loader).await?;
        }
        WorkerRequestKind::Info => {
            handle_info_plan(req, active_embedder, event_tx, loader).await?;
        }
        WorkerRequestKind::Unknown(other) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("Unknown mode: {other}")))
                .await;
        }
    }
    Ok(())
}

async fn handle_build_plan(
    req: WorkerRequest,
    active_embedder: &mut Option<LoadedEmbedder>,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl EmbedderLoader,
) -> anyhow::Result<()> {
    tracing::info!("[worker] build: loading embedder");
    let embedder = get_or_load_embedder(active_embedder, &req, loader, Some(&event_tx)).await?;
    tracing::info!(
        "[worker] build: embedder loaded (engine={:?}, model={}, dim={})",
        req.engine,
        embedder.model_id(),
        embedder.dimension()
    );

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
        chunk_size: req
            .chunk_size
            .ok_or_else(|| anyhow::anyhow!("build request missing chunk_size"))?,
        chunk_overlap: req
            .chunk_overlap
            .ok_or_else(|| anyhow::anyhow!("build request missing chunk_overlap"))?,
        supported_extensions: req.supported_extensions,
    };

    tracing::info!("[worker] build: starting build_index_with_embedder");
    let result =
        wilkes_api::commands::embed::build_index_with_embedder(req.root, embedder, options).await;
    tracing::info!("[worker] build: build_index_with_embedder returned");

    forward.await?;

    match result {
        Ok(_) => {
            let _ = event_tx.send(WorkerEvent::Done).await;
        }
        Err(e) => {
            let _ = event_tx.send(WorkerEvent::Error(e.to_string())).await;
        }
    }
    Ok(())
}

async fn handle_embed_plan(
    req: WorkerRequest,
    active_embedder: &mut Option<LoadedEmbedder>,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl EmbedderLoader,
) -> anyhow::Result<()> {
    let embedder = get_or_load_embedder(active_embedder, &req, loader, None).await?;
    let texts = req.texts.unwrap_or_default();
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    match embedder.embed(&text_refs) {
        Ok(embeddings) => {
            let _ = event_tx.send(WorkerEvent::Embeddings(embeddings)).await;
            let _ = event_tx.send(WorkerEvent::Done).await;
        }
        Err(e) => {
            let _ = event_tx
                .send(WorkerEvent::Error(format!("Embed error: {e}")))
                .await;
        }
    }
    Ok(())
}

async fn handle_info_plan(
    req: WorkerRequest,
    active_embedder: &mut Option<LoadedEmbedder>,
    event_tx: tokio::sync::mpsc::Sender<WorkerEvent>,
    loader: &impl EmbedderLoader,
) -> anyhow::Result<()> {
    let embedder = get_or_load_embedder(active_embedder, &req, loader, None).await?;
    let _ = event_tx
        .send(WorkerEvent::Info {
            dimension: embedder.dimension(),
            max_seq_length: 512,
        })
        .await;
    let _ = event_tx.send(WorkerEvent::Done).await;
    Ok(())
}

async fn get_or_load_embedder(
    active: &mut Option<LoadedEmbedder>,
    req: &WorkerRequest,
    loader: &impl EmbedderLoader,
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
) -> anyhow::Result<Arc<dyn wilkes_core::embed::Embedder>> {
    let key = LoadedEmbedderKey::from_request(req);

    if let Some(current) = active {
        if current.key == key {
            tracing::info!("[worker] reusing cached embedder");
            return Ok(Arc::clone(&current.embedder));
        }

        tracing::info!(
            "[worker] invalidating cached embedder (engine: {:?} -> {:?}, model: {} -> {}, device: {} -> {}, data_dir: {} -> {})",
            current.key.engine,
            key.engine,
            current.key.model,
            key.model,
            current.key.device,
            key.device,
            current.key.data_dir.display(),
            key.data_dir.display()
        );
    }

    tracing::info!("[worker] loading embedder from scratch");
    if let Some(current) = active.take() {
        if let Some(task) = current.background_task {
            task.abort();
        }
    }
    let loaded = loader.load(&key, event_tx).await?;
    tracing::info!("[worker] embedder load succeeded");
    let embedder = Arc::clone(&loaded.embedder);
    *active = Some(loaded);
    Ok(embedder)
}

fn emit(event: WorkerEvent) {
    let line = serde_json::to_string(&event).expect("WorkerEvent serialization failed");
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use wilkes_core::embed::worker::ipc::WorkerRequest;
    use wilkes_core::embed::MockEmbedder;
    use wilkes_core::types::EmbeddingEngine;

    struct SuccessLoader;

    impl EmbedderLoader for SuccessLoader {
        async fn load(
            &self,
            _key: &LoadedEmbedderKey,
            _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
        ) -> anyhow::Result<LoadedEmbedder> {
            Ok(LoadedEmbedder {
                key: _key.clone(),
                embedder: Arc::new(MockEmbedder::default()),
                background_task: None,
            })
        }
    }

    struct FailLoader;

    impl EmbedderLoader for FailLoader {
        async fn load(
            &self,
            _key: &LoadedEmbedderKey,
            _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
        ) -> anyhow::Result<LoadedEmbedder> {
            Err(anyhow::anyhow!("load failed"))
        }
    }

    fn sample_request(mode: &str) -> WorkerRequest {
        let dir = tempdir().unwrap();
        WorkerRequest {
            mode: mode.to_string(),
            root: PathBuf::from("."),
            engine: EmbeddingEngine::Candle,
            model: "model-a".to_string(),
            data_dir: dir.path().to_path_buf(),
            chunk_size: Some(32),
            chunk_overlap: Some(8),
            device: "cpu".to_string(),
            paths: None,
            texts: Some(vec!["hello".to_string()]),
            supported_extensions: vec!["txt".to_string()],
        }
    }

    #[test]
    fn test_classify_input_line_variants() {
        match classify_input_line("") {
            WorkerLoopAction::Stop => {}
            other => panic!("expected Stop, got {other:?}"),
        }

        match classify_input_line("not-json") {
            WorkerLoopAction::ParseError(message) => {
                assert!(message.contains("Failed to parse worker config"));
            }
            other => panic!("expected ParseError, got {other:?}"),
        }

        match classify_input_line(&serde_json::to_string(&sample_request("embed")).unwrap()) {
            WorkerLoopAction::Dispatch(req) => {
                assert_eq!(req.mode, "embed");
                assert_eq!(req.model, "model-a");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_worker_request_variants() {
        assert_eq!(
            classify_worker_request(&sample_request("build")),
            WorkerRequestKind::Build
        );
        assert_eq!(
            classify_worker_request(&sample_request("embed")),
            WorkerRequestKind::Embed
        );
        assert_eq!(
            classify_worker_request(&sample_request("info")),
            WorkerRequestKind::Info
        );

        match classify_worker_request(&sample_request("unknown")) {
            WorkerRequestKind::Unknown(value) => assert_eq!(value, "unknown"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_get_or_load_embedder_caching() {
        let dir = tempdir().unwrap();
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "any-model".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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
        let res = get_or_load_embedder(&mut active, &req, &SuccessLoader, None)
            .await
            .unwrap();
        assert_eq!(res.model_id(), "mock-model");
    }

    #[tokio::test]
    async fn test_get_or_load_embedder_invalidates_on_request_change() {
        let dir = tempdir().unwrap();
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "old-model".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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

        let res = get_or_load_embedder(&mut active, &req, &FailLoader, None).await;
        assert!(res.is_err());
        assert!(active.is_none());
    }

    #[tokio::test]
    async fn test_get_or_load_embedder_failure() {
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
        let res = get_or_load_embedder(&mut active, &req, &FailLoader, None).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_handle_worker_request_info() {
        let dir = tempdir().unwrap();
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "any".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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

        handle_worker_request(req, &mut active, tx, &SuccessLoader)
            .await
            .unwrap();

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
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "any".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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

        handle_worker_request(req, &mut active, tx, &SuccessLoader)
            .await
            .unwrap();

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

        handle_worker_request(req, &mut active, tx, &FailLoader)
            .await
            .unwrap();

        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, WorkerEvent::Error(_)));
    }

    #[tokio::test]
    async fn test_handle_worker_request_build() {
        let dir = tempdir().unwrap();
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "any".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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

        handle_worker_request(req, &mut active, tx, &SuccessLoader)
            .await
            .unwrap();

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
        let mut active = Some(LoadedEmbedder {
            key: LoadedEmbedderKey {
                engine: EmbeddingEngine::Candle,
                model: "any".to_string(),
                data_dir: dir.path().to_path_buf(),
                device: "cpu".to_string(),
            },
            embedder: Arc::new(MockEmbedder::default()),
            background_task: None,
        });
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

        let res = handle_worker_request(req, &mut active, tx, &SuccessLoader).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("missing chunk_size"));
    }

    #[test]
    fn test_loaded_embedder_key_equality() {
        let k1 = LoadedEmbedderKey {
            engine: EmbeddingEngine::Candle,
            model: "m".to_string(),
            data_dir: PathBuf::from("d"),
            device: "cpu".to_string(),
        };
        let k2 = k1.clone();
        let k3 = LoadedEmbedderKey {
            engine: EmbeddingEngine::SBERT,
            model: "m".to_string(),
            data_dir: PathBuf::from("d"),
            device: "cpu".to_string(),
        };
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
        assert!(format!("{:?}", k1).contains("model: \"m\""));
    }

    #[tokio::test]
    async fn test_real_loader_fails_on_missing_model() {
        let loader = RealEmbedderLoader;
        let key = LoadedEmbedderKey {
            engine: EmbeddingEngine::Candle,
            model: "non-existent".to_string(),
            data_dir: PathBuf::from("/tmp/non-existent-data-dir"),
            device: "cpu".to_string(),
        };
        let res = loader.load(&key, None).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_handle_embed_plan_failure() {
        struct FailEmbedder;
        impl wilkes_core::embed::Embedder for FailEmbedder {
            fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Err(anyhow::anyhow!("embed failed"))
            }
            fn model_id(&self) -> &str {
                "fail"
            }
            fn dimension(&self) -> usize {
                384
            }
            fn engine(&self) -> EmbeddingEngine {
                EmbeddingEngine::Candle
            }
        }

        struct FailEmbedderLoader;
        impl EmbedderLoader for FailEmbedderLoader {
            async fn load(
                &self,
                _key: &LoadedEmbedderKey,
                _event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
            ) -> anyhow::Result<LoadedEmbedder> {
                Ok(LoadedEmbedder {
                    key: _key.clone(),
                    embedder: Arc::new(FailEmbedder),
                    background_task: None,
                })
            }
        }

        let mut active = None;
        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let req = sample_request("embed");

        handle_worker_request(req, &mut active, tx, &FailEmbedderLoader)
            .await
            .unwrap();

        let ev = rx.recv().await.unwrap();
        match ev {
            WorkerEvent::Error(e) => assert!(e.contains("Embed error")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn test_stdout_event_sink() {
        let sink = StdoutEventSink;
        sink.emit(WorkerEvent::Done);
    }
}