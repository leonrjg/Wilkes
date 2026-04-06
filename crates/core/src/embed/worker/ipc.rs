use std::path::PathBuf;

use crate::types::{EmbeddingEngine};
use super::super::models::installer::EmbedProgress;

/// Sent once from the desktop to the worker on stdin to configure the build.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct WorkerRequest {
    #[serde(default = "default_mode")]
    pub mode: String, // "build" or "embed"
    pub root: PathBuf,
    pub engine: EmbeddingEngine,
    pub model: String, // HuggingFace model ID
    pub data_dir: PathBuf,
    /// Only used for "build" mode; absent (None) in "embed" and "info" requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_overlap: Option<usize>,
    #[serde(default = "default_device")]
    pub device: String, // "auto", "cpu", "mps", "cuda", etc.
    pub paths: Option<Vec<PathBuf>>, // Optional: incremental update for specific files
    pub texts: Option<Vec<String>>,  // Used by "embed" mode
    #[serde(default)]
    pub supported_extensions: Vec<String>,
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
    /// Embedding vectors returned by the "embed" mode.
    Embeddings(Vec<Vec<f32>>),
    /// Model metadata returned by the "info" mode.
    Info { dimension: usize, max_seq_length: usize },
    /// Index build completed successfully.
    Done,
    /// Index build failed.
    Error(String),
}
