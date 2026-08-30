use super::super::Embedder;
use crate::embed::installer::EmbedderInstaller;
use crate::models::progress::{DownloadProgress, EmbedProgress};
use crate::types::{
    CustomModel, EmbedderCapability, EmbedderCapabilityManifest, EmbedderModel, EmbeddingEngine,
    ModelDescriptor, PrefixSource,
};
use crate::worker::ipc::{WorkerEvent, WorkerKind};
use crate::worker::manager::WorkerManager;
use std::path::Path;
use std::sync::Arc;

pub struct PreparedEmbedder {
    pub embedder: Arc<dyn Embedder>,
    pub background_task: Option<tokio::task::JoinHandle<()>>,
}

pub fn list_models(engine: EmbeddingEngine, data_dir: &Path) -> Vec<ModelDescriptor> {
    // Each engine provides its own builtin catalog, checking data_dir for downloaded models.
    let mut models: Vec<ModelDescriptor> = match engine {
        EmbeddingEngine::SBERT => super::sbert::list_supported_models(data_dir),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::list_supported_models(data_dir),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => vec![],

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::list_supported_models(data_dir),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => vec![],
    };

    let default_model = engine.default_model();
    let mut found_default = false;
    for m in &mut models {
        m.is_default = m.model_id == default_model;
        if m.is_default {
            found_default = true;
        }
    }
    if !found_default {
        tracing::warn!(
            "Default model '{}' for engine {:?} not found in model catalog",
            default_model,
            engine
        );
    }

    models.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then(b.is_cached.cmp(&a.is_cached))
            .then(a.model_id.cmp(&b.model_id))
    });
    models
}

/// Everything this build can embed with, described rather than listed.
///
/// [`list_models`] answers what to put in a picker. This answers what choosing
/// one would *mean*: the width of its vectors, the prefixes its recipe
/// requires, whether its artifacts are here. A consumer that migrates a corpus
/// has to know those before it commits, and the alternative to asking here is
/// each consumer keeping its own table of model facts — which is how a
/// dimension typed into a form ends up outranking the engine that produces it.
///
/// `custom_models` are the ids the user added by hand. They are included so
/// the answer covers everything this Wilkes would accept, and they carry no
/// dimension: that is a property of weights nobody here has loaded yet.
pub fn model_capabilities(
    data_dir: &Path,
    custom_models: &[CustomModel],
) -> EmbedderCapabilityManifest {
    let engines = EmbeddingEngine::supported_engines();
    let mut models = Vec::new();
    for engine in &engines {
        for descriptor in list_models(*engine, data_dir) {
            let repository_id = repository_id(*engine, &descriptor.model_id);
            let (prefixes, prefix_source) = match &repository_id {
                Some(repo) => super::aux_config::describe(data_dir, repo),
                None => (Default::default(), PrefixSource::Undetermined),
            };
            models.push(EmbedderCapability {
                engine: *engine,
                model_id: descriptor.model_id.clone(),
                display_name: descriptor.display_name,
                description: descriptor.description,
                repository_id,
                dimension: Some(descriptor.dimension),
                supported_dimensions: vec![descriptor.dimension],
                query_prefix: Some(prefixes.query_prefix).filter(|p| !p.is_empty()),
                passage_prefix: Some(prefixes.passage_prefix).filter(|p| !p.is_empty()),
                prefix_source,
                max_input_tokens: prefixes.max_sequence_length,
                locally_available: descriptor.is_cached,
                size_bytes: descriptor.size_bytes,
                preferred_batch_size: descriptor.preferred_batch_size,
                catalogued: true,
                is_default: descriptor.is_default,
                is_recommended: descriptor.is_recommended,
            });
        }
    }
    for custom in custom_models {
        if !engines.contains(&custom.engine)
            || models
                .iter()
                .any(|model| model.engine == custom.engine && model.model_id == custom.model_id)
        {
            continue;
        }
        let (prefixes, prefix_source) = super::aux_config::describe(data_dir, &custom.model_id);
        models.push(EmbedderCapability {
            engine: custom.engine,
            model_id: custom.model_id.clone(),
            display_name: custom.model_id.clone(),
            description: "Added by hand; not in this engine's catalogue".to_string(),
            repository_id: Some(custom.model_id.clone()),
            dimension: None,
            supported_dimensions: Vec::new(),
            query_prefix: Some(prefixes.query_prefix).filter(|p| !p.is_empty()),
            passage_prefix: Some(prefixes.passage_prefix).filter(|p| !p.is_empty()),
            prefix_source,
            max_input_tokens: prefixes.max_sequence_length,
            locally_available: crate::models::hf_hub::is_model_cached(data_dir, &custom.model_id),
            size_bytes: None,
            preferred_batch_size: None,
            catalogued: false,
            is_default: false,
            is_recommended: false,
        });
    }
    EmbedderCapabilityManifest {
        engines,
        roles: vec!["query".to_string(), "passage".to_string()],
        models,
    }
}

