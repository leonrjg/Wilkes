use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use super::super::Embedder;
use super::aux_config;
use crate::embed::installer::EmbedderInstaller;
use crate::models::progress::{EmbedProgress, ProgressTx};
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};
use crate::worker::embedder::{WorkerEmbedder, WorkerEmbedderConfig};
use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};

// ── Static model catalog ──────────────────────────────────────────────────────

struct ModelInfo {
    model_id: &'static str,
    display_name: &'static str,
    description: &'static str,
    dimension: usize,
    is_recommended: bool,
}

const PREEXISTING_MODELS: &[ModelInfo] = &[
    ModelInfo {
        model_id: "intfloat/e5-small-v2",
        display_name: "e5-small-v2",
        description: "Speed: high, accuracy: medium-high (English only)",
        dimension: 384,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "nomic-ai/nomic-embed-text-v1",
        display_name: "nomic-embed-text-v1",
        description: "Speed: high, accuracy: medium-high (English only)",
        dimension: 384,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "nomic-ai/nomic-embed-text-v1.5",
        display_name: "nomic-embed-text-v1.5",
        description: "Speed: medium, accuracy: medium-high (English only)",
        dimension: 768,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-MiniLM-L12-v2",
        display_name: "all-MiniLM-L12-v2",
        description: "Speed: high, accuracy: medium (English)",
        dimension: 384,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "jinaai/jina-embeddings-v5-text-small",
        display_name: "jina-embeddings-v5-text-small",
        description: "Speed: slow, accuracy: high (English only)",
        dimension: 1024,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "jinaai/jina-embeddings-v5-text-nano",
        display_name: "jina-embeddings-v5-text-nano",
        description: "Speed: slow, accuracy: medium (English only)",
        dimension: 768,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "sentence-transformers/all-mpnet-base-v2",
        display_name: "all-mpnet-base-v2",
        description: "Speed: medium, accuracy: medium (English only)",
        dimension: 768,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "minishlab/potion-multilingual-128M",
        display_name: "potion-multilingual-128M",
        description: "Speed: highest, accuracy: low",
        dimension: 256,
        is_recommended: false,
    },
    ModelInfo {
        model_id: "thenlper/gte-small",
        display_name: "gte-small",
        description: "Speed: medium, accuracy: low (English only)",
        dimension: 384,
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
            is_default: false,
            is_recommended: info.is_recommended,
            size_bytes: None,
            preferred_batch_size: Some(32),
        })
        .collect()
}

/// Wait for the worker's answer to a probe, however long the answer takes.
///
/// There is no deadline here, on purpose. Probing a model that is not cached
/// yet is its whole download and load, which depends on the size of the model
/// and the speed of the connection and says nothing about whether anything is
/// wrong. A deadline also never stopped that work: the worker carried on with
/// nobody waiting for it. A probe ends in the worker's answer, its error or its
/// death, and a user's cancel is one of those, because it kills the worker.
async fn await_probe(
    mut reply_rx: tokio::sync::mpsc::Receiver<WorkerEvent>,
    progress_tx: &ProgressTx,
    model_id: &str,
) -> anyhow::Result<usize> {
    while let Some(event) = reply_rx.recv().await {
        match event {
            WorkerEvent::Info { dimension, .. } => return Ok(dimension),
            // Only the download: a probe is not an index build, and the
            // worker's other progress would tell the interface one had begun.
            // `try_send` because progress is a notification and the next one
            // supersedes it; some callers probe with a channel nobody reads,
            // and waiting on one of those would hang the probe.
            WorkerEvent::Progress(progress @ EmbedProgress::Download(_)) => {
                if let Err(e) = progress_tx.try_send(progress) {
                    tracing::trace!("dropping download progress for '{model_id}': {e}");
                }
            }
            WorkerEvent::Error(err) => {
                return Err(anyhow::anyhow!("Worker error probing model: {}", err))
            }
            WorkerEvent::Gone(detail) => {
                return Err(crate::worker::fault::WorkerFault::gone(detail))
            }
            WorkerEvent::Done => break,
            _ => {}
        }
    }
    Err(anyhow::anyhow!(
        "Worker finished without returning model info for {}",
        model_id
    ))
}

// ── SBERTInstaller ───────────────────────────────────────────────────────────

pub struct SBERTInstaller {
    pub model: EmbedderModel,
    pub manager: WorkerManager,
    pub device: String,
    /// Texts one forward pass may hold. Travels with the device because it is
    /// the same kind of fact: what the machine this runs on can afford.
    pub batch_size: usize,
    pub dimension: std::sync::Mutex<Option<usize>>,
}

