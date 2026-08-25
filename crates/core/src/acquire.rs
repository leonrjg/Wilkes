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

/// The extension for a content type, for the cases a library actually serves.
///
/// Deliberately a short closed list rather than a mime database: an unknown
/// type is reported, not guessed at, because a wrong extension produces a file
/// that fails later and further away.
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "application/pdf" => Some("pdf"),
        "application/epub+zip" => Some("epub"),
        "text/plain" => Some("txt"),
        "text/markdown" => Some("md"),
        "text/html" => Some("html"),
        "application/json" => Some("json"),
        "application/zip" => Some("zip"),
        "application/msword" => Some("doc"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        _ => None,
    }
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
    // A URL whose last path segment carries no extension leaves a file that
    // nothing downstream can type: LibreTexts serves whole books from
    // `.../download/<id>/pdf`, which yields a file literally named `pdf`, and
    // an importer that reads the kind off the name then refuses it. When the
    // caller did not name the file and the URL gave no extension, take one
    // from what the server said it was sending.
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_string());
    let target = match (target.extension(), content_type.as_deref()) {
        (None, Some(mime)) => {
            let Some(extension) = extension_for_mime(mime) else {
                return Err(format!(
                    "Download has no file extension and its content type {mime:?} is not one \
                     we can name; pass an explicit filename."
                ));
            };
            target.with_extension(extension)
        }
        _ => target,
    };

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
    // The name is checked here rather than before the request, because
    // whether an existing file is a collision depends on what is in it. A
    // caller that fetches the same record twice — an acquisition retried after
    // the import failed, say — writes the same bytes to the same name, and
    // refusing that is refusing a no-op: the second attempt could never
    // succeed, and the first one's leftovers would have to be deleted by hand
    // before the button worked again. Different content under the same name is
    // still a refusal, and the user's file is still never overwritten.
    if target.exists() {
        let existing = std::fs::read(&target)
            .map_err(|error| format!("Failed to read {}: {error}", target.display()))?;
        if Sha256::digest(&existing) == Sha256::digest(&bytes) {
            return Ok(DownloadResponse {
                path: display_path(&target),
                bytes: bytes.len(),
                already_present: true,
            });
        }
        return Err(format!(
            "Refusing to overwrite existing file: {}",
            target.display()
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
    async fn download_rejects_a_filename_with_a_path_in_it() {
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
        // Refused before anything left the machine: the name is wrong on its
        // own terms and no request could make it right.
        assert!(!dir.path().join("paper.pdf").exists());
    }

    /// A file already there under the same name and holding different content
    /// is a real collision, and the existing file survives it.
    #[tokio::test]
    async fn download_refuses_to_overwrite_a_different_file_of_the_same_name() {
        let dir = tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/paper.pdf")
            .with_status(200)
            .with_body(b"downloaded")
            .create_async()
            .await;

        let existing = dir.path().join("paper.pdf");
        std::fs::write(&existing, b"existing").unwrap();
        let refusal = download_to_root(
            dir.path(),
            DownloadParams {
                url: format!("{}/paper.pdf", server.url()),
                filename: Some("paper.pdf".to_string()),
            },
        )
        .await
        .unwrap_err();
        assert!(refusal.contains("Refusing to overwrite"), "{refusal}");
        assert_eq!(std::fs::read(&existing).unwrap(), b"existing");
        mock.assert_async().await;
    }

    /// Fetching the same record twice is a no-op, not a refusal.
    ///
    /// The live case this comes from: an admission downloaded a book, the
    /// import after it failed, and every retry then died on the leftover file
    /// — a button that could not be pressed a second time until someone
    /// deleted a file by hand.
    #[tokio::test]
    async fn downloading_the_same_content_to_the_same_name_reports_it_as_present() {
        let dir = tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/paper.pdf")
            .with_status(200)
            .with_body(b"the same bytes")
            .expect(2)
            .create_async()
            .await;
        let params = || DownloadParams {
            url: format!("{}/paper.pdf", server.url()),
            filename: Some("paper.pdf".to_string()),
        };

        let first = download_to_root(dir.path(), params()).await.unwrap();
        assert!(!first.already_present);
        let second = download_to_root(dir.path(), params()).await.unwrap();
        assert!(second.already_present, "a second fetch is not a collision");
        assert_eq!(second.path, first.path);
        mock.assert_async().await;
    }

    #[test]
    fn a_content_type_names_a_file_the_url_left_unnamed() {
        // The real case: LibreTexts serves a whole book from `.../<id>/pdf`.
        assert_eq!(extension_for_mime("application/pdf"), Some("pdf"));
        assert_eq!(extension_for_mime("text/markdown"), Some("md"));
        // Unknown types are refused rather than guessed: a wrong extension
        // produces a file that fails somewhere further away.
        assert_eq!(extension_for_mime("application/x-nonsense"), None);
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
