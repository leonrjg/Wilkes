use std::path::PathBuf;

use crate::types::{EmbedderModel, EmbeddingEngine};
use super::installer::EmbedProgress;

/// Sent once from the desktop to the worker on stdin to configure the build.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct WorkerRequest {
    pub root: PathBuf,
    pub engine: EmbeddingEngine,
    pub model: EmbedderModel,
    pub data_dir: PathBuf,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
}

/// Lines emitted by the worker to stdout.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum WorkerEvent {
    /// Forwarded from the index build progress channel.
    Progress(EmbedProgress),
    /// Index build completed successfully.
    Done,
    /// Index build failed.
    Error(String),
}
