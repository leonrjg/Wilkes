use std::path::Path;
use std::sync::Arc;
use crate::types::{EmbeddingEngine, EmbedderModel, ModelDescriptor};
use super::super::Embedder;
use super::super::models::installer::EmbedderInstaller;
use super::super::worker::manager::WorkerManager;

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

    models.sort_by(|a, b| {
        b.is_default.cmp(&a.is_default)
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
        EmbeddingEngine::SBERT => Arc::new(super::sbert::SBERTInstaller::new(model, manager, device)),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => Arc::new(super::candle::CandleInstaller::new(model, manager, device)),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => panic!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => Arc::new(super::fastembed::FastembedInstaller::new(model, manager, device)),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => panic!("Fastembed feature is disabled"),
    }
}

/// Load the model directly in the calling process without going through IPC.
/// Must only be called from the worker subprocess — in the main Tauri process,
/// a crash in ONNX/CoreML/Metal would take down the whole app.
pub fn load_embedder_local(engine: EmbeddingEngine, model: &EmbedderModel, data_dir: &Path, device: &str) -> anyhow::Result<Arc<dyn Embedder>> {
    match engine {
        EmbeddingEngine::SBERT => anyhow::bail!("SBERT has no local embedder; it always runs in the Python worker"),

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
