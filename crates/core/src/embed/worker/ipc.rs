use std::path::PathBuf;

use super::super::models::installer::EmbedProgress;
use crate::types::EmbeddingEngine;

/// Sent once from the desktop to the worker on stdin to configure the build.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
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
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum WorkerEvent {
    /// Forwarded from the index build progress channel.
    Progress(EmbedProgress),
    /// Embedding vectors returned by the "embed" mode.
    Embeddings(Vec<Vec<f32>>),
    /// Model metadata returned by the "info" mode.
    Info {
        dimension: usize,
        max_seq_length: usize,
    },
    /// Index build completed successfully.
    Done,
    /// Index build failed.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EmbeddingEngine;

    #[test]
    fn test_worker_request_serialization() {
        let req = WorkerRequest {
            mode: "build".to_string(),
            root: PathBuf::from("root"),
            engine: EmbeddingEngine::Fastembed,
            model: "model".to_string(),
            data_dir: PathBuf::from("data"),
            chunk_size: Some(100),
            chunk_overlap: Some(10),
            device: "cpu".to_string(),
            paths: None,
            texts: None,
            supported_extensions: vec!["txt".to_string()],
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: WorkerRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.mode, "build");
        assert_eq!(de.model, "model");
    }

    #[test]
    fn test_worker_event_serialization() {
        let events = vec![
            WorkerEvent::Done,
            WorkerEvent::Error("fail".to_string()),
            WorkerEvent::Info {
                dimension: 384,
                max_seq_length: 512,
            },
            WorkerEvent::Embeddings(vec![vec![1.0]]),
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            let _: WorkerEvent = serde_json::from_str(&json).unwrap();
        }
    }
}
