use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;
use crate::types::{EmbeddingEngine, EmbedderModel, ModelDescriptor};
use super::Embedder;
use super::installer::EmbedderInstaller;
use super::worker_manager::WorkerManager;

pub fn list_models(engine: EmbeddingEngine, data_dir: &Path) -> Vec<ModelDescriptor> {
    // Each engine provides its own builtin catalog, optionally checking data_dir
    // for models it has downloaded itself.
    let engine_models: Vec<ModelDescriptor> = match engine {
        EmbeddingEngine::SBERT => super::sbert::list_supported_models(),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::list_supported_models(data_dir),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => vec![],

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::list_supported_models(data_dir),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => vec![],
    };

    // Merge: start from the engine's list, then overlay the global HF cache so that
    // any model the user has downloaded externally (e.g. via SBERT or huggingface-cli)
    // is also visible here with is_cached = true.
    let mut by_id: HashMap<String, ModelDescriptor> = engine_models
        .into_iter()
        .map(|m| (m.model_id.clone(), m))
        .collect();

    super::hf_cache::overlay_hf_cache(&mut by_id);

    let mut result: Vec<ModelDescriptor> = by_id.into_values().collect();
    result.sort_by(|a, b| {
        b.is_default.cmp(&a.is_default)
            .then(b.is_cached.cmp(&a.is_cached))
            .then(a.model_id.cmp(&b.model_id))
    });
    result
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
        EmbeddingEngine::SBERT => super::hf_hub::fetch_model_size(_model_id),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::hf_hub::fetch_model_size(_model_id),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => anyhow::bail!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::fetch_model_size(_model_id),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => anyhow::bail!("Fastembed feature is disabled"),
    }
}
