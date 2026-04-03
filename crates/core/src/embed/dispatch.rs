use std::path::Path;
use std::sync::Arc;
use crate::types::{EmbeddingEngine, EmbedderModel, ModelDescriptor};
use super::installer::EmbedderInstaller;

pub fn list_models(engine: EmbeddingEngine, _data_dir: &Path) -> Vec<ModelDescriptor> {
    match engine {
        EmbeddingEngine::SBERT => super::hf_cache::list_cached_models(),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::list_supported_models(_data_dir),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => vec![],

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::list_supported_models(_data_dir),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => vec![],
    }
}

pub fn get_installer(engine: EmbeddingEngine, _model: EmbedderModel) -> Arc<dyn EmbedderInstaller> {
    match engine {
        EmbeddingEngine::SBERT => panic!("SBERT engine does not use in-process installers"),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => Arc::new(super::candle::CandleInstaller::new(_model)),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => panic!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => Arc::new(super::fastembed::FastembedInstaller::new(_model)),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => panic!("Fastembed feature is disabled"),
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
