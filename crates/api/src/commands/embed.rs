use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ignore::WalkBuilder;
use wilkes_core::embed::index::db::{BuildOptions, BuildScope};
use wilkes_core::embed::index::{BuildReporter, SemanticIndex};
use wilkes_core::embed::installer::EmbedderInstaller;
use wilkes_core::embed::Embedder;
use wilkes_core::models::progress::ProgressTx;
use wilkes_core::types::{IndexStatus, RootCoverage, SelectedEmbedder};

pub struct BuildIndexOptions {
    pub manager: Option<wilkes_core::worker::manager::WorkerManager>,
    pub device: Option<String>,
    /// Texts the embedding worker may put through the model at once. Required
    /// alongside the device, and for the same reason: both say what the
    /// machine running the build can afford, and neither has a safe guess.
    pub batch_size: Option<usize>,
    /// Where model artefacts are cached. One directory for the whole
    /// installation; never the workspace's own directory, or every workspace
    /// downloads the same model again and derives its own embedding-space
    /// identity from its own copy.
    pub model_dir: PathBuf,
    /// Where the index is written. One per workspace.
    pub index_dir: PathBuf,
    /// How the build names what it is doing, to the interface and to the job
    /// journal. It carries the progress channel the model download also uses.
    pub reporter: BuildReporter,
    pub cancel_flag: Arc<AtomicBool>,
    /// The documents this build is over, and whether they are the whole root.
    ///
    /// They are decided by the caller rather than discovered here because the
    /// caller is what records the job, and a job whose scope is discovered
    /// somewhere else cannot say what is left until it has already finished
    /// finding out.
    pub documents: Vec<PathBuf>,
    pub scope: BuildScope,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub supported_extensions: Vec<String>,
}

