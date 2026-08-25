//! Fetching a file named by a URL into a library root.
//!
//! Lives in core rather than in the MCP server because there are now two
//! callers — the `download` tool an agent drives, and the HTTP route Underdog
//! drives when a learner admits a catalogue candidate — and a second
//! downloader would be a second set of answers to *may this URL be fetched*,
//! *where may it land*, and *is this already here*.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Cap on a single download. Checked twice: against the advertised
/// `Content-Length` before reading, and against the bytes actually received,
/// because a server may under-report or omit the header entirely.
pub const MAX_DOWNLOAD_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct DownloadParams {
    pub url: String,
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadResponse {
    pub path: String,
    pub bytes: usize,
    /// True when the exact content was already in the root under some other
    /// name. Nothing is written in that case; the existing path is returned.
    pub already_present: bool,
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

pub async fn download_to_root(
    root: &Path,
    params: DownloadParams,
) -> Result<DownloadResponse, String> {
    if !root.is_dir() {
        return Err(format!(
            "Current Wilkes root does not exist: {}",
            root.display()
        ));
    }
    let url = reqwest::Url::parse(params.url.trim())
        .map_err(|error| format!("Invalid download URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Download URL must use HTTP or HTTPS.".to_string());
    }
    let filename = params
        .filename
        .or_else(|| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "download.pdf".to_string());
    let filename_path = Path::new(&filename);
    if filename_path.components().count() != 1
        || !matches!(
            filename_path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("filename must be a single file name without directories.".to_string());
    }
    let target = root.join(filename_path);
    if target.exists() {
        return Err(format!(
            "Refusing to overwrite existing file: {}",
            target.display()
        ));
    }

    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Download failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Download failed: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64)
    {
        return Err(format!(
            "Download exceeds the {} MiB limit.",
            MAX_DOWNLOAD_BYTES / 1024 / 1024
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Failed to read download: {error}"))?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        return Err(format!(
            "Download exceeds the {} MiB limit.",
            MAX_DOWNLOAD_BYTES / 1024 / 1024
        ));
    }
    if let Some(existing) = find_file_with_content(root, &target, &bytes)? {
        return Ok(DownloadResponse {
            path: display_path(&existing),
            bytes: bytes.len(),
            already_present: true,
        });
    }
    std::fs::write(&target, &bytes)
        .map_err(|error| format!("Failed to save {}: {error}", target.display()))?;
    Ok(DownloadResponse {
        path: display_path(&target),
        bytes: bytes.len(),
        already_present: false,
    })
}

/// Find an existing regular file with exactly the downloaded content. Size is
/// the cheap prefilter; SHA-256 is only computed for equal-size candidates.
/// Symlinked directories are not followed, keeping the search inside `root`.
pub(crate) fn find_file_with_content(
    root: &Path,
    target: &Path,
    downloaded: &[u8],
) -> Result<Option<PathBuf>, String> {
    let expected_len = u64::try_from(downloaded.len()).unwrap_or(u64::MAX);
    let expected_digest = Sha256::digest(downloaded);
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("Failed to inspect {}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "Failed to inspect an entry in {}: {error}",
                    directory.display()
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!("Failed to inspect {}: {error}", entry.path().display())
            })?;
            if file_type.is_dir() {
                directories.push(entry.path());
                continue;
            }
            if !file_type.is_file() || entry.path() == target {
                continue;
            }
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))?;
            if metadata.len() != expected_len {
                continue;
            }
            let candidate = std::fs::read(&path)
                .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
            if Sha256::digest(&candidate) == expected_digest {
                return Ok(Some(path));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn download_rejects_path_traversal_and_existing_files() {
        let dir = tempdir().unwrap();
        let traversal = download_to_root(
            dir.path(),
            DownloadParams {
                url: "https://example.test/paper.pdf".to_string(),
                filename: Some("../paper.pdf".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(traversal.contains("single file name"));

        let existing = dir.path().join("paper.pdf");
        std::fs::write(&existing, b"existing").unwrap();
        let overwrite = download_to_root(
            dir.path(),
            DownloadParams {
                url: "https://example.test/paper.pdf".to_string(),
                filename: Some("paper.pdf".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(overwrite.contains("Refusing to overwrite"));
        assert_eq!(std::fs::read(existing).unwrap(), b"existing");
    }

    #[test]
    fn download_content_check_finds_equal_file_under_a_different_name() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("papers");
        std::fs::create_dir(&nested).unwrap();
        let existing = nested.join("original.pdf");
        std::fs::write(&existing, b"same paper").unwrap();
        std::fs::write(dir.path().join("same-size.pdf"), b"other text").unwrap();

        let found =
            find_file_with_content(dir.path(), &dir.path().join("new-name.pdf"), b"same paper")
                .unwrap();

        assert_eq!(found, Some(existing));
    }

    #[test]
    fn download_content_check_ignores_target_and_different_content() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("new-name.pdf");
        std::fs::write(&target, b"same paper").unwrap();
        std::fs::write(dir.path().join("other.pdf"), b"other text").unwrap();

        let found = find_file_with_content(dir.path(), &target, b"same paper").unwrap();

        assert_eq!(found, None);
    }
}
