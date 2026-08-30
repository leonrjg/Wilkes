//! `Generator` over IPC. Mirrors `worker::embedder::WorkerEmbedder`.

use std::ops::ControlFlow;

use crate::generate::{
    Generated, GenerationEngine, GenerationRequest, GenerationTimings, Generator, StopReason,
};
use crate::worker::ipc::{WorkerEvent, WorkerRequest, WorkerRole};
use crate::worker::manager::{ManagerCommand, WorkerManager};

pub struct WorkerGeneratorConfig {
    pub model_id: String,
    pub device: String,
    pub engine: GenerationEngine,
    pub context_tokens: usize,
    pub data_dir: std::path::PathBuf,
}

pub struct WorkerGenerator {
    manager: WorkerManager,
    /// Captured at construction (always in an async context) so `generate` can
    /// be called from non-Tokio threads.
    tokio_handle: tokio::runtime::Handle,
    model_id: String,
    device: String,
    engine: GenerationEngine,
    context_tokens: usize,
    data_dir: std::path::PathBuf,
    last_timings: std::sync::Mutex<Option<GenerationTimings>>,
}

impl WorkerGenerator {
    pub fn new(manager: WorkerManager, config: WorkerGeneratorConfig) -> Self {
        Self {
            manager,
            tokio_handle: tokio::runtime::Handle::current(),
            model_id: config.model_id,
            device: config.device,
            engine: config.engine,
            context_tokens: config.context_tokens,
            data_dir: config.data_dir,
            last_timings: std::sync::Mutex::new(None),
        }
    }

    fn worker_request(&self, req: GenerationRequest) -> WorkerRequest {
        WorkerRequest {
            mode: "generate".to_string(),
            role: WorkerRole::Generate(self.engine),
            model: self.model_id.clone(),
            model_dir: self.data_dir.clone(),
            device: self.device.clone(),
            texts: None,
            generate: Some(req),
            recognize: None,
        }
    }
}

