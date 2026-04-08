use super::super::models::installer::EmbedderInstaller;
use super::super::worker::manager::WorkerManager;
use super::super::Embedder;
use crate::types::{EmbedderModel, EmbeddingEngine, ModelDescriptor};
use std::path::Path;
use std::sync::Arc;

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

pub fn fetch_model_size(engine: EmbeddingEngine, _model_id: &str) -> anyhow::Result<u64> {
    match engine {
        EmbeddingEngine::SBERT => super::super::models::hf_hub::fetch_model_size(_model_id),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::super::models::hf_hub::fetch_model_size(_model_id),
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
    use crate::embed::worker::manager::{WorkerManager, WorkerPaths};
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
        let (manager, _, _) = WorkerManager::new(WorkerPaths::resolve(dir.path()));

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
}
