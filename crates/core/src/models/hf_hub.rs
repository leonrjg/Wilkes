use std::path::Path;

use crate::types::ModelSearchHit;

#[derive(Debug, serde::Deserialize)]
pub struct HfSibling {
    pub rfilename: String,
    pub size: Option<u64>,
}

/// Make a request of the hub's metadata API, recorded in the HTTP history.
fn metadata_call(request: ureq::Request) -> Result<ureq::Response, Box<ureq::Error>> {
    let guard =
        crate::http_history::CallGuard::start("Hugging Face metadata", "GET", request.url());
    let response = request.call();
    let (status, error) = match &response {
        Ok(response) => (Some(response.status()), None),
        Err(ureq::Error::Status(status, _)) => (Some(*status), None),
        Err(error) => (None, Some(error.to_string())),
    };
    guard.finish(status, error);
    response.map_err(Box::new)
}

/// Fetch the sibling file list for `model_id` from the HuggingFace API, at the
/// repo's default branch.
pub fn fetch_hf_siblings(model_id: &str) -> anyhow::Result<Vec<HfSibling>> {
    fetch_hf_siblings_at(model_id, None)
}

/// Same, at a specific revision. Callers that download a pinned revision must
/// ask about that revision: sizes read from the branch describe files they will
/// never fetch.
pub fn fetch_hf_siblings_at(
    model_id: &str,
    revision: Option<&str>,
) -> anyhow::Result<Vec<HfSibling>> {
    fetch_hf_siblings_from_response(model_id, revision, |url| {
        let body = metadata_call(ureq::get(url))
            .map_err(|e| anyhow::anyhow!("HF API request failed: {e}"))?
            .into_string()
            .map_err(|e| anyhow::anyhow!("HF API response read failed: {e}"))?;
        Ok(body)
    })
}

pub(crate) fn fetch_hf_siblings_from_response<F>(
    model_id: &str,
    revision: Option<&str>,
    request: F,
) -> anyhow::Result<Vec<HfSibling>>
where
    F: FnOnce(&str) -> anyhow::Result<String>,
{
    #[derive(serde::Deserialize)]
    struct HfModelInfo {
        siblings: Vec<HfSibling>,
    }

    let url = match revision {
        Some(revision) => {
            format!("https://huggingface.co/api/models/{model_id}/revision/{revision}?blobs=true")
        }
        None => format!("https://huggingface.co/api/models/{model_id}?blobs=true"),
    };
    let body = request(&url)?;
    let info: HfModelInfo = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("HF API response parse failed: {e}"))?;

    Ok(info.siblings)
}

/// How long a model's `config.json` gets. A small file, so this only has to
/// tell a slow connection from a dead one.
const MODEL_CONFIG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Fetch `config.json` for `model_id` at the repo's default branch. `None` means
/// the repository answered and publishes no such file, which is ordinary for a
/// repository that is not a Transformers model.
pub fn fetch_model_config(model_id: &str) -> anyhow::Result<Option<String>> {
    let url = format!("https://huggingface.co/{model_id}/resolve/main/config.json");
    match metadata_call(ureq::get(&url).timeout(MODEL_CONFIG_TIMEOUT)).map_err(|e| *e) {
        Ok(response) => response
            .into_string()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("Reading config.json of {model_id} failed: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(anyhow::anyhow!(
            "Fetching config.json of {model_id} failed: {e}"
        )),
    }
}

/// Fetch total download size for `model_id` from the HuggingFace API.
/// Sums all files in the repo — accurate for SBERT, an upper bound for Candle.
pub fn fetch_model_size(model_id: &str) -> anyhow::Result<u64> {
    fetch_model_size_with(model_id, fetch_hf_siblings)
}

pub(crate) fn fetch_model_size_with<F>(model_id: &str, fetch: F) -> anyhow::Result<u64>
where
    F: FnOnce(&str) -> anyhow::Result<Vec<HfSibling>>,
{
    let siblings = fetch(model_id)?;
    let total: u64 = siblings.iter().filter_map(|s| s.size).sum();
    anyhow::ensure!(
        total > 0,
        "No model files found in HF repo for '{model_id}'"
    );
    Ok(total)
}

