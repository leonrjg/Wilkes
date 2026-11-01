use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

use super::super::Embedder;
use super::aux_config;
use crate::embed::installer::EmbedderInstaller;
use crate::models::hf_hub::fetch_model_config;
use crate::models::progress::{EmbedProgress, ProgressTx};
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};
use crate::worker::embedder::{WorkerEmbedder, WorkerEmbedderConfig};
use crate::worker::ipc::{UnmetNeeds, WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};
use crate::worker::{
    base_transformers, forget_model_env, record_model_env, recorded_model_env, EnvNeeds, ModelEnv,
    TransformersSeries,
};

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
            // Only the download and the environment: a probe is not an index
            // build, and the worker's other progress would tell the interface
            // one had begun. `try_send` because progress is a notification and
            // the next one supersedes it; some callers probe with a channel
            // nobody reads, and waiting on one of those would hang the probe.
            WorkerEvent::Progress(
                progress @ (EmbedProgress::Download(_) | EmbedProgress::Setup(_)),
            ) => {
                if let Err(e) = progress_tx.try_send(progress) {
                    tracing::trace!("dropping probe progress for '{model_id}': {e}");
                }
            }
            WorkerEvent::Unmet(needs) => return Err(anyhow::Error::new(needs)),
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

/// What a model's `config.json` says about Transformers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ModelManifest {
    /// The version it was saved with.
    declared: Option<TransformersSeries>,
    /// It ships its own code, which is written against that version. Without
    /// any, its architecture ships with Transformers, which keeps it working
    /// from one release to the next, so the version says nothing it needs.
    custom_code: bool,
}

fn read_manifest(config: &str) -> Result<ModelManifest, String> {
    #[derive(serde::Deserialize)]
    struct ModelConfig {
        auto_map: Option<serde_json::Value>,
        transformers_version: Option<String>,
    }

    let config: ModelConfig =
        serde_json::from_str(config).map_err(|e| format!("Unreadable config.json: {e}"))?;
    Ok(ModelManifest {
        declared: config
            .transformers_version
            .as_deref()
            .map(TransformersSeries::parse)
            .transpose()?,
        custom_code: config.auto_map.is_some(),
    })
}

/// The environment a manifest calls for, `None` being the base one. Custom code
/// gets the series it was written for, on either side of the base one; whether
/// that series can be had here is for the resolver to say, not this.
fn needs_for(manifest: ModelManifest, base: TransformersSeries) -> Option<EnvNeeds> {
    let series = manifest.declared.filter(|_| manifest.custom_code)?;
    (series != base).then(|| EnvNeeds {
        transformers: Some(series),
        ..EnvNeeds::default()
    })
}

/// `needs` with what a failed load asked for added, or `None` if that adds
/// nothing and the failure stands. Asking for an unknown architecture means a
/// newer Transformers; an older one could not know it either.
fn refined_needs(
    needs: Option<&EnvNeeds>,
    unmet: &UnmetNeeds,
    declared: Option<TransformersSeries>,
    base: TransformersSeries,
) -> Option<EnvNeeds> {
    let mut refined = needs.cloned().unwrap_or_default();
    refined.packages.extend(unmet.packages.iter().cloned());
    if unmet.architecture {
        let current = refined.transformers.unwrap_or(base);
        refined.transformers = declared.filter(|d| *d > current).or(refined.transformers);
    }
    (refined != needs.cloned().unwrap_or_default()).then_some(refined)
}

