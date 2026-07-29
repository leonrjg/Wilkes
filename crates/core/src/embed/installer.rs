use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use super::Embedder;
use crate::models::progress::ProgressTx;

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
