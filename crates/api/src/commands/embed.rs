use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ignore::WalkBuilder;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::ProgressTx;
use wilkes_core::embed::models::installer::EmbedderInstaller;
use wilkes_core::embed::worker::ipc::{WorkerEvent, WorkerRequest};
use wilkes_core::embed::worker::manager::{ManagerCommand, WorkerManager};
use wilkes_core::embed::Embedder;
use wilkes_core::extract::pdf::PdfExtractor;
use wilkes_core::extract::ExtractorRegistry;
use wilkes_core::types::{EmbeddingEngine, IndexStatus, SelectedEmbedder};

pub struct BuildIndexOptions {
    pub manager: Option<wilkes_core::embed::worker::manager::WorkerManager>,
    pub device: Option<String>,
    pub data_dir: PathBuf,
    pub tx: ProgressTx,
    pub cancel_flag: Arc<AtomicBool>,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub supported_extensions: Vec<String>,
}

/// Download and install the model. Reports progress via `tx`.
pub async fn download_model(
    selected: SelectedEmbedder,
    manager: wilkes_core::embed::worker::manager::WorkerManager,
    device: String,
    data_dir: PathBuf,
    tx: ProgressTx,
) -> anyhow::Result<()> {
    let installer = wilkes_core::embed::dispatch::get_installer(
        selected.engine,
        selected.model,
        manager,
        device,
    );
    installer.install(&data_dir, tx).await
}

/// Walk `root`, embed every file using `embedder`, and write a new `SemanticIndex`
/// at `data_dir`. The embedder is returned so callers can cache it without reloading.
pub async fn build_index_with_embedder(
    root: PathBuf,
    embedder: Arc<dyn Embedder>,
    options: BuildIndexOptions,
) -> anyhow::Result<Arc<dyn Embedder>> {
    tracing::info!(
        "build_index_with_embedder: root={}, model={}, engine={:?}",
        root.display(),
        embedder.model_id(),
        embedder.engine()
    );
    let embedder_clone = Arc::clone(&embedder);

    let paths: Vec<PathBuf> = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file()
                && wilkes_core::types::FileType::detect(e.path(), &options.supported_extensions)
                    .is_some()
        })
        .map(|e| e.path().to_path_buf())
        .collect();
    tracing::info!(
        "build_index_with_embedder: collected {} candidate files",
        paths.len()
    );

    let data_dir_clone = options.data_dir.clone();
    let root_clone = root.clone();
    let indexing = wilkes_core::types::IndexingConfig {
        chunk_size: options.chunk_size,
        chunk_overlap: options.chunk_overlap,
        supported_extensions: options.supported_extensions.clone(),
    };

    tokio::task::spawn_blocking(move || {
        tracing::info!("build_index_with_embedder: spawn_blocking SemanticIndex::build start");
        let mut registry = ExtractorRegistry::new();
        registry.register(Box::new(PdfExtractor::new()));

        SemanticIndex::build(
            &data_dir_clone,
            &root_clone,
            &paths,
            &registry,
            embedder_clone.as_ref(),
            options.tx,
            options.cancel_flag,
            &indexing,
        )?;
        tracing::info!("build_index_with_embedder: SemanticIndex::build done");
        anyhow::Ok(())
    })
    .await??;

    Ok(embedder)
}

/// Walk `root`, embed every file, and write a new `SemanticIndex` at `data_dir`.
/// Returns the `Arc<dyn Embedder>` used during the build so the caller can store
/// it in state without loading the model a second time.
///
/// Cancellation is handled by the caller via `tokio::select!` on the returned
/// future; this function runs to completion once started.
pub async fn build_index(
    root: PathBuf,
    selected: SelectedEmbedder,
    options: BuildIndexOptions,
) -> anyhow::Result<Arc<dyn Embedder>> {
    let manager = options
        .manager
        .clone()
        .ok_or_else(|| anyhow::anyhow!("manager is required for build_index"))?;
    let device = options
        .device
        .clone()
        .ok_or_else(|| anyhow::anyhow!("device is required for build_index"))?;

    if matches!(selected.engine, EmbeddingEngine::Fastembed | EmbeddingEngine::Candle) {
        return build_index_via_worker(root, selected, manager, device, options).await;
    }

    let installer =
        wilkes_core::embed::dispatch::get_installer(selected.engine, selected.model, manager, device);

    build_index_with_installer(root, installer, options).await
}