impl Generator for WorkerGenerator {
    fn generate_stream(
        &self,
        req: GenerationRequest,
        sink: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> anyhow::Result<Generated> {
        *self
            .last_timings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let request = self.worker_request(req);
        // Tokens arrive one per event, so the channel needs room to buffer a
        // burst without stalling the worker between decode steps.
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let cmd = ManagerCommand::Submit {
            req: Box::new(request),
            reply: tx,
        };

        self.tokio_handle.block_on(async move {
            self.manager
                .send(cmd)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to send command to manager: {e}"))?;

            let mut text = String::new();
            let mut cancelled = false;

            while let Some(event) = rx.recv().await {
                match event {
                    WorkerEvent::GenerationRuntime(runtime) => {
                        self.manager.record_generation_runtime(runtime);
                    }
                    WorkerEvent::GenerationMetrics(timings) => {
                        *self
                            .last_timings
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            Some(timings.clone());
                        self.manager.record_generation_timings(timings);
                    }
                    WorkerEvent::Token { text: token } => {
                        if cancelled {
                            continue;
                        }
                        text.push_str(&token);
                        if sink(&token).is_break() {
                            cancelled = true;
                            // Ask the worker to stop decoding rather than
                            // burning the rest of the budget into a channel
                            // nobody reads. The reply channel stays open so the
                            // terminal event still arrives normally.
                            self.manager.cancel_active_request();
                        }
                    }
                    WorkerEvent::Completion { tokens, stop } => {
                        let stop = if cancelled {
                            StopReason::Cancelled
                        } else {
                            stop
                        };
                        return Ok(Generated { text, tokens, stop });
                    }
                    WorkerEvent::Error(err) => {
                        // A worker that dies mid-generation has already
                        // delivered tokens the caller rendered. Returning the
                        // partial text would be indistinguishable from success.
                        return Err(anyhow::anyhow!("Worker error: {err}"));
                    }
                    other => {
                        tracing::warn!(
                            "WorkerGenerator: ignoring unexpected worker event: {other:?}"
                        );
                    }
                }
            }

            Err(anyhow::anyhow!(
                "Worker finished without completing the generation"
            ))
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn context_tokens(&self) -> usize {
        self.context_tokens
    }

    fn last_timings(&self) -> Option<GenerationTimings> {
        self.last_timings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{Constraint, Sampling};
    use crate::worker::manager::WorkerPaths;
    use std::path::PathBuf;

    fn paths(dir: &std::path::Path) -> WorkerPaths {
        WorkerPaths {
            python_path: PathBuf::from("p"),
            python_package_dir: PathBuf::from("pkg"),
            requirements_path: PathBuf::from("r"),
            venv_dir: PathBuf::from("v"),
            worker_bin: dir.join("worker.sh"),
            data_dir: dir.to_path_buf(),
        }
    }

    fn request() -> GenerationRequest {
        GenerationRequest {
            system: None,
            prompt: "hi".to_string(),
            max_tokens: Some(8),
            constraint: Constraint::Text { stop: Vec::new() },
            sampling: Sampling::default(),
        }
    }

    fn config(dir: &std::path::Path) -> WorkerGeneratorConfig {
        WorkerGeneratorConfig {
            model_id: "m".to_string(),
            device: "cpu".to_string(),
            engine: GenerationEngine::Candle,
            context_tokens: 4096,
            data_dir: dir.to_path_buf(),
        }
    }

    #[cfg(unix)]
    fn write_worker_script(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn streams_tokens_in_order_and_returns_their_concatenation() {
        let dir = tempfile::tempdir().unwrap();
        write_worker_script(
            &dir.path().join("worker.sh"),
            r#"#!/bin/sh
read req
echo '{"Token":{"text":"one "}}'
echo '{"Token":{"text":"two "}}'
echo '{"Token":{"text":"three"}}'
echo '{"GenerationMetrics":{"prompt_micros":12,"decode_micros":34,"constraint_micros":5}}'
echo '{"Completion":{"tokens":3,"stop":"Eos"}}'
"#,
        );

        let (manager, _rx, loop_fut) =
            WorkerManager::new(paths(dir.path()), crate::worker::ipc::WorkerKind::Generate);
        tokio::spawn(loop_fut);
        let generator = std::sync::Arc::new(WorkerGenerator::new(manager, config(dir.path())));
        let decode_generator = std::sync::Arc::clone(&generator);

        let generated = tokio::task::spawn_blocking(move || decode_generator.generate(request()))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(generated.text, "one two three");
        assert_eq!(generated.tokens, 3);
        assert_eq!(generated.stop, StopReason::Eos);
        assert_eq!(
            generator.last_timings(),
            Some(GenerationTimings {
                prompt_micros: 12,
                decode_micros: 34,
                constraint_micros: 5,
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_error_mid_stream_is_an_error_not_partial_text() {
        let dir = tempfile::tempdir().unwrap();
        write_worker_script(
            &dir.path().join("worker.sh"),
            r#"#!/bin/sh
read req
echo '{"Token":{"text":"half a sen"}}'
echo '{"Error":"worker died"}'
"#,
        );

        let (manager, _rx, loop_fut) =
            WorkerManager::new(paths(dir.path()), crate::worker::ipc::WorkerKind::Generate);
        tokio::spawn(loop_fut);
        let generator = WorkerGenerator::new(manager, config(dir.path()));

        let mut streamed = String::new();
        let result = tokio::task::spawn_blocking(move || {
            generator.generate_stream(request(), &mut |token| {
                streamed.push_str(token);
                ControlFlow::Continue(())
            })
        })
        .await
        .unwrap();

        let err = result.unwrap_err().to_string();
        assert!(err.contains("worker died"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_that_never_completes_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write_worker_script(
            &dir.path().join("worker.sh"),
            r#"#!/bin/sh
read req
echo '{"Token":{"text":"x"}}'
"#,
        );

        let (manager, _rx, loop_fut) =
            WorkerManager::new(paths(dir.path()), crate::worker::ipc::WorkerKind::Generate);
        tokio::spawn(loop_fut);
        let generator = WorkerGenerator::new(manager, config(dir.path()));

        let result = tokio::task::spawn_blocking(move || generator.generate(request()))
            .await
            .unwrap();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn request_carries_the_generate_role_and_payload() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx, _loop_fut) =
            WorkerManager::new(paths(dir.path()), crate::worker::ipc::WorkerKind::Generate);
        let generator = WorkerGenerator::new(manager, config(dir.path()));

        let wire = generator.worker_request(request());
        assert_eq!(wire.mode, "generate");
        assert_eq!(wire.role, WorkerRole::Generate(GenerationEngine::Candle));
        assert_eq!(wire.generate.unwrap().prompt, "hi");
        assert_eq!(generator.context_tokens(), 4096);
        assert_eq!(generator.model_id(), "m");
    }
}