/// Every indexable file under `root`, in walk order.
///
/// This is a build's scope when the user asked for the whole root, and it is
/// taken before the model is fetched so that the activity view can name the
/// corpus while the download is still running.
pub fn collect_indexable_paths(root: &Path, supported_extensions: &[String]) -> Vec<PathBuf> {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_file()
                && wilkes_core::types::FileType::detect(e.path(), supported_extensions).is_some()
        })
        .map(|e| e.path().to_path_buf())
        .collect()
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
        // Downloading artefacts runs no forward pass, so this installer never
        // embeds anything and the value cannot matter. The default rather than
        // a zero, so that an installer reused by mistake is slow rather than
        // broken.
        wilkes_core::types::SemanticSettings::default_embed_batch_size(),
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

    let paths = options.documents;
    tracing::info!(
        "build_index_with_embedder: {} document(s) in scope ({:?})",
        paths.len(),
        options.scope
    );

    let index_dir = options.index_dir;
    let root_clone = root.clone();
    let indexing = wilkes_core::types::IndexingConfig {
        chunk_size: options.chunk_size,
        chunk_overlap: options.chunk_overlap,
        supported_extensions: options.supported_extensions,
    };
    let build_options = match options.scope {
        BuildScope::WholeRoot => BuildOptions::new(options.reporter, options.cancel_flag),
        BuildScope::Subset => BuildOptions::over_subset(options.reporter, options.cancel_flag),
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
            build_options,
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
    let batch_size = options
        .batch_size
        .ok_or_else(|| anyhow::anyhow!("batch_size is required for build_index"))?;

    let installer = wilkes_core::embed::dispatch::get_installer(
        selected.engine,
        selected.model,
        manager,
        device,
        batch_size,
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
        .install(&options.model_dir, options.reporter.progress_tx())
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

/// How much of each root the index covers, one directory walk per root.
///
/// `settled_empty` carries, per root, the paths the job journal says were read
/// and found to hold no text. They are read out of the journal by the caller
/// and passed in rather than looked up here, so that the journal's lock is not
/// held across the walks below — a running build writes to it document by
/// document.
///
/// No model is loaded and no document is read: this is `files` rows against
/// directory entries. A root whose walk fails is reported as an error rather
/// than as an empty directory, because "nothing to index" and "could not look"
/// are opposite answers and only one of them means the user has nothing to do.
pub async fn root_coverage(
    data_dir: &Path,
    roots: Vec<PathBuf>,
    supported_extensions: Vec<String>,
    settled_empty: Vec<Vec<PathBuf>>,
) -> anyhow::Result<Vec<RootCoverage>> {
    anyhow::ensure!(
        roots.len() == settled_empty.len(),
        "root_coverage was given {} root(s) but {} journal reading(s)",
        roots.len(),
        settled_empty.len()
    );
    let data_dir = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let index = SemanticIndex::open_for_maintenance(&data_dir)?;
        roots
            .into_iter()
            .zip(settled_empty)
            .map(|(root, empty)| {
                let paths = collect_indexable_paths(&root, &supported_extensions);
                // A document the index holds is covered; so is one a job read
                // and found no text in, which the index has no row for and
                // never will. Counted as one set rather than two tallies, so
                // that a path both could name is counted once.
                let empty: std::collections::HashSet<PathBuf> = empty
                    .into_iter()
                    .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
                    .collect();
                let unheld: Vec<PathBuf> = index.unindexed_paths_for_root(&root, &paths)?;
                let missing = unheld
                    .into_iter()
                    .filter(|path| {
                        !empty
                            .contains(&std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
                    })
                    .count();
                Ok(RootCoverage {
                    indexable: paths.len(),
                    covered: paths.len() - missing,
                    complete: missing == 0,
                    root,
                })
            })
            .collect()
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
        let documents = collect_indexable_paths(&root, &supported_extensions);
        assert_eq!(documents.len(), 1, "the walk is the caller's now");

        let options = BuildIndexOptions {
            batch_size: Some(16),
            manager: None,
            device: None,
            model_dir: model_dir.clone(),
            index_dir: index_dir.clone(),
            reporter: BuildReporter::without_journal(tx),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            documents,
            scope: BuildScope::WholeRoot,
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

    /// Coverage is a claim about the directory as it is *now*, not as it was
    /// when the build ran. A root indexed last month and added to since is what
    /// the indicator exists to catch.
    #[tokio::test]
    async fn coverage_counts_files_added_since_the_build_as_missing() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("files");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("read.txt"), "hello world").unwrap();

        let index_dir = dir.path().join("workspace");
        std::fs::create_dir(&index_dir).unwrap();
        let model_dir = dir.path().join("models");
        std::fs::create_dir(&model_dir).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let supported_extensions = vec!["txt".to_string()];

        build_index_with_embedder(
            root.clone(),
            Arc::new(TestEmbedder),
            BuildIndexOptions {
                batch_size: Some(16),
                manager: None,
                device: None,
                model_dir,
                index_dir: index_dir.clone(),
                reporter: BuildReporter::without_journal(tx),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                documents: collect_indexable_paths(&root, &supported_extensions),
                scope: BuildScope::WholeRoot,
                chunk_size: 600,
                chunk_overlap: 128,
                supported_extensions: supported_extensions.clone(),
            },
        )
        .await
        .unwrap();

        let covered = root_coverage(
            &index_dir,
            vec![root.clone()],
            supported_extensions.clone(),
            vec![vec![]],
        )
        .await
        .unwrap();
        assert_eq!(covered[0].indexable, 1);
        assert_eq!(covered[0].covered, 1);
        assert!(covered[0].complete);

        std::fs::write(root.join("arrived-later.txt"), "new").unwrap();
        let covered = root_coverage(
            &index_dir,
            vec![root.clone()],
            supported_extensions.clone(),
            vec![vec![]],
        )
        .await
        .unwrap();
        assert_eq!(covered[0].indexable, 2);
        assert_eq!(covered[0].covered, 1);
        assert!(
            !covered[0].complete,
            "a file the index has never seen is missing"
        );

        // A document read and found to hold no text has no index row and never
        // will. Counting it as missing would leave a fully-read root reported
        // as incomplete for as long as it existed.
        let covered = root_coverage(
            &index_dir,
            vec![root.clone()],
            supported_extensions.clone(),
            vec![vec![root.join("arrived-later.txt")]],
        )
        .await
        .unwrap();
        assert_eq!(covered[0].covered, 2);
        assert!(covered[0].complete);
    }

    /// A root nothing has ever indexed is answered, not refused: "never
    /// indexed" is exactly what the caller is asking about.
    #[tokio::test]
    async fn coverage_reports_a_root_the_index_has_never_seen() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("files");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::write(root.join("b.txt"), "world").unwrap();

        let index_dir = dir.path().join("workspace");
        std::fs::create_dir(&index_dir).unwrap();
        let model_dir = dir.path().join("models");
        std::fs::create_dir(&model_dir).unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("c.txt"), "elsewhere").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let supported_extensions = vec!["txt".to_string()];

        // An index has to exist for coverage to be a claim about anything, so
        // one root is built and the *other* is the one under test.
        build_index_with_embedder(
            other.clone(),
            Arc::new(TestEmbedder),
            BuildIndexOptions {
                batch_size: Some(16),
                manager: None,
                device: None,
                model_dir,
                index_dir: index_dir.clone(),
                reporter: BuildReporter::without_journal(tx),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                documents: collect_indexable_paths(&other, &supported_extensions),
                scope: BuildScope::WholeRoot,
                chunk_size: 600,
                chunk_overlap: 128,
                supported_extensions: supported_extensions.clone(),
            },
        )
        .await
        .unwrap();

        let covered = root_coverage(
            &index_dir,
            vec![root.clone(), other.clone()],
            supported_extensions,
            vec![vec![], vec![]],
        )
        .await
        .unwrap();
        assert_eq!(covered[0].root, root);
        assert_eq!((covered[0].indexable, covered[0].covered), (2, 0));
        assert!(!covered[0].complete);
        assert!(
            covered[1].complete,
            "the built root is unaffected by the other"
        );
    }

    /// One reading per root, or the zip below would silently pair a root with
    /// another root's verdicts.
    #[tokio::test]
    async fn coverage_refuses_a_mismatched_number_of_journal_readings() {
        let dir = tempdir().unwrap();
        let err = root_coverage(
            dir.path(),
            vec![dir.path().to_path_buf(), dir.path().to_path_buf()],
            vec!["txt".to_string()],
            vec![vec![]],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("2 root(s) but 1"), "{err:#}");
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
            batch_size: Some(16),
            manager: None,
            device: None,
            model_dir: model_dir.clone(),
            index_dir: index_dir.clone(),
            reporter: BuildReporter::without_journal(tx),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            documents: collect_indexable_paths(&root, &["txt".to_string()]),
            scope: BuildScope::WholeRoot,
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
            batch_size: Some(16),
            manager: None,
            device: None,
            model_dir: dir.path().to_path_buf(),
            index_dir: dir.path().to_path_buf(),
            reporter: BuildReporter::without_journal(tx),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            documents: Vec::new(),
            scope: BuildScope::WholeRoot,
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
