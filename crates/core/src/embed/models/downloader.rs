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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_local_model_manager_download_fails() {
        let dir = tempdir().unwrap();
        let (tx, _rx) = tokio::sync::mpsc::channel(1);

        let result = LocalModelManager::download(
            "http://example.com/model.bin",
            &dir.path().join("model.bin"),
            tx,
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not implemented"));
    }

    #[test]
    fn test_local_model_manager_is_downloaded() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("model.bin");

        assert!(!LocalModelManager::is_downloaded(&path));

        fs::write(&path, "data").unwrap();
        assert!(LocalModelManager::is_downloaded(&path));
    }

    #[test]
    fn test_local_model_manager_delete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("model.bin");

        // Deleting non-existent file should be ok
        assert!(LocalModelManager::delete(&path).is_ok());

        fs::write(&path, "data").unwrap();
        assert!(path.exists());

        assert!(LocalModelManager::delete(&path).is_ok());
        assert!(!path.exists());
    }
}
