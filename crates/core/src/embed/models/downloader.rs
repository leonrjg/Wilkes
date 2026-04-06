use std::path::Path;

use super::installer::ProgressTx;

/// Raw ONNX model file manager for non-fastembed backends.
/// Not used by the fastembed path — `FastembedInstaller` relies on fastembed's
/// internal HF Hub download. A future `OnnxInstaller` would use this.
pub struct LocalModelManager;

impl LocalModelManager {
    /// Stream `url` to `dest`, reporting byte-level progress via `tx`.
    pub async fn download(_url: &str, _dest: &Path, _tx: ProgressTx) -> anyhow::Result<()> {
        // Requires reqwest + stream support; not needed for the fastembed backend.
        // A future OnnxInstaller would implement this with reqwest::get(url).
        Err(anyhow::anyhow!(
            "LocalModelManager::download is not implemented; use FastembedInstaller instead"
        ))
    }

    pub fn is_downloaded(dest: &Path) -> bool {
        dest.exists()
    }

    pub fn delete(dest: &Path) -> anyhow::Result<()> {
        if dest.exists() {
            std::fs::remove_file(dest)?;
        }
        Ok(())
    }
}