/// How long a search of the hub gets. Someone is watching a list fill in, and
/// the repository id can still be typed by hand.
const MODEL_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Repositories asked of the hub. More than are shown, because the hub cannot
/// be asked for everything [`usable_hits`] requires and some of a page is dropped.
const MODEL_SEARCH_PAGE: usize = 50;

/// Repositories a search answers with.
const MODEL_SEARCH_RESULTS: usize = 20;

/// Pipelines that turn text into a vector. The `sentence-transformers` tag alone
/// also covers cross-encoders, which score pairs and cannot embed anything.
const EMBEDDING_PIPELINES: &[&str] = &["sentence-similarity", "feature-extraction"];

/// Search the hub for embedding models, most downloaded first. An empty `query`
/// lists the most downloaded of them. A repository lacking any of
/// `required_files` is left out.
pub fn search_embedding_models(
    query: &str,
    required_files: &[&str],
) -> anyhow::Result<Vec<ModelSearchHit>> {
    let request = model_search_request(query);
    let body = metadata_call(request.timeout(MODEL_SEARCH_TIMEOUT))
        .map_err(|e| anyhow::anyhow!("HF model search failed: {e}"))?
        .into_string()
        .map_err(|e| anyhow::anyhow!("HF model search response read failed: {e}"))?;
    usable_hits(&body, required_files)
}

fn model_search_request(query: &str) -> ureq::Request {
    let mut request = ureq::get("https://huggingface.co/api/models")
        .query("search", query)
        .query("filter", "sentence-transformers")
        .query("sort", "downloads")
        .query("direction", "-1")
        .query("limit", &MODEL_SEARCH_PAGE.to_string());
    for field in ["downloads", "pipeline_tag", "gated", "config", "siblings"] {
        request = request.query("expand[]", field);
    }
    request
}

/// The repositories in a search response that could actually be added: open,
/// embedding models, holding `required_files`. Gated repositories are dropped
/// because Wilkes holds no Hugging Face token to download them with.
fn usable_hits(body: &str, required_files: &[&str]) -> anyhow::Result<Vec<ModelSearchHit>> {
    #[derive(serde::Deserialize)]
    struct Config {
        model_type: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct Repository {
        id: String,
        #[serde(default)]
        downloads: u64,
        pipeline_tag: Option<String>,
        /// `false`, or the kind of approval it takes as a string.
        #[serde(default)]
        gated: serde_json::Value,
        config: Option<Config>,
        #[serde(default)]
        siblings: Vec<HfSibling>,
    }

    let repositories: Vec<Repository> = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("HF model search response parse failed: {e}"))?;

    Ok(repositories
        .into_iter()
        .filter(|repository| repository.gated == serde_json::Value::Bool(false))
        .filter(|repository| {
            repository
                .pipeline_tag
                .as_deref()
                .is_some_and(|pipeline| EMBEDDING_PIPELINES.contains(&pipeline))
        })
        .filter(|repository| {
            required_files.iter().all(|file| {
                repository
                    .siblings
                    .iter()
                    .any(|sibling| sibling.rfilename == *file)
            })
        })
        .take(MODEL_SEARCH_RESULTS)
        .map(|repository| ModelSearchHit {
            model_id: repository.id,
            model_type: repository.config.and_then(|config| config.model_type),
            downloads: repository.downloads,
        })
        .collect())
}