/// The model's `config.json`, from wherever a load would find it. Waiting on
/// the network is no process a cancel could kill, so the wait watches the seal.
async fn model_config(manager: &WorkerManager, model_id: &str) -> anyhow::Result<Option<String>> {
    let local = Path::new(model_id);
    if local.is_dir() {
        let path = local.join("config.json");
        return match std::fs::read_to_string(&path) {
            Ok(config) => Ok(Some(config)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Reading {} failed: {e}", path.display())),
        };
    }

    let cache = hf_hub::Cache::new(manager.paths().hub_cache());
    if let Some(path) = cache
        .repo(hf_hub::Repo::model(model_id.to_string()))
        .get("config.json")
    {
        return std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("Reading {} failed: {e}", path.display()));
    }

    let id = model_id.to_string();
    let fetch = tokio::task::spawn_blocking(move || fetch_model_config(&id));
    tokio::select! {
        fetched = fetch => fetched?,
        () = manager.until_sealed() => Err(crate::worker::fault::WorkerFault::gone(
            "the user cancelled while reading the model's config",
        )),
    }
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
    /// The Python environment found by `install()`, for the same reason
    /// `dimension` is kept: `build()` has to hand on what the probe learned.
    /// Stays `None` for a built-in model, which the base environment is
    /// curated to run whatever its metadata declares.
    python_env: std::sync::Mutex<Option<EnvNeeds>>,
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
            python_env: std::sync::Mutex::new(None),
        }
    }

    /// The model's prefixes, from the cache the worker downloads into, which
    /// is not the `data_dir` every installer is handed.
    fn prefixes(&self) -> aux_config::EmbedderConfig {
        aux_config::load_prefixes(&self.manager.paths().hub_cache(), self.model.model_id())
    }

    /// Ask a worker in `python_env` for the model's width.
    async fn probe(
        &self,
        python_env: Option<EnvNeeds>,
        progress_tx: &ProgressTx,
    ) -> anyhow::Result<usize> {
        let model_id = self.model.model_id();
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
            python_env,
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

        await_probe(reply_rx, progress_tx, model_id).await
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

        // Otherwise, perform the Live Probe asynchronously, in the environment
        // the model needs. What it needs is only known for sure by trying, so
        // a probe that fails for want of something gets one more attempt with
        // it. One: a model that is still missing something after that is told
        // so, rather than an environment being built per guess.
        let paths = self.manager.paths();
        let base = base_transformers(paths).map_err(|e| anyhow::anyhow!(e))?;
        let manifest = match model_config(&self.manager, model_id).await? {
            Some(config) => read_manifest(&config)
                .map_err(|e| anyhow::anyhow!("Cannot read what '{model_id}' requires: {e}"))?,
            None => ModelManifest::default(),
        };
        let recorded = recorded_model_env(paths, model_id)
            .map_err(|e| anyhow::anyhow!(e))?
            .filter(|record| record.declared == manifest.declared);
        let mut python_env = match recorded {
            Some(record) => record.needs,
            None => needs_for(manifest, base),
        };

        let dimension = match self.probe(python_env.clone(), &progress_tx).await {
            Ok(dimension) => dimension,
            Err(failure) => {
                let unmet = failure
                    .downcast_ref::<UnmetNeeds>()
                    .and_then(|unmet| {
                        refined_needs(python_env.as_ref(), unmet, manifest.declared, base)
                    })
                    .ok_or(failure)?;
                tracing::info!(
                    "'{model_id}' needs more than its first environment had: trying {unmet:?}"
                );
                python_env = Some(unmet);
                self.probe(python_env.clone(), &progress_tx).await?
            }
        };

        record_model_env(
            paths,
            model_id,
            ModelEnv {
                declared: manifest.declared,
                needs: python_env.clone(),
            },
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        *self.python_env.lock().unwrap() = python_env;
        *self.dimension.lock().unwrap() = Some(dimension);
        Ok(())
    }

    /// Everything of a model's own: its weights and the record of what it
    /// needs. Its environment stays, since other models may share it, and is
    /// then the user's to remove.
    fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
        let model_id = self.model.model_id();
        let paths = self.manager.paths();
        let repo = paths
            .hub_cache()
            .join(format!("models--{}", model_id.replace('/', "--")));
        match std::fs::remove_dir_all(&repo) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => anyhow::bail!("Cannot remove {}: {e}", repo.display()),
        }
        forget_model_env(paths, model_id).map_err(|e| anyhow::anyhow!(e))
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

        let prefixes = self.prefixes();
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
                python_env: self.python_env.lock().unwrap().clone(),
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

    fn series(raw: &str) -> TransformersSeries {
        TransformersSeries::parse(raw).unwrap()
    }

    fn on_series(raw: &str) -> Option<EnvNeeds> {
        Some(EnvNeeds {
            transformers: Some(series(raw)),
            ..EnvNeeds::default()
        })
    }

    #[test]
    fn custom_code_gets_the_series_it_declares_on_either_side_of_the_base() {
        let base = series("4.57");
        let custom = |declared: &str| ModelManifest {
            declared: Some(series(declared)),
            custom_code: true,
        };

        assert_eq!(needs_for(custom("4.26.0"), base), on_series("4.26"));
        assert_eq!(needs_for(custom("4.47.0.dev0"), base), on_series("4.47"));
        assert_eq!(needs_for(custom("5.3.0"), base), on_series("5.3"));
        assert_eq!(needs_for(custom("4.57.1"), base), None);
    }

    #[test]
    fn a_model_with_no_custom_code_asks_for_nothing_up_front() {
        let plain = read_manifest(r#"{"transformers_version": "4.30.0"}"#).unwrap();
        assert_eq!(plain.declared, Some(series("4.30")));
        assert_eq!(needs_for(plain, series("5.22")), None);

        let undeclared = read_manifest(r#"{"auto_map": {"AutoModel": "m.M"}}"#).unwrap();
        assert!(undeclared.custom_code);
        assert_eq!(needs_for(undeclared, series("5.22")), None);
    }

    #[test]
    fn a_config_that_cannot_be_read_is_an_error_rather_than_no_requirement() {
        assert!(read_manifest("not json").is_err());
        assert!(read_manifest(r#"{"transformers_version": "recent"}"#).is_err());
    }

    fn unmet(packages: &[&str], architecture: bool) -> UnmetNeeds {
        UnmetNeeds {
            packages: packages.iter().map(|p| p.to_string()).collect(),
            architecture,
            detail: "failed".to_string(),
        }
    }

    #[test]
    fn a_missing_package_is_added_to_whatever_environment_it_was_missing_from() {
        let base = series("5.22");

        let on_base = refined_needs(None, &unmet(&["timm"], false), None, base).unwrap();
        assert_eq!(on_base.transformers, None);
        assert!(on_base.packages.contains("timm"));

        let current = on_series("4.47");
        let refined =
            refined_needs(current.as_ref(), &unmet(&["timm"], false), None, base).unwrap();
        assert_eq!(refined.transformers, Some(series("4.47")));
        assert!(refined.packages.contains("timm"));
    }

    #[test]
    fn an_unknown_architecture_is_answered_by_the_newer_transformers_it_declares() {
        let base = series("4.57");

        let newer = refined_needs(None, &unmet(&[], true), Some(series("5.3")), base);
        assert_eq!(newer.unwrap().transformers, Some(series("5.3")));

        // Nothing newer to offer, so the failure stands.
        assert_eq!(
            refined_needs(None, &unmet(&[], true), Some(series("4.30")), base),
            None
        );
        assert_eq!(refined_needs(None, &unmet(&[], true), None, base), None);
    }

    #[tokio::test]
    async fn a_probe_that_wants_for_something_says_what() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1);
        reply_tx
            .send(WorkerEvent::Unmet(unmet(&["timm"], false)))
            .await
            .unwrap();

        let err = await_probe(reply_rx, &progress_tx, "org/model")
            .await
            .unwrap_err();

        assert_eq!(err.downcast_ref::<UnmetNeeds>().unwrap().packages, ["timm"]);
    }

    #[tokio::test]
    async fn a_probe_passes_on_what_the_environment_setup_is_doing() {
        let (reply_tx, reply_rx) = tokio::sync::mpsc::channel(32);
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(8);
        reply_tx
            .send(WorkerEvent::Progress(EmbedProgress::Setup(
                "Installing Python 3.13".to_string(),
            )))
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

        assert!(matches!(
            progress_rx.try_recv(),
            Ok(EmbedProgress::Setup(message)) if message == "Installing Python 3.13"
        ));
    }

    #[test]
    fn uninstalling_removes_the_weights_and_the_record() {
        let dir = tempdir().unwrap();
        let requirements_path = dir.path().join("requirements.txt");
        fs::write(&requirements_path, "transformers==5.22.1\n").unwrap();
        let paths = crate::worker::manager::WorkerPaths {
            python_override: None,
            python_package_dir: dir.path().to_path_buf(),
            requirements_path,
            uv_bin: dir.path().join("uv"),
            worker_bin: dir.path().join("worker"),
            data_dir: dir.path().to_path_buf(),
        };
        let repo = paths.hub_cache().join("models--org--old-code");
        fs::create_dir_all(repo.join("snapshots")).unwrap();
        let record = ModelEnv {
            declared: Some(series("4.47")),
            needs: on_series("4.47"),
        };
        record_model_env(&paths, "org/old-code", record).unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            paths.clone(),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("org/old-code".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );

        installer.uninstall(dir.path()).unwrap();

        assert!(!repo.exists());
        assert!(recorded_model_env(&paths, "org/old-code")
            .unwrap()
            .is_none());
    }

    #[test]
    fn prefixes_are_read_from_where_the_worker_downloads_to() {
        let dir = tempdir().unwrap();
        let paths = crate::worker::manager::WorkerPaths::resolve(dir.path());
        let repo = paths.hub_cache().join("models--org--prompted");
        let snapshot = repo.join("snapshots").join("abc");
        fs::create_dir_all(&snapshot).unwrap();
        fs::create_dir_all(repo.join("refs")).unwrap();
        fs::write(repo.join("refs").join("main"), "abc").unwrap();
        fs::write(
            snapshot.join("config_sentence_transformers.json"),
            r#"{"prompts": {"query": "ask: ", "passage": "text: "}}"#,
        )
        .unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            paths,
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("org/prompted".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );

        let prefixes = installer.prefixes();

        assert_eq!(prefixes.query_prefix, "ask: ");
        assert_eq!(prefixes.passage_prefix, "text: ");
    }

    #[tokio::test]
    async fn a_built_in_model_stays_in_the_base_environment() {
        let dir = tempdir().unwrap();
        let (manager, _, _) = crate::worker::manager::WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Embed,
        );
        let installer = SBERTInstaller::new(
            EmbedderModel("nomic-ai/nomic-embed-text-v1".to_string()),
            manager,
            "cpu".to_string(),
            16,
        );
        let (tx, _) = tokio::sync::mpsc::channel(1);

        installer.install(dir.path(), tx).await.unwrap();

        assert_eq!(*installer.python_env.lock().unwrap(), None);
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