async fn build_index_via_worker(
    root: PathBuf,
    selected: SelectedEmbedder,
    manager: WorkerManager,
    device: String,
    options: BuildIndexOptions,
) -> anyhow::Result<Arc<dyn Embedder>> {
    let request = WorkerRequest {
        mode: "build".to_string(),
        root,
        engine: selected.engine,
        model: selected.model.model_id().to_string(),
        data_dir: options.data_dir.clone(),
        chunk_size: Some(options.chunk_size),
        chunk_overlap: Some(options.chunk_overlap),
        device: device.clone(),
        paths: None,
        texts: None,
        supported_extensions: options.supported_extensions.clone(),
    };

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(128);
    manager
        .send(ManagerCommand::Submit {
            req: Box::new(request),
            reply: reply_tx,
        })
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send build command to manager: {e}"))?;

    while let Some(event) = reply_rx.recv().await {
        match event {
            WorkerEvent::Progress(progress) => {
                let _ = options.tx.send(progress).await;
            }
            WorkerEvent::Done => {
                let installer = wilkes_core::embed::dispatch::get_installer(
                    selected.engine,
                    selected.model.clone(),
                    manager,
                    device,
                );
                return installer.build(&options.data_dir);
            }
            WorkerEvent::Error(err) => {
                anyhow::bail!(err);
            }
            WorkerEvent::Embeddings(_) | WorkerEvent::Info { .. } => {}
        }
    }

    anyhow::bail!("Worker finished without returning build status")
}

pub async fn build_index_with_installer(
    root: PathBuf,
    installer: Arc<dyn EmbedderInstaller>,
    options: BuildIndexOptions,
) -> anyhow::Result<Arc<dyn Embedder>> {
    // Ensure model is ready (probes dimension for SBERT, no-op for others if already cached)
    installer
        .install(&options.data_dir, options.tx.clone())
        .await?;

    let embedder = installer.build(&options.data_dir)?;
    build_index_with_embedder(root, embedder, options).await
}

/// Fetch the total download size for `model_id` from the HuggingFace API.
pub async fn get_model_size(
    engine: wilkes_core::types::EmbeddingEngine,
    model_id: String,
) -> anyhow::Result<u64> {
    tokio::task::spawn_blocking(move || {
        wilkes_core::embed::dispatch::fetch_model_size(engine, &model_id)
    })
    .await?
}

/// Return all engine-supported models, annotated with local cache availability.
pub async fn list_models(
    engine: wilkes_core::types::EmbeddingEngine,
    data_dir: &Path,
) -> Vec<wilkes_core::types::ModelDescriptor> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        wilkes_core::embed::dispatch::list_models(engine, &data_dir)
    })
    .await
    .unwrap_or_default()
}

/// Read index status from disk without opening the full index.
pub async fn get_index_status(data_dir: &Path) -> anyhow::Result<IndexStatus> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || SemanticIndex::read_status_from_path(&data_dir)).await?
}