/// The HuggingFace repository behind a catalogue id.
///
/// Two of the three engines are addressed by repository id already; fastembed
/// is addressed by its own enum name and has to be asked.
fn repository_id(engine: EmbeddingEngine, model_id: &str) -> Option<String> {
    match engine {
        EmbeddingEngine::SBERT => Some(model_id.to_string()),
        EmbeddingEngine::Candle => Some(model_id.to_string()),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::repository_id(model_id),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => None,
    }
}

pub fn get_installer(
    engine: EmbeddingEngine,
    model: EmbedderModel,
    manager: WorkerManager,
    device: String,
) -> Arc<dyn EmbedderInstaller> {
    match engine {
        EmbeddingEngine::SBERT => {
            Arc::new(super::sbert::SBERTInstaller::new(model, manager, device))
        }

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => {
            Arc::new(super::candle::CandleInstaller::new(model, manager, device))
        }
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => panic!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => Arc::new(super::fastembed::FastembedInstaller::new(
            model, manager, device,
        )),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => panic!("Fastembed feature is disabled"),
    }
}

/// Load the model directly in the calling process without going through IPC.
/// Must only be called from the worker subprocess — in the main Tauri process,
/// a crash in ONNX/CoreML/Metal would take down the whole app.
pub fn load_embedder_local(
    engine: EmbeddingEngine,
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Embedder>> {
    match engine {
        EmbeddingEngine::SBERT => {
            anyhow::bail!("SBERT has no local embedder; it always runs in the Python worker")
        }

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::load_embedder(model, data_dir, device),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => anyhow::bail!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::load_embedder(model, data_dir, device),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => anyhow::bail!("Fastembed feature is disabled"),
    }
}

async fn emit_download_progress(
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
    done: bool,
) {
    if let Some(tx) = event_tx {
        let _ = tx
            .send(WorkerEvent::Progress(EmbedProgress::Download(
                DownloadProgress {
                    bytes_received: 0,
                    total_bytes: 0,
                    done,
                },
            )))
            .await;
    }
}

/// Bridge a byte-level `ProgressTx` onto the worker's event channel. Returns the
/// sender to hand to the installer plus the join handle of the forwarder, which
/// finishes when the sender is dropped.
fn bridge_progress_to_worker_events(
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
) -> (
    Option<crate::models::progress::ProgressTx>,
    Option<tokio::task::JoinHandle<()>>,
) {
    let Some(event_tx) = event_tx.cloned() else {
        return (None, None);
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<EmbedProgress>(64);
    let handle = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            if event_tx
                .send(WorkerEvent::Progress(progress))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    (Some(tx), Some(handle))
}

pub async fn prepare_embedder(
    engine: EmbeddingEngine,
    model: &EmbedderModel,
    data_dir: &Path,
    device: &str,
    event_tx: Option<&tokio::sync::mpsc::Sender<WorkerEvent>>,
) -> anyhow::Result<PreparedEmbedder> {
    match engine {
        EmbeddingEngine::SBERT => {
            let paths = crate::worker::manager::WorkerPaths::resolve(data_dir);
            let (manager, _event_rx, loop_fut) =
                crate::worker::manager::WorkerManager::new(paths, WorkerKind::Embed);
            let background_task = tokio::spawn(loop_fut);
            let installer =
                super::sbert::SBERTInstaller::new(model.clone(), manager, device.to_string());
            let (probe_tx, _probe_rx) = tokio::sync::mpsc::channel(1);
            installer.install(data_dir, probe_tx).await?;
            let embedder = installer.build(data_dir)?;
            Ok(PreparedEmbedder {
                embedder,
                background_task: Some(background_task),
            })
        }

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => {
            emit_download_progress(event_tx, false).await;
            let (progress_tx, forwarder) = bridge_progress_to_worker_events(event_tx);
            let install_model = model.clone();
            let install_data_dir = data_dir.to_path_buf();
            let install = tokio::task::spawn_blocking(move || {
                super::candle::install_local(&install_data_dir, &install_model, progress_tx)
            })
            .await;
            if let Some(forwarder) = forwarder {
                // The blocking task owns the only sender; awaiting the forwarder
                // here drains every progress event before the install result is
                // reported.
                let _ = forwarder.await;
            }
            install??;
            let model = model.clone();
            let data_dir = data_dir.to_path_buf();
            let device = device.to_string();
            let embedder = tokio::task::spawn_blocking(move || {
                super::candle::load_embedder(&model, &data_dir, &device)
            })
            .await??;
            emit_download_progress(event_tx, true).await;
            Ok(PreparedEmbedder {
                embedder,
                background_task: None,
            })
        }
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => anyhow::bail!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => {
            let available = super::fastembed::is_model_available(data_dir, model);
            if !available {
                emit_download_progress(event_tx, false).await;
            }
            let model = model.clone();
            let data_dir = data_dir.to_path_buf();
            let device = device.to_string();
            let embedder = tokio::task::spawn_blocking(move || {
                super::fastembed::load_embedder(&model, &data_dir, &device)
            })
            .await??;
            if !available {
                emit_download_progress(event_tx, true).await;
            }
            Ok(PreparedEmbedder {
                embedder,
                background_task: None,
            })
        }
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => anyhow::bail!("Fastembed feature is disabled"),
    }
}

pub fn fetch_model_size(engine: EmbeddingEngine, _model_id: &str) -> anyhow::Result<u64> {
    match engine {
        EmbeddingEngine::SBERT => crate::models::hf_hub::fetch_model_size(_model_id),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => crate::models::hf_hub::fetch_model_size(_model_id),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => anyhow::bail!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::fetch_model_size(_model_id),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => anyhow::bail!("Fastembed feature is disabled"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::manager::{WorkerManager, WorkerPaths};
    use tempfile::tempdir;

    #[test]
    fn test_list_models_dispatch() {
        let dir = tempdir().unwrap();
        let sbert_models = list_models(EmbeddingEngine::SBERT, dir.path());
        assert!(!sbert_models.is_empty());

        #[cfg(feature = "candle")]
        {
            let candle_models = list_models(EmbeddingEngine::Candle, dir.path());
            assert!(!candle_models.is_empty());
        }

        #[cfg(feature = "fastembed")]
        {
            let fastembed_models = list_models(EmbeddingEngine::Fastembed, dir.path());
            assert!(!fastembed_models.is_empty());
        }
    }

    #[test]
    fn test_get_installer_dispatch() {
        let dir = tempdir().unwrap();
        let (manager, _, _) =
            WorkerManager::new(WorkerPaths::resolve(dir.path()), WorkerKind::Embed);

        let installer = get_installer(
            EmbeddingEngine::SBERT,
            EmbedderModel("intfloat/e5-small-v2".to_string()),
            manager.clone(),
            "cpu".to_string(),
        );
        assert!(installer.is_available(dir.path()));

        #[cfg(feature = "candle")]
        {
            let installer = get_installer(
                EmbeddingEngine::Candle,
                EmbedderModel("m".to_string()),
                manager.clone(),
                "cpu".to_string(),
            );
            assert!(!installer.is_available(dir.path()));
        }
    }

    #[test]
    fn test_load_embedder_local_dispatch() {
        let dir = tempdir().unwrap();
        let res = load_embedder_local(
            EmbeddingEngine::SBERT,
            &EmbedderModel("m".to_string()),
            dir.path(),
            "cpu",
        );
        match res {
            Err(e) => assert_eq!(
                e.to_string(),
                "SBERT has no local embedder; it always runs in the Python worker"
            ),
            _ => panic!("Expected error"),
        }
    }

    #[test]
    fn test_fetch_model_size_dispatch() {
        // Just verify it doesn't panic and reaches the SBERT branch
        let _ = fetch_model_size(EmbeddingEngine::SBERT, "invalid/model");
    }

    /// The manifest exists so a consumer stops keeping its own table of model
    /// facts. That only works if every catalogue entry states its dimension —
    /// and if a model nobody has loaded says so instead of guessing.
    #[test]
    fn every_catalogued_model_names_its_dimension_and_a_hand_added_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let custom = vec![CustomModel {
            engine: EmbeddingEngine::SBERT,
            model_id: "some-org/added-by-hand".to_string(),
        }];
        let manifest = model_capabilities(dir.path(), &custom);

        assert!(!manifest.models.is_empty());
        assert!(manifest.roles.iter().any(|role| role == "query"));
        for model in manifest.models.iter().filter(|model| model.catalogued) {
            assert!(
                model.dimension.is_some(),
                "{} is catalogued but states no dimension",
                model.model_id
            );
            assert_eq!(model.supported_dimensions, vec![model.dimension.unwrap()]);
        }

        let added = manifest
            .models
            .iter()
            .find(|model| model.model_id == "some-org/added-by-hand")
            .expect("a hand-added model belongs in the manifest");
        assert!(!added.catalogued);
        assert_eq!(
            added.dimension, None,
            "the width of unloaded weights is not something to fill in from a picker"
        );
        assert!(!added.locally_available);
        assert_eq!(added.prefix_source, PrefixSource::Undetermined);
    }

    /// fastembed addresses models by an enum name, so without the repository
    /// behind it the prefix reader looks in the wrong place — the defect that
    /// made a model's card unreachable in the first place.
    #[cfg(feature = "fastembed")]
    #[test]
    fn a_fastembed_entry_carries_the_repository_its_config_lives_in() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = model_capabilities(dir.path(), &[]);
        let mini = manifest
            .models
            .iter()
            .find(|model| {
                model.engine == EmbeddingEngine::Fastembed && model.model_id == "AllMiniLML6V2"
            })
            .expect("the pinned model is in the fastembed catalogue");
        let repository = mini.repository_id.as_deref().expect("a repository id");
        assert!(
            repository.contains('/') && repository != mini.model_id,
            "expected an HF repository id, got {repository}"
        );
        assert_eq!(mini.dimension, Some(384));
    }
}
