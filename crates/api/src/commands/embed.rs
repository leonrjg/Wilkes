use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ignore::WalkBuilder;
use wilkes_core::embed::index::SemanticIndex;
use wilkes_core::embed::installer::EmbedderInstaller;
use wilkes_core::embed::Embedder;
use wilkes_core::models::progress::ProgressTx;
use wilkes_core::types::{IndexStatus, SelectedEmbedder};

pub struct BuildIndexOptions {
    pub manager: Option<wilkes_core::worker::manager::WorkerManager>,
    pub device: Option<String>,
    /// Where model artefacts are cached. One directory for the whole
    /// installation; never the workspace's own directory, or every workspace
    /// downloads the same model again and derives its own embedding-space
    /// identity from its own copy.
    pub model_dir: PathBuf,
    /// Where the index is written. One per workspace.
    pub index_dir: PathBuf,
    pub tx: ProgressTx,
    pub cancel_flag: Arc<AtomicBool>,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub supported_extensions: Vec<String>,
}

/// Download and install the model into the installation-wide model cache.
/// Reports progress via `tx`.
pub async fn download_model(
    selected: SelectedEmbedder,
    manager: wilkes_core::worker::manager::WorkerManager,
    device: String,
    model_dir: PathBuf,
    tx: ProgressTx,
) -> anyhow::Result<()> {
    let installer = wilkes_core::embed::dispatch::get_installer(
        selected.engine,
        selected.model,
        manager,
        device,
    );
    installer.install(&model_dir, tx).await
}

/// Walk `root`, embed every file using `embedder`, and write a new `SemanticIndex`
/// at `index_dir`. The embedder is returned so callers can cache it without reloading.
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

    let index_dir = options.index_dir.clone();
    let root_clone = root.clone();
    let indexing = wilkes_core::types::IndexingConfig {
        chunk_size: options.chunk_size,
        chunk_overlap: options.chunk_overlap,
        supported_extensions: options.supported_extensions.clone(),
    };

    tokio::task::spawn_blocking(move || {
        tracing::info!("build_index_with_embedder: spawn_blocking SemanticIndex::build start");
        let registry = wilkes_core::extract::production_registry();

        SemanticIndex::build(
            &index_dir,
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

/// Walk `root`, embed every file, and write a new `SemanticIndex` at `index_dir`.
/// Returns the `Arc<dyn Embedder>` used during the build so the caller can store
/// it in state without loading the model a second time.
///
/// Every engine builds here, in the process that owns the settings. The
/// subprocess owns model inference and nothing else: each engine's installer
/// hands back a `WorkerEmbedder` that carries `embed` across the boundary, so
/// a crash in ONNX, CoreML, Metal or Python still cannot reach the host. What
/// must not cross is extraction — a second process extracting is a second
/// process deciding what a document says, and it decides it under whatever
/// `extract::image::configure` was never called on. That is not a hypothetical:
/// a build relocated into the worker read every PDF without the configured
/// image analyzer while the watcher, in this process, read the same PDFs with
/// it, and wrote both answers into one index under extraction recipes that
/// disagreed.
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

    let installer = wilkes_core::embed::dispatch::get_installer(
        selected.engine,
        selected.model,
        manager,
        device,
    );

    build_index_with_installer(root, installer, options).await
}

pub async fn build_index_with_installer(
    root: PathBuf,
    installer: Arc<dyn EmbedderInstaller>,
    options: BuildIndexOptions,
) -> anyhow::Result<Arc<dyn Embedder>> {
    // Ensure model is ready (probes dimension for SBERT, no-op for others if already cached)
    installer
        .install(&options.model_dir, options.tx.clone())
        .await?;

    let embedder = installer.build(&options.model_dir)?;
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

/// Read index status from disk without opening the full index.
pub async fn get_index_status(
    data_dir: &Path,
    root: Option<PathBuf>,
) -> anyhow::Result<IndexStatus> {
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        SemanticIndex::read_status_from_path_for_root(&data_dir, root.as_deref())
    })
    .await?
}

/// Delete the whole index database or, when `root` is supplied, only that root's coverage.
pub async fn delete_index(data_dir: &Path, root: Option<PathBuf>) -> anyhow::Result<()> {
    let data_dir = data_dir.to_path_buf();
    if let Some(root) = root {
        tokio::task::spawn_blocking(move || {
            let mut index = SemanticIndex::open_for_maintenance(&data_dir)?;
            index.delete_root(&root)
        })
        .await?
    } else {
        let path = data_dir.join("semantic_index.db");
        tokio::fs::remove_file(&path).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_get_index_status_missing() {
        let dir = tempdir().unwrap();
        let res = get_index_status(dir.path(), None).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_delete_index() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("semantic_index.db");
        std::fs::write(&db_path, "fake db").unwrap();

        delete_index(dir.path(), None).await.unwrap();
        assert!(!db_path.exists());
    }

    struct TestEmbedder;
    impl Embedder for TestEmbedder {
        fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
            wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

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

        let index_dir = dir.path().join("workspace");
        std::fs::create_dir(&index_dir).unwrap();
        let model_dir = dir.path().join("models");
        std::fs::create_dir(&model_dir).unwrap();

        let embedder = Arc::new(TestEmbedder);
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let supported_extensions = vec!["txt".to_string()];

        let options = BuildIndexOptions {
            manager: None,
            device: None,
            model_dir: model_dir.clone(),
            index_dir: index_dir.clone(),
            tx,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            chunk_size: 600,
            chunk_overlap: 128,
            supported_extensions,
        };

        let result = build_index_with_embedder(root, embedder, options).await;

        assert!(result.is_ok());

        assert!(index_dir.join("semantic_index.db").exists());
        // The index belongs to the workspace, the artefacts to the shared
        // cache: neither may be written where the other lives.
        assert!(!model_dir.join("semantic_index.db").exists());
    }

    #[tokio::test]
    async fn test_build_index_with_installer() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("files");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("test.txt"), "hello world").unwrap();

        let index_dir = dir.path().join("workspace");
        std::fs::create_dir(&index_dir).unwrap();
        let model_dir = dir.path().join("models");
        std::fs::create_dir(&model_dir).unwrap();

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
            model_dir: model_dir.clone(),
            index_dir: index_dir.clone(),
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
        assert!(index_dir.join("semantic_index.db").exists());
        assert!(!model_dir.join("semantic_index.db").exists());
    }

    #[tokio::test]
    async fn test_build_index_missing_options() {
        let dir = tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let options = BuildIndexOptions {
            manager: None,
            device: None,
            model_dir: dir.path().to_path_buf(),
            index_dir: dir.path().to_path_buf(),
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
