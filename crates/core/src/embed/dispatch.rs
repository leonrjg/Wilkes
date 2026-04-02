use std::path::Path;
use std::sync::Arc;
use crate::types::{EmbeddingEngine, EmbedderModel, ModelDescriptor};
use super::installer::EmbedderInstaller;

pub fn list_models(engine: EmbeddingEngine, _data_dir: &Path) -> Vec<ModelDescriptor> {
    match engine {
        EmbeddingEngine::Python => vec![], // Handled by desktop spawning worker

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::list_supported_models(data_dir),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => vec![],

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::list_supported_models(data_dir),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => vec![],
    }
}

pub fn get_installer(engine: EmbeddingEngine, _model: EmbedderModel) -> Arc<dyn EmbedderInstaller> {
    match engine {
        EmbeddingEngine::Python => panic!("Python engine does not use in-process installers"),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => Arc::new(super::candle::CandleInstaller::new(model)),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => panic!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => Arc::new(super::fastembed::FastembedInstaller::new(model)),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => panic!("Fastembed feature is disabled"),
    }
}

pub fn fetch_model_size(engine: EmbeddingEngine, _model_id: &str) -> anyhow::Result<u64> {
    match engine {
        EmbeddingEngine::Python => anyhow::bail!("Python engine model size fetched via worker"),

        #[cfg(feature = "candle")]
        EmbeddingEngine::Candle => super::candle::fetch_model_size(model_id),
        #[cfg(not(feature = "candle"))]
        EmbeddingEngine::Candle => anyhow::bail!("Candle feature is disabled"),

        #[cfg(feature = "fastembed")]
        EmbeddingEngine::Fastembed => super::fastembed::fetch_model_size(model_id),
        #[cfg(not(feature = "fastembed"))]
        EmbeddingEngine::Fastembed => anyhow::bail!("Fastembed feature is disabled"),
    }
}