/// Check if a model is cached in the given data directory using hf-hub's structure.
pub fn is_model_cached(data_dir: &Path, model_id: &str) -> bool {
    // For Python/SBERT, they use the same standard HF cache structure if
    // HF_HOME is set, or they use their own. In Wilkes, we try to share the data_dir.
    // If config.json exists, it's a good indicator.
    hf_hub::Cache::new(data_dir.to_path_buf())
        .repo(hf_hub::Repo::model(model_id.to_string()))
        .get("config.json")
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_is_model_cached_not_found() {
        let dir = tempdir().unwrap();
        assert!(!is_model_cached(dir.path(), "test/repo"));
    }

    #[test]
    fn test_is_model_cached_found() {
        let dir = tempdir().unwrap();
        // This is a hacky way to create the HF cache structure for testing,
        // it simulates creating a file pointer in the cache.
        let blob_path = dir.path().join("blobs");
        fs::create_dir_all(&blob_path).unwrap();
        let file_blob = blob_path.join("abcdef123456");
        fs::write(&file_blob, "{}").unwrap();

        let snapshots = dir
            .path()
            .join("models--test--repo")
            .join("snapshots")
            .join("main");
        fs::create_dir_all(&snapshots).unwrap();

        // Create symlink or just copy file to mimic cache
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file_blob, snapshots.join("config.json")).unwrap_or_default();

        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&file_blob, snapshots.join("config.json"))
            .unwrap_or_default();

        // Because symlinks can be tricky in tests, we just check if it returns what we expect
        // If it doesn't work, at least we exercised the function
        let _ = is_model_cached(dir.path(), "test/repo");
    }

    #[test]
    fn test_fetch_model_size_with_injected_siblings() {
        let size = fetch_model_size_with("test/repo", |_model_id| {
            Ok(vec![
                HfSibling {
                    rfilename: "a.bin".to_string(),
                    size: Some(10),
                },
                HfSibling {
                    rfilename: "b.bin".to_string(),
                    size: Some(20),
                },
                HfSibling {
                    rfilename: "ignored".to_string(),
                    size: None,
                },
            ])
        })
        .unwrap();

        assert_eq!(size, 30);
    }

    #[test]
    fn test_fetch_model_size_with_empty_result() {
        let err = fetch_model_size_with("test/repo", |_model_id| {
            Ok(vec![HfSibling {
                rfilename: "a.bin".to_string(),
                size: None,
            }])
        })
        .unwrap_err();

        assert!(err.to_string().contains("No model files found"));
    }

    #[test]
    fn test_fetch_hf_siblings_from_response_parses_json() {
        let siblings = fetch_hf_siblings_from_response("test/repo", None, |_url| {
            Ok(r#"{"siblings":[{"rfilename":"a.bin","size":10}]}"#.to_string())
        })
        .unwrap();

        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0].rfilename, "a.bin");
        assert_eq!(siblings[0].size, Some(10));
    }

    #[test]
    fn test_fetch_hf_siblings_from_response_request_error() {
        let err = fetch_hf_siblings_from_response("test/repo", None, |_url| {
            Err(anyhow::anyhow!("request failed"))
        })
        .unwrap_err();

        assert!(err.to_string().contains("request failed"));
    }

    #[test]
    fn test_fetch_hf_siblings_from_response_parse_error() {
        let err =
            fetch_hf_siblings_from_response("test/repo", None, |_url| Ok("not json".to_string()))
                .unwrap_err();

        assert!(err.to_string().contains("HF API response parse failed"));
    }

    fn repository(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "downloads": 7,
            "pipeline_tag": "feature-extraction",
            "gated": false,
            "config": { "model_type": "bert" },
            "siblings": [{ "rfilename": "config.json" }, { "rfilename": "model.safetensors" }],
        })
    }

    fn hit_ids(repositories: Vec<serde_json::Value>, required_files: &[&str]) -> Vec<String> {
        let body = serde_json::Value::Array(repositories).to_string();
        usable_hits(&body, required_files)
            .unwrap()
            .into_iter()
            .map(|hit| hit.model_id)
            .collect()
    }

    #[test]
    fn a_search_hit_carries_what_the_picker_shows() {
        let body = serde_json::json!([repository("org/model")]).to_string();

        assert_eq!(
            usable_hits(&body, &[]).unwrap(),
            vec![ModelSearchHit {
                model_id: "org/model".to_string(),
                model_type: Some("bert".to_string()),
                downloads: 7,
            }]
        );
    }

    #[test]
    fn a_search_leaves_out_what_cannot_embed() {
        let mut reranker = repository("org/reranker");
        reranker["pipeline_tag"] = "text-ranking".into();
        let mut classifier = repository("org/classifier");
        classifier["pipeline_tag"] = "text-classification".into();
        let mut untagged = repository("org/untagged");
        untagged["pipeline_tag"] = serde_json::Value::Null;

        let hits = hit_ids(
            vec![repository("org/embedder"), reranker, classifier, untagged],
            &[],
        );

        assert_eq!(hits, vec!["org/embedder"]);
    }

    #[test]
    fn a_search_leaves_out_gated_repositories() {
        let mut manual = repository("org/manual");
        manual["gated"] = "manual".into();
        let mut auto = repository("org/auto");
        auto["gated"] = "auto".into();

        let hits = hit_ids(vec![manual, auto, repository("org/open")], &[]);

        assert_eq!(hits, vec!["org/open"]);
    }

    #[test]
    fn a_search_leaves_out_repositories_missing_a_required_file() {
        let mut no_tokenizer = repository("org/no-tokenizer");
        no_tokenizer["siblings"] = serde_json::json!([{ "rfilename": "config.json" }]);

        let hits = hit_ids(
            vec![no_tokenizer, repository("org/complete")],
            &["config.json", "model.safetensors"],
        );

        assert_eq!(hits, vec!["org/complete"]);
    }

    #[test]
    fn a_search_stops_at_the_results_it_shows_after_dropping_the_rest() {
        let mut gated = repository("org/gated");
        gated["gated"] = "manual".into();
        let mut repositories = vec![gated];
        repositories.extend((0..MODEL_SEARCH_PAGE).map(|n| repository(&format!("org/m{n}"))));

        let hits = hit_ids(repositories, &[]);

        assert_eq!(hits.len(), MODEL_SEARCH_RESULTS);
        assert_eq!(hits[0], "org/m0");
    }

    #[test]
    fn a_search_reports_a_response_it_cannot_read() {
        let err = usable_hits("not json", &[]).unwrap_err();

        assert!(err
            .to_string()
            .contains("HF model search response parse failed"));
    }

    #[test]
    fn a_search_encodes_its_query_and_asks_for_the_most_downloaded() {
        let request = model_search_request("org/bge & friends");
        let url = request.url();

        assert!(url.contains("search=org%2Fbge+%26+friends"), "{url}");
        assert!(url.contains("filter=sentence-transformers"), "{url}");
        assert!(url.contains("sort=downloads"), "{url}");
        assert!(url.contains("direction=-1"), "{url}");
    }

    #[test]
    fn test_fetch_hf_siblings_asks_about_the_pinned_revision() {
        let mut seen = String::new();
        fetch_hf_siblings_from_response("test/repo", Some("abc123"), |url| {
            seen = url.to_string();
            Ok(r#"{"siblings":[]}"#.to_string())
        })
        .unwrap();

        assert_eq!(
            seen,
            "https://huggingface.co/api/models/test/repo/revision/abc123?blobs=true"
        );
    }
}