impl SBERTInstaller {
    pub fn new(
        model: EmbedderModel,
        manager: WorkerManager,
        device: String,
        batch_size: usize,
    ) -> Self {
        Self {
            model,
            manager,
            device,
            batch_size,
            dimension: std::sync::Mutex::new(None),
        }
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

    async fn install(&self, _data_dir: &Path, progress_tx: ProgressTx) -> anyhow::Result<()> {
        let model_id = self.model.model_id();

        // If it's a built-in model, we already know the dimension.
        if let Some(m) = PREEXISTING_MODELS.iter().find(|m| m.model_id == model_id) {
            *self.dimension.lock().unwrap() = Some(m.dimension);
            return Ok(());
        }

        // Otherwise, perform the Live Probe asynchronously.
        let request = WorkerRequest {
            batch_size: crate::worker::ipc::default_embed_batch_size(),
            mode: "info".to_string(),
            role: WorkerRole::Embed(EmbeddingEngine::SBERT),
            model: model_id.to_string(),
            model_dir: std::path::PathBuf::new(),
            device: self.device.clone(),
            texts: None,
            generate: None,
            recognize: None,
            layout: None,
            table: None,
        };

        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: reply_tx,
        };

        self.manager
            .send(cmd)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to send info command to manager: {e}"))?;

        let dimension = await_probe(reply_rx, &progress_tx, model_id).await?;
        *self.dimension.lock().unwrap() = Some(dimension);
        Ok(())
    }

    fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
        let model_id = self.model.model_id();

