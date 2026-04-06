use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;

use async_trait::async_trait;

use super::super::Embedder;

// ── Progress types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgress {
    pub bytes_received: u64,
    pub total_bytes: u64,
    pub done: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexBuildProgress {
    pub files_processed: usize,
    pub total_files: usize,
    pub message: String,
    pub done: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EmbedProgress {
    Download(DownloadProgress),
    Build(IndexBuildProgress),
}

pub type ProgressTx = mpsc::Sender<EmbedProgress>;

// ── EmbedderInstaller trait ───────────────────────────────────────────────────

#[async_trait]
pub trait EmbedderInstaller: Send + Sync {
    /// Returns true if the model files are present locally.
    fn is_available(&self, data_dir: &Path) -> bool;

    /// Download and install the model. Reports download progress via `tx`.
    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()>;

    /// Remove the model files.
    fn uninstall(&self, data_dir: &Path) -> anyhow::Result<()>;

    /// Construct the live embedder. Called after `install` succeeds (or if already available).
    fn build(&self, data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>>;
}
