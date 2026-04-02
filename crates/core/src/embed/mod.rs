pub mod chunk;
pub mod dispatch;
pub mod downloader;
pub mod index;
pub mod installer;
pub mod watcher;
pub mod worker_ipc;

#[cfg(feature = "fastembed")]
pub mod fastembed;

#[cfg(feature = "candle")]
pub mod candle;

use std::sync::Arc;

pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn model_id(&self) -> &str;
    fn dimension(&self) -> usize;

    /// Suggested batch size for this model.
    /// `None` means the entire input should be embedded as a single batch
    /// (e.g. for dynamically quantized models).
    fn preferred_batch_size(&self) -> Option<usize> {
        Some(32)
    }

    /// Embed texts that will be used as search queries.
    /// Override to add model-specific query prefixes (e.g. "query: " for E5).
    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }

    /// Embed texts that will be stored as indexed passages.
    /// Override to add model-specific passage prefixes (e.g. "passage: " for E5).
    fn embed_passages(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }
}

/// The active embedder is stored as `Mutex<Option<Arc<dyn Embedder>>>` in app state.
/// Only one embedder is live at a time because each model occupies significant memory.
pub type ActiveEmbedder = std::sync::Mutex<Option<Arc<dyn Embedder>>>;
