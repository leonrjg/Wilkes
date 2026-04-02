use std::path::PathBuf;

use crate::types::{EmbeddingEngine};
use super::installer::EmbedProgress;

/// Sent once from the desktop to the worker on stdin to configure the build.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct WorkerRequest {
    #[serde(default = "default_mode")]
    pub mode: String, // "build" or "list-models"
    pub root: PathBuf,
    pub engine: EmbeddingEngine,
    pub model: String, // HuggingFace model ID
    pub data_dir: PathBuf,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    #[serde(default = "default_device")]
    pub device: String, // "auto", "cpu", "mps", "cuda", etc.
    pub paths: Option<Vec<PathBuf>>, // Optional: incremental update for specific files
}

fn default_mode() -> String {
    "build".to_string()
}

fn default_device() -> String {
    "auto".to_string()
}

/// Lines emitted by the worker to stdout.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum WorkerEvent {
    /// Forwarded from the index build progress channel.
    Progress(EmbedProgress),
    /// List of models locally available in the HF cache.
    Models(Vec<ModelInfo>),
    /// Index build completed successfully.
    Done,
    /// Index build failed.
    Error(String),
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub dimension: usize,
    pub size_bytes: u64,
}
