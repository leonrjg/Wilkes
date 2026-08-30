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
use tokio::sync::mpsc;

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

/// How far a download has got.
///
/// `total_bytes` is what the server said it was sending, and its absence is
/// ordinary rather than exceptional — a chunked response has no length until
/// it ends. A consumer that assumed a total would show a progress bar that
/// never fills for exactly the downloads that take longest.
#[derive(Clone, Debug, Serialize)]
pub struct DownloadProgress {
    pub url: String,
    /// The name the bytes are being saved under, once it is known.
    pub filename: String,
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    /// True on the last report, whatever the total turned out to be.
    pub done: bool,
}

/// One progress report per this many bytes. A 40 MB textbook is then about
/// 160 reports: enough for a bar that moves, few enough that the reporting is
/// not itself the work.
const PROGRESS_STRIDE: u64 = 256 * 1024;

/// Reports without waiting to be heard.
///
/// `try_send` rather than `send().await`: progress is lossy by nature, and a
/// consumer that stopped draining must slow nothing down — least of all a
/// download it is no longer watching. The caller learns the outcome from the
/// return value, so a dropped report costs nothing that matters.
fn report(progress: Option<&mpsc::Sender<DownloadProgress>>, update: DownloadProgress) {
    if let Some(tx) = progress {
        let _ = tx.try_send(update);
    }
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
    progress: Option<mpsc::Sender<DownloadProgress>>,
) -> Result<DownloadResponse, String> {
    if !root.is_dir() {
        let message = format!("Current Wilkes root does not exist: {}", root.display());
        tracing::warn!("download refused: {message}");
        return Err(message);
    }
    let requested_url = params.url.trim().to_string();
    let url = reqwest::Url::parse(&requested_url).map_err(|error| {
        tracing::warn!(url = %requested_url, "download refused: invalid URL: {error}");
        format!("Invalid download URL: {error}")
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        tracing::warn!(url = %url, scheme = url.scheme(), "download refused: unsupported scheme");
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
        tracing::warn!(filename, "download refused: filename is not a single name");
        return Err("filename must be a single file name without directories.".to_string());
    }
    let target = root.join(filename_path);

    tracing::info!(url = %url, root = %root.display(), "download started");
    let response = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(url = %url, "download failed before any bytes: {error}");
            format!("Download failed: {error}")
        })?
        .error_for_status()
        .map_err(|error| {
            tracing::warn!(url = %url, "download refused by the server: {error}");
            format!("Download failed: {error}")
        })?;
    let total_bytes = response.content_length();
    if total_bytes.is_some_and(|length| length > MAX_DOWNLOAD_BYTES as u64) {
        tracing::warn!(
            url = %url,
            advertised = total_bytes,
            "download refused: over the size limit before reading"
        );
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

    // Read the body a chunk at a time rather than in one `bytes()` call. Two
    // reasons, and only one of them is the progress bar: a body that lied about
    // its length, or never declared one, is now refused at the moment it
    // crosses the limit instead of after it has all been buffered.
    let saved_as = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.clone());
    let mut response = response;
    let mut bytes: Vec<u8> = Vec::with_capacity(total_bytes.unwrap_or(0).min(1024 * 1024) as usize);
    let mut received: u64 = 0;
    let mut next_report: u64 = PROGRESS_STRIDE;
    report(
        progress.as_ref(),
        DownloadProgress {
            url: requested_url.clone(),
            filename: saved_as.clone(),
            received_bytes: 0,
            total_bytes,
            done: false,
        },
    );
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    url = %url,
                    received,
                    "download failed after {received} bytes: {error}"
                );
                return Err(format!("Failed to read download: {error}"));
            }
        };
        received += chunk.len() as u64;
        if received > MAX_DOWNLOAD_BYTES as u64 {
            tracing::warn!(url = %url, received, "download refused: over the size limit while reading");
            return Err(format!(
                "Download exceeds the {} MiB limit.",
                MAX_DOWNLOAD_BYTES / 1024 / 1024
            ));
        }
        bytes.extend_from_slice(&chunk);
        if received >= next_report {
            report(
                progress.as_ref(),
                DownloadProgress {
                    url: requested_url.clone(),
                    filename: saved_as.clone(),
                    received_bytes: received,
                    total_bytes,
                    done: false,
                },
            );
            next_report = received + PROGRESS_STRIDE;
        }
    }
    report(
        progress.as_ref(),
        DownloadProgress {
            url: requested_url.clone(),
            filename: saved_as,
            received_bytes: received,
            // Whatever the server claimed, this is what arrived, and the last
            // report is the one a bar settles on.
            total_bytes: Some(received),
            done: true,
        },
    );
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
            tracing::info!(
                url = %url,
                path = %target.display(),
                bytes = bytes.len(),
                "download already present under the same name; nothing written"
            );
            return Ok(DownloadResponse {
                path: display_path(&target),
                bytes: bytes.len(),
                already_present: true,
            });
        }
        tracing::warn!(
            url = %url,
            path = %target.display(),
            "download refused: a different file already has that name"
        );
        return Err(format!(
            "Refusing to overwrite existing file: {}",
            target.display()
        ));
    }
    if let Some(existing) = find_file_with_content(root, &target, &bytes)? {
        tracing::info!(
            url = %url,
            path = %existing.display(),
            bytes = bytes.len(),
            "download already present under another name; nothing written"
        );
        return Ok(DownloadResponse {
            path: display_path(&existing),
            bytes: bytes.len(),
            already_present: true,
        });
    }
    std::fs::write(&target, &bytes).map_err(|error| {
        tracing::warn!(path = %target.display(), "download could not be saved: {error}");
        format!("Failed to save {}: {error}", target.display())
    })?;
    tracing::info!(
        url = %url,
        path = %target.display(),
        bytes = bytes.len(),
        "download saved"
    );
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
            None,
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
            None,
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

        let first = download_to_root(dir.path(), params(), None).await.unwrap();
        assert!(!first.already_present);
        let second = download_to_root(dir.path(), params(), None).await.unwrap();
        assert!(second.already_present, "a second fetch is not a collision");
        assert_eq!(second.path, first.path);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_download_reports_its_bytes_as_they_arrive() {
        let dir = tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        // Larger than one stride, so the middle of the download is reported
        // and not only its two ends.
        let body = vec![b'x'; (PROGRESS_STRIDE * 2 + 1024) as usize];
        let expected = body.len() as u64;
        let _mock = server
            .mock("GET", "/book.pdf")
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let (tx, mut rx) = mpsc::channel(64);
        let response = download_to_root(
            dir.path(),
            DownloadParams {
                url: format!("{}/book.pdf", server.url()),
                filename: Some("book.pdf".to_string()),
            },
            Some(tx),
        )
        .await
        .expect("download");
        assert_eq!(response.bytes as u64, expected);

        let mut reports = Vec::new();
        while let Ok(update) = rx.try_recv() {
            reports.push(update);
        }
        assert!(reports.len() >= 3, "expected a middle, got {reports:?}");
        // The first report is sent before any byte has arrived, so a bar can
        // appear at the moment the click happens rather than a stride later.
        assert_eq!(reports[0].received_bytes, 0);
        assert!(!reports[0].done);
        assert!(
            reports
                .windows(2)
                .all(|w| w[0].received_bytes <= w[1].received_bytes),
            "progress must not go backwards: {reports:?}"
        );
        let last = reports.last().expect("a final report");
        assert!(last.done);
        assert_eq!(last.received_bytes, expected);
        // Whatever the server claimed, the last report settles on what arrived.
        assert_eq!(last.total_bytes, Some(expected));
        // The URL is echoed as it was requested, so a caller can match its own
        // request without knowing how the URL parser normalizes.
        assert_eq!(last.url, format!("{}/book.pdf", server.url()));
        assert_eq!(last.filename, "book.pdf");
    }

    #[tokio::test]
    async fn a_download_that_outgrows_the_limit_is_refused_while_it_reads() {
        let dir = tempdir().unwrap();
        let mut server = mockito::Server::new_async().await;
        // No `Content-Length` to check against: the body is only known to be
        // too big once enough of it has arrived, which is the case that used
        // to be caught after buffering all of it.
        let _mock = server
            .mock("GET", "/huge.pdf")
            .with_status(200)
            .with_chunked_body(|writer| {
                let chunk = vec![b'x'; 1024 * 1024];
                for _ in 0..(MAX_DOWNLOAD_BYTES / chunk.len() + 2) {
                    writer.write_all(&chunk)?;
                }
                Ok(())
            })
            .create_async()
            .await;

        let error = download_to_root(
            dir.path(),
            DownloadParams {
                url: format!("{}/huge.pdf", server.url()),
                filename: Some("huge.pdf".to_string()),
            },
            None,
        )
        .await
        .expect_err("a body past the limit must be refused");
        assert!(error.contains("exceeds"), "{error}");
        assert!(
            !dir.path().join("huge.pdf").exists(),
            "a refused download must leave nothing behind"
        );
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