/// Delete the index database from disk.
pub async fn delete_index(data_dir: &Path) -> anyhow::Result<()> {
    let path = data_dir.join("semantic_index.db");
    tokio::fs::remove_file(&path).await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_get_index_status_missing() {
        let dir = tempdir().unwrap();
        let res = get_index_status(dir.path()).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_delete_index() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("semantic_index.db");
        std::fs::write(&db_path, "fake db").unwrap();

        delete_index(dir.path()).await.unwrap();
        assert!(!db_path.exists());
    }

    struct TestEmbedder;
    impl Embedder for TestEmbedder {
        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.0; 768]])
        }
        fn model_id(&self) -> &str {
            "mock"
        }
        fn dimension(&self) -> usize {
            768
        }
        fn engine(&self) -> wilkes_core::types::EmbeddingEngine {
            wilkes_core::types::EmbeddingEngine::Candle
        }
    }

    struct FakeInstaller {
        install_calls: Arc<AtomicUsize>,
        build_calls: Arc<AtomicUsize>,
        install_should_fail: bool,
    }

    #[async_trait::async_trait]
    impl EmbedderInstaller for FakeInstaller {
        fn is_available(&self, _data_dir: &Path) -> bool {
            true
        }

        async fn install(&self, _data_dir: &Path, _tx: ProgressTx) -> anyhow::Result<()> {
            self.install_calls.fetch_add(1, Ordering::Relaxed);
            if self.install_should_fail {
                anyhow::bail!("install failed")
            }
            Ok(())
        }

        fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
            Ok(())
        }

        fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
            self.build_calls.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(TestEmbedder))
        }
    }

    #[tokio::test]
    async fn test_build_index_with_embedder() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("files");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("test.txt"), "hello world").unwrap();

        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();

        let embedder = Arc::new(TestEmbedder);
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let supported_extensions = vec!["txt".to_string()];

        let options = BuildIndexOptions {
            manager: None,
            device: None,
            data_dir: data_dir.clone(),
            tx,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            chunk_size: 600,
            chunk_overlap: 128,
            supported_extensions,
        };

        let result = build_index_with_embedder(root, embedder, options).await;

        assert!(result.is_ok());

        let db_path = data_dir.join("semantic_index.db");
        assert!(db_path.exists());
    }

    #[tokio::test]
    async fn test_build_index_with_installer() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("files");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("test.txt"), "hello world").unwrap();

        let data_dir = dir.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();

        let install_calls = Arc::new(AtomicUsize::new(0));
        let build_calls = Arc::new(AtomicUsize::new(0));
        let installer = Arc::new(FakeInstaller {
            install_calls: Arc::clone(&install_calls),
            build_calls: Arc::clone(&build_calls),
            install_should_fail: false,
        });
        let (tx, _rx) = tokio::sync::mpsc::channel(10);

        let options = BuildIndexOptions {
            manager: None,
            device: None,
            data_dir: data_dir.clone(),
            tx,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            chunk_size: 600,
            chunk_overlap: 128,
            supported_extensions: vec!["txt".to_string()],
        };

        let result = build_index_with_installer(root, installer, options).await;

        assert!(result.is_ok());
        assert_eq!(install_calls.load(Ordering::Relaxed), 1);
        assert_eq!(build_calls.load(Ordering::Relaxed), 1);
        assert!(data_dir.join("semantic_index.db").exists());
    }

    #[tokio::test]
    async fn test_build_index_missing_options() {
        let dir = tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let options = BuildIndexOptions {
            manager: None,
            device: None,
            data_dir: dir.path().to_path_buf(),
            tx,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            chunk_size: 100,
            chunk_overlap: 10,
            supported_extensions: vec![],
        };

        let res = build_index(
            dir.path().to_path_buf(),
            wilkes_core::types::SelectedEmbedder {
                engine: wilkes_core::types::EmbeddingEngine::Candle,
                model: wilkes_core::types::EmbedderModel("m".to_string()),
                dimension: 384,
            },
            options,
        )
        .await;

        assert!(res.is_err());
        assert!(res
            .err()
            .unwrap()
            .to_string()
            .contains("manager is required"));
    }

    #[tokio::test]
    async fn test_list_models() {
        let dir = tempdir().unwrap();
        let models = list_models(wilkes_core::types::EmbeddingEngine::Fastembed, dir.path()).await;
        assert!(!models.is_empty());
    }

    #[tokio::test]
    async fn test_get_model_size_error() {
        // Should error for non-existent engine or invalid model
        let res = get_model_size(
            wilkes_core::types::EmbeddingEngine::Fastembed,
            "invalid".to_string(),
        )
        .await;
        assert!(res.is_err());
    }
}
