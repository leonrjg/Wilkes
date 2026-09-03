//! Raw file download for the models that are not on the HuggingFace hub.
//!
//! Most of Wilkes' artifacts come through `hf-hub`, which resolves a repo and a
//! revision and hands back a cached path. Not every pinned model lives there:
//! SLANet-plus is published by RapidAI on ModelScope, at a plain URL. That is a
//! second *source*, not a second *rule* — the rule is unchanged and lives where
//! it always has, in
//! [`crate::extract::image::verify_artifact`](crate::extract::image): a file is
//! installed only once its size and SHA-256 match the ones the module pinning
//! it declares. This fetches; that decides. A downloader that verified on its
//! own would be a second answer to "is this the file the recipe names", which
//! is exactly the question that must have one.
//!
//! Blocking, because every `install` in this tree is: they are called from the
//! command that offers the download, and an async one here would make that
//! command async to suit one artifact.

use std::io::{Read, Write};
use std::path::Path;

use super::progress::{DownloadProgress, EmbedProgress, ProgressTx};

/// Minimum gap between progress events, matching
/// [`crate::models::hf_hub`]'s: without it a large download emits one event per
/// chunk and floods the IPC channel.
const PROGRESS_EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// How much is read from the socket at a time.
const CHUNK_BYTES: usize = 64 * 1024;

/// Raw model file manager for artifacts that are not on the HuggingFace hub.
pub struct LocalModelManager;

impl LocalModelManager {
    /// Stream `url` to `dest`, reporting byte-level progress via `tx`.
    ///
    /// Written to a sibling temporary file and renamed on success, so a
    /// download that is interrupted — a broken socket, a killed process, a full
    /// disk — never leaves a short file where `is_installed` would count it. The
    /// caller still checks the finished file against its declared size and
    /// digest; a rename is what makes that check meaningful rather than a race.
    ///
    /// `expected_bytes` is the size the caller's own recipe pins, used as the
    /// progress denominator. Taken from the caller rather than from
    /// `Content-Length` because the recipe is what the file will be held to, and
    /// a server that announced a different length would otherwise report
    /// progress against a number nothing checks.
    pub fn download(
        url: &str,
        dest: &Path,
        expected_bytes: u64,
        tx: Option<ProgressTx>,
    ) -> anyhow::Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                anyhow::anyhow!("could not create {}: {error}", parent.display())
            })?;
        }

        let mut response = reqwest::blocking::Client::builder()
            // A deadline on reaching the host, and none on the transfer. This
            // is tens of megabytes over whatever connection the user has, and a
            // whole-request timeout — which is the only kind this `reqwest`
            // offers — would abort the honest slow download along with the
            // stalled one. A download that has stopped making progress is
            // stopped the way every other long operation in this application
            // is, by killing the process, and the partial file it leaves is
            // removed rather than counted as installed.
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(None)
            .build()
            .map_err(|error| anyhow::anyhow!("could not build an HTTP client: {error}"))?
            .get(url)
            .send()
            .map_err(|error| anyhow::anyhow!("could not fetch {url}: {error}"))?;
        anyhow::ensure!(
            response.status().is_success(),
            "{url} answered {}",
            response.status()
        );

        // Beside the destination rather than in the system temp: the bytes are
        // moved into place with a rename, and a rename across filesystems is a
        // copy that can fail halfway.
        let partial = dest.with_extension("partial");
        let mut file = std::fs::File::create(&partial)
            .map_err(|error| anyhow::anyhow!("could not create {}: {error}", partial.display()))?;

        let mut received = 0u64;
        let mut last_emit = std::time::Instant::now() - PROGRESS_EMIT_INTERVAL;
        let mut buffer = vec![0u8; CHUNK_BYTES];
        loop {
            let read = match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) => {
                    // Removed rather than left behind: a partial file whose
                    // name looked finished is the failure this whole function
                    // is shaped to avoid.
                    let _ = std::fs::remove_file(&partial);
                    anyhow::bail!("{url} stopped after {received} byte(s): {error}");
                }
            };
            if let Err(error) = file.write_all(&buffer[..read]) {
                let _ = std::fs::remove_file(&partial);
                anyhow::bail!("could not write {}: {error}", partial.display());
            }
            received += read as u64;
            if let Some(tx) = &tx {
                if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    emit(tx, received, expected_bytes, false);
                    last_emit = std::time::Instant::now();
                }
            }
        }
        if let Err(error) = file.sync_all() {
            let _ = std::fs::remove_file(&partial);
            anyhow::bail!("could not flush {}: {error}", partial.display());
        }
        drop(file);

        std::fs::rename(&partial, dest).map_err(|error| {
            let _ = std::fs::remove_file(&partial);
            anyhow::anyhow!("could not place {}: {error}", dest.display())
        })?;
        if let Some(tx) = &tx {
            emit(tx, received, expected_bytes, true);
        }
        Ok(())
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

/// Progress is lossy by nature: a full channel means the consumer is behind,
/// and the next tick carries the newer figure anyway.
fn emit(tx: &ProgressTx, bytes_received: u64, total_bytes: u64, done: bool) {
    if let Err(error) = tx.try_send(EmbedProgress::Download(DownloadProgress {
        bytes_received,
        total_bytes,
        done,
    })) {
        tracing::trace!("dropping download progress event: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// A URL nothing serves is an error naming it, never a file left where
    /// `is_downloaded` would count it.
    #[test]
    fn a_url_that_does_not_resolve_leaves_nothing_behind() {
        let dir = tempdir().unwrap();
        let dest = dir.path().join("model.onnx");
        let error = LocalModelManager::download(
            "http://127.0.0.1:1/there-is-no-server-here.onnx",
            &dest,
            10,
            None,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("there-is-no-server-here"),
            "the error should name what it tried to fetch: {error}"
        );
        assert!(!dest.exists());
        assert!(!dest.with_extension("partial").exists());
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
