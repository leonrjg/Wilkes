//! Engine dispatch for generation, mirroring `embed::engines::dispatch`.
//!
//! There is one engine today. The indirection exists so the call sites read the
//! same as their embedding counterparts, not to anticipate a second engine.

use std::path::Path;
use std::sync::Arc;

use crate::generate::{GenerationEngine, Generator};
use crate::models::progress::ProgressTx;
use crate::types::{GeneratorDescriptor, GeneratorModel};
use crate::worker::manager::WorkerManager;

pub use super::candle::{GeneratorInstaller, DEFAULT_GENERATOR_MODEL};

pub fn list_models(engine: GenerationEngine, data_dir: &Path) -> Vec<GeneratorDescriptor> {
    match engine {
        GenerationEngine::Candle => super::candle::list_supported_models(data_dir),
    }
}

pub fn get_installer(
    engine: GenerationEngine,
    model: GeneratorModel,
    manager: WorkerManager,
    device: String,
) -> Arc<dyn GeneratorInstaller> {
    match engine {
        GenerationEngine::Candle => Arc::new(super::candle::CandleGeneratorInstaller::new(
            model, manager, device,
        )),
    }
}

/// Load the model in the calling process. Must only be called from the worker
/// subprocess — in the main process a Metal fault would take down the app.
pub fn load_generator_local(
    engine: GenerationEngine,
    model: &GeneratorModel,
    data_dir: &Path,
    device: &str,
) -> anyhow::Result<Arc<dyn Generator>> {
    match engine {
        GenerationEngine::Candle => super::candle::load_generator(model, data_dir, device),
    }
}

pub fn fetch_model_size(engine: GenerationEngine, model_id: &str) -> anyhow::Result<u64> {
    match engine {
        GenerationEngine::Candle => super::candle::fetch_model_size(model_id),
    }
}

/// Download if needed, then load. The worker's entry point for a generation
/// request whose model is not yet resident.
pub async fn prepare_generator(
    engine: GenerationEngine,
    model: &GeneratorModel,
    data_dir: &Path,
    device: &str,
    progress: Option<ProgressTx>,
) -> anyhow::Result<Arc<dyn Generator>> {
    match engine {
        GenerationEngine::Candle => {
            let install_model = model.clone();
            let install_data_dir = data_dir.to_path_buf();
            tokio::task::spawn_blocking(move || {
                super::candle::install_local(&install_data_dir, &install_model, progress)
            })
            .await??;

            let model = model.clone();
            let data_dir = data_dir.to_path_buf();
            let device = device.to_string();
            tokio::task::spawn_blocking(move || {
                super::candle::load_generator(&model, &data_dir, &device)
            })
            .await?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn list_models_returns_the_candle_catalog() {
        let dir = tempdir().unwrap();
        assert!(!list_models(GenerationEngine::Candle, dir.path()).is_empty());
    }

    #[test]
    fn load_generator_local_fails_without_cached_weights() {
        let dir = tempdir().unwrap();
        let result = load_generator_local(
            GenerationEngine::Candle,
            &GeneratorModel(DEFAULT_GENERATOR_MODEL.to_string()),
            dir.path(),
            "cpu",
        );
        assert!(result.is_err());
    }

    #[test]
    fn installer_dispatch_yields_an_unavailable_installer_for_an_empty_dir() {
        let dir = tempdir().unwrap();
        let (manager, _rx, _fut) =
            WorkerManager::new(crate::worker::manager::WorkerPaths::resolve(dir.path()));
        let installer = get_installer(
            GenerationEngine::Candle,
            GeneratorModel(DEFAULT_GENERATOR_MODEL.to_string()),
            manager,
            "cpu".to_string(),
        );
        assert!(!installer.is_available(dir.path()));
    }
}
