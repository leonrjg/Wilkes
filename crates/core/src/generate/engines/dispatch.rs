//! Engine dispatch for generation.
//!
//! Candle owns downloaded artifacts and a worker proxy; Ollama owns an HTTP
//! model store. This module is the common attachment and catalog boundary, so
//! callers never grow a second backend-specific lifecycle.

use std::path::Path;
use std::sync::Arc;

use crate::generate::{GenerationEngine, Generator};
use crate::models::progress::ProgressTx;
use crate::types::{GeneratorDescriptor, GeneratorModel};
use crate::worker::manager::WorkerManager;

use super::candle::GeneratorInstaller;
pub use super::candle::DEFAULT_GENERATOR_MODEL;

pub async fn list_models(
    engine: GenerationEngine,
    data_dir: &Path,
    ollama_url: &str,
) -> anyhow::Result<Vec<GeneratorDescriptor>> {
    match engine {
        GenerationEngine::Candle => Ok(super::candle::list_supported_models(data_dir)),
        GenerationEngine::Ollama => {
            let ollama_url = ollama_url.to_string();
            tokio::task::spawn_blocking(move || super::ollama::list_models(&ollama_url)).await?
        }
    }
}

fn candle_installer(
    model: GeneratorModel,
    manager: WorkerManager,
    device: String,
) -> Arc<dyn GeneratorInstaller> {
    Arc::new(super::candle::CandleGeneratorInstaller::new(
        model, manager, device,
    ))
}

/// Prepare and attach one generator through the lifecycle appropriate to its
/// engine. This is the sole main-process construction path for both backends.
pub async fn attach_generator(
    engine: GenerationEngine,
    model: GeneratorModel,
    manager: WorkerManager,
    device: String,
    data_dir: &Path,
    ollama_url: &str,
    context_tokens: Option<usize>,
    progress: ProgressTx,
) -> anyhow::Result<Arc<dyn Generator>> {
    let generator: Arc<dyn Generator> = match engine {
        GenerationEngine::Candle => {
            let installer = candle_installer(model, manager, device);
            installer.install(data_dir, progress).await?;
            installer.build(data_dir)?
        }
        GenerationEngine::Ollama => {
            drop(progress);
            let model_id = model.0;
            let ollama_url = ollama_url.to_string();
            tokio::task::spawn_blocking(move || {
                super::ollama::OllamaGenerator::connect(&ollama_url, &model_id, context_tokens)
            })
            .await??
        }
    };
    Ok(crate::generate::with_request_logging(generator))
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
        GenerationEngine::Ollama => {
            anyhow::bail!("Ollama generators are attached through the HTTP backend")
        }
    }
}

pub fn fetch_model_size(engine: GenerationEngine, model_id: &str) -> anyhow::Result<u64> {
    match engine {
        GenerationEngine::Candle => super::candle::fetch_model_size(model_id),
        GenerationEngine::Ollama => {
            anyhow::bail!("Ollama manages model downloads and reports sizes in its catalog")
        }
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
        GenerationEngine::Ollama => {
            anyhow::bail!("Ollama generation never runs in the Wilkes worker")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn list_models_returns_the_candle_catalog() {
        let dir = tempdir().unwrap();
        assert!(!list_models(GenerationEngine::Candle, dir.path(), "")
            .await
            .unwrap()
            .is_empty());
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
        let (manager, _rx, _fut) = WorkerManager::new(
            crate::worker::manager::WorkerPaths::resolve(dir.path()),
            crate::worker::ipc::WorkerKind::Generate,
        );
        let installer = candle_installer(
            GeneratorModel(DEFAULT_GENERATOR_MODEL.to_string()),
            manager,
            "cpu".to_string(),
        );
        assert!(!installer.is_available(dir.path()));
    }
}