        // Use the dimension discovered during install() or from the built-in list.
        let dimension = self
            .dimension
            .lock()
            .unwrap()
            .or_else(|| {
                PREEXISTING_MODELS
                    .iter()
                    .find(|m| m.model_id == model_id)
                    .map(|m| m.dimension)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Dimension unknown for model '{}'. install() must be called before build().",
                    model_id
                )
            })?;

        let prefixes = aux_config::load_prefixes(_data_dir, model_id);
        let artifact_revision =
            crate::embed::identity::artifact_revision_for_cache(_data_dir, model_id)?;
        let embedding_space_identity = crate::embed::EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::SBERT,
            model_id,
            dimension,
            artifact_revision,
        );

        Ok(Arc::new(WorkerEmbedder::new(
            self.manager.clone(),
            WorkerEmbedderConfig {
                model_id: model_id.to_string(),
                dimension,
                device: self.device.clone(),
                engine: EmbeddingEngine::SBERT,
                data_dir: _data_dir.to_path_buf(),
                query_prefix: prefixes.query_prefix,
                passage_prefix: prefixes.passage_prefix,
                embedding_space_identity,
                batch_size: self.batch_size,
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_list_supported_models() {
        let dir = tempdir().unwrap();
        let models = list_supported_models(dir.path());
        assert!(!models.is_empty());
        let default_id = crate::types::EmbeddingEngine::SBERT.default_model();
        assert!(
            models.iter().any(|m| m.model_id == default_id),
            "Default model '{default_id}' must exist in the SBERT catalog"
        );
    }

    #[test]
    fn test_is_sbert_model_cached() {
        let dir = tempdir().unwrap();
        let model_id = "org/model";
        assert!(!is_sbert_model_cached(dir.path(), model_id));

        let folder = format!("models--{}", model_id.replace('/', "--"));
        let snapshots = dir.path().join(folder).join("snapshots");
        fs::create_dir_all(&snapshots).unwrap();
        assert!(!is_sbert_model_cached(dir.path(), model_id));

        fs::create_dir_all(snapshots.join("hash")).unwrap();
        assert!(is_sbert_model_cached(dir.path(), model_id));
    }

    #[test]
    fn test_sbert_installer_new() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("m".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );
        assert_eq!(installer.model.model_id(), "m");
        assert_eq!(installer.device, "cpu");
    }

    #[tokio::test]
    async fn test_sbert_installer_install_builtin() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("intfloat/e5-small-v2".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );
        let (tx, _) = tokio::sync::mpsc::channel(1);
        installer.install(dir.path(), tx).await.unwrap();
        assert_eq!(*installer.dimension.lock().unwrap(), Some(384));
    }

    #[test]
    fn test_sbert_installer_is_available() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("m".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );
        assert!(!installer.is_available(dir.path()));

        *installer.dimension.lock().unwrap() = Some(128);
        assert!(installer.is_available(dir.path()));

        let builtin = SBERTInstaller::new(
            EmbedderModel("intfloat/e5-small-v2".to_string()),
            crate::worker::manager::WorkerManager::new(
                crate::worker::manager::WorkerPaths::resolve(dir.path()),
                crate::worker::ipc::WorkerKind::Embed,
            )
            .0,
            "cpu".to_string(),
            16,
        );
        assert!(builtin.is_available(dir.path()));
    }

    #[tokio::test]
    async fn test_sbert_installer_build() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("intfloat/e5-small-v2".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );

        // Build should succeed for built-in models
        let embedder = installer.build(dir.path()).unwrap();
        assert_eq!(embedder.model_id(), "intfloat/e5-small-v2");
        assert_eq!(embedder.dimension(), 384);
        assert_eq!(embedder.engine(), EmbeddingEngine::SBERT);
    }

    #[test]
    fn test_sbert_installer_build_without_dimension_errors() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("custom/model".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );

        match installer.build(dir.path()) {
            Ok(_) => panic!("expected build to fail without a probed dimension"),
            Err(err) => assert!(err.to_string().contains("Dimension unknown")),
        }
    }

    /// The clock is paused, so the hours pass instantly: a probe still has to
    /// be waiting for its answer on the far side of them.
    #[tokio::test(start_paused = true)]
    async fn a_probe_waits_for_as_long_as_the_download_and_load_take() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
            reply_tx
                .send(WorkerEvent::Info {
                    dimension: 4096,
                    max_seq_length: 131072,
                })
                .await
                .unwrap();
        });

        let dimension = await_probe(reply_rx, &progress_tx, "org/large").await;

        assert_eq!(dimension.unwrap(), 4096);
    }

    #[tokio::test]
    async fn a_probe_passes_on_the_download_and_nothing_else() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(8);
        let build = crate::models::progress::IndexBuildProgress {
            files_processed: 0,
            total_files: 0,
            job_id: None,
            document: None,
            stage: None,
            outcome: None,
            done: false,
        };
        let download = crate::models::progress::DownloadProgress {
            bytes_received: 5,
            total_bytes: 10,
            done: false,
        };
        reply_tx
            .send(WorkerEvent::Progress(EmbedProgress::Build(build)))
            .await
            .unwrap();
        reply_tx
            .send(WorkerEvent::Progress(EmbedProgress::Download(download)))
            .await
            .unwrap();
        reply_tx
            .send(WorkerEvent::Info {
                dimension: 8,
                max_seq_length: 512,
            })
            .await
            .unwrap();

        await_probe(reply_rx, &progress_tx, "org/model")
            .await
            .unwrap();

        match progress_rx.try_recv() {
            Ok(EmbedProgress::Download(progress)) => assert_eq!(progress.bytes_received, 5),
            other => panic!("expected the download's progress, got {other:?}"),
        }
        assert!(progress_rx.try_recv().is_err());
    }

    /// `prepare_embedder` probes with a channel of one that nothing reads, so
    /// a probe that waited for room to report progress would never finish.
    #[tokio::test]
    async fn a_probe_is_not_held_up_by_progress_nobody_reads() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, _unread) = tokio::sync::mpsc::channel(1);
        for bytes_received in 0..3 {
            let progress = crate::models::progress::DownloadProgress {
                bytes_received,
                total_bytes: 3,
                done: false,
            };
            reply_tx
                .send(WorkerEvent::Progress(EmbedProgress::Download(progress)))
                .await
                .unwrap();
        }
        reply_tx
            .send(WorkerEvent::Info {
                dimension: 8,
                max_seq_length: 512,
            })
            .await
            .unwrap();

        let dimension = await_probe(reply_rx, &progress_tx, "org/model").await;

        assert_eq!(dimension.unwrap(), 8);
    }

    /// What a user's cancel looks like from here: the worker is killed and the
    /// probe ends at once, which is what keeps a download cancellable now that
    /// no deadline would have ended it.
    #[tokio::test]
    async fn a_probe_ends_when_its_worker_is_gone() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1);
        reply_tx
            .send(WorkerEvent::Gone("the user cancelled".to_string()))
            .await
            .unwrap();

        let err = await_probe(reply_rx, &progress_tx, "org/model")
            .await
            .unwrap_err();

        assert!(format!("{err:#}").contains("the user cancelled"));
    }

    #[test]
    fn test_list_supported_models_missing_cache() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");
        let models = list_supported_models(&missing);
        assert!(!models.is_empty());
        assert!(models.iter().all(|m| !m.is_cached));
    }
}
