pub mod engines;
pub mod models;
pub mod index;
pub mod worker;

pub use engines::dispatch;
pub use models::installer;
pub use worker::ipc as worker_ipc;
pub use worker::manager as worker_manager;

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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEmbedder;

    impl Embedder for TestEmbedder {
        fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0, 2.0]])
        }

        fn model_id(&self) -> &str {
            "test"
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    #[test]
    fn test_embedder_defaults() {
        let embedder = TestEmbedder;
        assert_eq!(embedder.preferred_batch_size(), Some(32));
        assert!(embedder.embed_query(&["a"]).is_ok());
        assert!(embedder.embed_passages(&["b"]).is_ok());
    }
}