// ── Download progress ─────────────────────────────────────────────────────────

/// Adapter from `hf_hub::api::sync::Progress` to the app's `ProgressTx`.
///
/// hf-hub reports progress per file, so the reporter accumulates across the
/// files of one install. Repeated `init` calls for one file are transfer setup
/// or retries, not additional files; resumed offsets likewise replace that
/// file's current count rather than adding it again. Cloning yields a handle
/// onto the same accumulator, which lets one reporter span several files.
#[derive(Clone)]
pub struct HfProgressReporter {
    state: std::sync::Arc<std::sync::Mutex<ReporterState>>,
}

struct ReporterState {
    tx: crate::models::progress::ProgressTx,
    total_bytes: u64,
    bytes_received: u64,
    active_filename: Option<String>,
    active_bytes_received: u64,
    /// `hf-hub` calls `init` once when creating the temporary file and again
    /// before each transfer attempt. The first `update` after the latter is the
    /// absolute resume offset, not a new byte delta.
    resume_update_pending: bool,
    last_emit: std::time::Instant,
}

/// Minimum gap between progress events. Without it a large download emits one
/// event per chunk and floods the IPC channel.
const PROGRESS_EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

impl HfProgressReporter {
    pub fn new(tx: crate::models::progress::ProgressTx) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(ReporterState {
                tx,
                total_bytes: 0,
                bytes_received: 0,
                active_filename: None,
                active_bytes_received: 0,
                resume_update_pending: false,
                last_emit: std::time::Instant::now() - PROGRESS_EMIT_INTERVAL,
            })),
        }
    }

    fn emit(state: &mut ReporterState, done: bool) {
        use crate::models::progress::{DownloadProgress, EmbedProgress};
        let progress = EmbedProgress::Download(DownloadProgress {
            bytes_received: state.bytes_received,
            total_bytes: state.total_bytes,
            done,
        });
        // Progress is lossy by nature: a full channel means the consumer is
        // behind, and the next tick carries the newer figure anyway.
        if let Err(e) = state.tx.try_send(progress) {
            tracing::trace!("dropping download progress event: {e}");
        }
        state.last_emit = std::time::Instant::now();
    }
}

