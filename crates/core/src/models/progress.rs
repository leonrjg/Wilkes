use tokio::sync::mpsc;

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