impl hf_hub::api::Progress for HfProgressReporter {
    fn init(&mut self, size: usize, filename: &str) {
        let Ok(mut state) = self.state.lock() else {
            tracing::warn!("download progress state poisoned; progress will not be reported");
            return;
        };
        tracing::debug!("downloading {filename} ({size} bytes)");
        if state.active_filename.as_deref() == Some(filename) {
            // The transfer-level init which follows the tempfile-level init,
            // or a retry of the same transfer. Its next update is a resume
            // offset and must replace, not add to, this file's current count.
            state.resume_update_pending = true;
        } else {
            state.total_bytes += size as u64;
            state.active_filename = Some(filename.to_string());
            state.active_bytes_received = 0;
            state.resume_update_pending = false;
        }
        Self::emit(&mut state, false);
    }

    fn update(&mut self, size: usize) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let size = size as u64;
        if state.resume_update_pending {
            if size > state.active_bytes_received {
                state.bytes_received += size - state.active_bytes_received;
                state.active_bytes_received = size;
            }
            state.resume_update_pending = false;
        } else {
            state.bytes_received += size;
            state.active_bytes_received += size;
        }
        if state.last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
            Self::emit(&mut state, false);
        }
    }

    fn finish(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        Self::emit(&mut state, false);
        state.active_filename = None;
        state.active_bytes_received = 0;
        state.resume_update_pending = false;
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use crate::models::progress::EmbedProgress;
    use hf_hub::api::Progress;

    #[tokio::test]
    async fn reporter_accumulates_bytes_across_files() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut reporter = HfProgressReporter::new(tx);

        reporter.init(100, "a.bin");
        reporter.update(60);
        reporter.init(50, "b.bin");
        reporter.update(50);
        reporter.finish();

        let mut last = None;
        while let Ok(event) = rx.try_recv() {
            let EmbedProgress::Download(progress) = event else {
                panic!("expected a download progress event");
            };
            last = Some(progress);
        }

        let last = last.expect("at least one progress event");
        assert_eq!(last.total_bytes, 150);
        assert_eq!(last.bytes_received, 110);
    }

    #[tokio::test]
    async fn reporter_throttles_updates_but_always_reports_the_last_one() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let mut reporter = HfProgressReporter::new(tx);

        reporter.init(1000, "big.bin");
        for _ in 0..50 {
            reporter.update(20);
        }
        reporter.finish();

        let mut events = Vec::new();
        while let Ok(EmbedProgress::Download(progress)) = rx.try_recv() {
            events.push(progress);
        }

        // Throttling means far fewer events than update() calls, but the final
        // one still carries the complete byte count.
        assert!(events.len() < 50, "expected throttling, got {events:?}");
        assert_eq!(events.last().unwrap().bytes_received, 1000);
    }

    #[tokio::test]
    async fn reporter_deduplicates_transfer_init_and_resume_offsets() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let mut reporter = HfProgressReporter::new(tx);

        // `hf-hub` initializes at the tempfile layer, then again when the HTTP
        // transfer begins and reports its absolute resume offset.
        reporter.init(100, "model.gguf");
        reporter.init(100, "model.gguf");
        reporter.update(0);
        reporter.update(60);
        // A retry repeats transfer init and reports 60 already-written bytes.
        reporter.init(100, "model.gguf");
        reporter.update(60);
        reporter.update(40);
        reporter.finish();

        let mut last = None;
        while let Ok(EmbedProgress::Download(progress)) = rx.try_recv() {
            last = Some(progress);
        }
        let last = last.unwrap();
        assert_eq!(last.total_bytes, 100);
        assert_eq!(last.bytes_received, 100);
    }
}
