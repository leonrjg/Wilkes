use super::restore::*;
use super::*;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use tempfile::tempdir;

fn hashes_of(texts: &[&str]) -> Vec<String> {
    texts
        .iter()
        .map(|text| wilkes_core::embed::identity::sha256_bytes(text.as_bytes()))
        .collect()
}

/// A provider whose search answers with fixed works, or fails.
struct TitleSearchSource(Option<Vec<(&'static str, &'static str)>>);

#[async_trait::async_trait]
impl LiteratureSource for TitleSearchSource {
    fn id(&self) -> &str {
        "fake"
    }
    fn name(&self) -> &str {
        "Fake"
    }
    fn manifest(&self) -> Option<&wilkes_core::integrations::custom::manifest::Manifest> {
        None
    }
    fn base_url(&self) -> &str {
        "http://fake.invalid"
    }
    fn capabilities(&self) -> wilkes_core::integrations::ProviderCapabilities {
        wilkes_core::integrations::ProviderCapabilities {
            paged_search: false,
            browse: None,
            lookup: true,
            references: false,
            resolve_download: false,
            mirrored: false,
        }
    }
    async fn search(
        &self,
        _query: &str,
        _limit: usize,
        _page: usize,
    ) -> anyhow::Result<Vec<wilkes_core::types::LiteratureSearchResult>> {
        let works = self
            .0
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("search is down"))?;
        Ok(works
            .iter()
            .map(|(title, doi)| {
                serde_json::from_value(serde_json::json!({
                    "id": doi, "doi": doi, "title": title, "year": null,
                    "publication_date": null, "venue": null, "citation_count": null,
                    "is_open_access": false, "pdf_url": null, "landing_page_url": null,
                    "open_access_status": null, "license": null,
                }))
                .unwrap()
            })
            .collect())
    }
    async fn status(
        &self,
        _enabled: bool,
    ) -> anyhow::Result<wilkes_core::types::IntegrationStatus> {
        anyhow::bail!("not asked in these tests")
    }
}

use tokio::sync::mpsc;
use tracing::subscriber;
use tracing_subscriber::prelude::*;
use wilkes_core::embed::MockEmbedder;
use wilkes_core::generate::mock::MockGenerator;
use wilkes_core::types::EmbeddingEngine;
use wilkes_core::types::{
    BookmarkDock, ByteRange, CitationReference, EmbedderModel, IndexStatus, SearchMode,
    SelectedEmbedder, SemanticSettings, Settings, SourceOrigin, Theme,
};

mod bookmarks;
mod chunks;
mod citations;
mod embedding;
mod files;
mod generation;
mod indexing;
mod lifecycle;
mod literature;
mod managed;
mod metadata;
mod research;
mod restore;
mod search;
mod settings;
mod topics;
mod workers;

fn topic_chunk(file_id: i64, chunk_id: i64) -> TopicChunkData {
    TopicChunkData {
        chunk_id,
        file_id,
        file_path: PathBuf::from(format!("/library/{file_id}.txt")),
        chunk_text: format!("chunk {chunk_id}"),
        chunk_ref: Some(ChunkRef(format!("chunk-ref-{chunk_id}"))),
        text_sha256: Some(format!("sha-{chunk_id}")),
        extraction_byte_range: ByteRange {
            start: chunk_id as usize,
            end: chunk_id as usize + 1,
        },
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
        embedding: vec![chunk_id as f32, 1.0],
    }
}

/// An embedder whose two roles land in different places, which is the
/// whole reason the query role is worth an endpoint.
struct RoleEmbedder;

impl Embedder for RoleEmbedder {
    fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
        wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }

    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0, 1.0]).collect())
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn model_id(&self) -> &str {
        "role-test"
    }

    fn dimension(&self) -> usize {
        2
    }

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Fastembed
    }
}

#[cfg(unix)]
fn write_executable(path: &Path, content: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct MockEmitter {
    events: Arc<Mutex<Vec<(String, Value)>>>,
}
impl EventEmitter for MockEmitter {
    fn emit(&self, name: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((name.to_string(), payload));
    }
}

struct FakeInstaller {
    install_calls: Arc<AtomicUsize>,
    build_calls: Arc<AtomicUsize>,
    available: bool,
    install_should_fail: bool,
    build_should_fail: bool,
}

struct TopicEmbedder {
    calls: Arc<AtomicUsize>,
}

impl Embedder for TopicEmbedder {
    fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
        wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }

    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(texts
            .iter()
            .map(|text| {
                if text.contains("cat") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect())
    }

    fn model_id(&self) -> &str {
        "topic-test"
    }

    fn dimension(&self) -> usize {
        2
    }

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Fastembed
    }
}

#[async_trait::async_trait]
impl EmbedderInstaller for FakeInstaller {
    fn is_available(&self, _data_dir: &Path) -> bool {
        self.available
    }

    async fn install(
        &self,
        _data_dir: &Path,
        _tx: mpsc::Sender<EmbedProgress>,
    ) -> anyhow::Result<()> {
        self.install_calls.fetch_add(1, Ordering::Relaxed);
        if self.install_should_fail {
            Err(anyhow::anyhow!("install failed"))
        } else {
            Ok(())
        }
    }

    fn uninstall(&self, _data_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn build(&self, _data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>> {
        self.build_calls.fetch_add(1, Ordering::Relaxed);
        if self.build_should_fail {
            Err(anyhow::anyhow!("build failed"))
        } else {
            Ok(Arc::new(MockEmbedder::default()))
        }
    }
}

fn test_ctx() -> (tempfile::TempDir, Arc<AppContext>) {
    let dir = tempdir().unwrap();
    let settings_path = dir.path().join("settings.json");
    let emitter = Arc::new(MockEmitter {
        events: Arc::new(Mutex::new(Vec::new())),
    });
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: dir.path().to_path_buf(),
    };
    let (ctx, _rx, _loop) =
        AppContext::new(dir.path().to_path_buf(), settings_path, paths, emitter);
    (dir, ctx)
}

fn generation_ctx(
    dir: &Path,
    events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
) -> Arc<AppContext> {
    let emitter = Arc::new(MockEmitter { events });
    let (ctx, _rx, _loop) = AppContext::new(
        dir.to_path_buf(),
        dir.join("settings.json"),
        WorkerPaths::resolve(dir),
        emitter,
    );
    ctx
}

async fn enable_generation(ctx: &Arc<AppContext>, model: &str) {
    update_settings(
        &ctx.settings_path,
        serde_json::json!({
            "generation": {
                "enabled": true,
                "model": model,
                "device": "cpu",
                "sampling_overrides": {},
            }
        }),
    )
    .await
    .unwrap();
}

fn scoped_ctx(dir: &std::path::Path, kind: serde_json::Value) -> Arc<AppContext> {
    let workspace_path = dir.join("workspace.json");
    std::fs::write(
        &workspace_path,
        serde_json::json!({
            "version": 1,
            "id": "workspace-under-test",
            "name": "under test",
            "kind": kind,
        })
        .to_string(),
    )
    .unwrap();
    let paths = WorkerPaths {
        python_override: None,
        python_package_dir: PathBuf::from("pkg"),
        requirements_path: PathBuf::from("reqs.txt"),
        uv_bin: PathBuf::from("venv"),
        worker_bin: PathBuf::from("worker"),
        data_dir: dir.to_path_buf(),
    };
    let (ctx, _rx, _loop) = AppContext::new_scoped(
        dir.to_path_buf(),
        dir.to_path_buf(),
        dir.join("settings.json"),
        workspace_path,
        paths,
        Arc::new(MockEmitter {
            events: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    ctx
}

/// Record a finished job whose documents ended three different ways, as an
/// interrupted build would leave them.
fn journal_a_stopped_job(ctx: &Arc<AppContext>, root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let saved = root.join("saved.pdf");
    let broke = root.join("broke.pdf");
    let never = root.join("never.pdf");
    let journal = ctx.job_journal().unwrap();
    let mut guard = journal.lock().unwrap();
    let job = guard
        .begin(root, &[saved.clone(), broke.clone(), never.clone()])
        .unwrap();
    guard
        .note_outcome(job, &saved, DocumentOutcome::Indexed, None, Some(7))
        .unwrap();
    guard
        .note_outcome(
            job,
            &broke,
            DocumentOutcome::Failed,
            Some("mupdf: broken xref"),
            None,
        )
        .unwrap();
    guard.finish(job, JobState::Cancelled, None).unwrap();
    (saved, broke, never)
}

/// Build `root` into the context's own index with the mock model
/// (Candle, "mock-model", 384 dimensions).
async fn index_root_with_mock_model(ctx: &Arc<AppContext>, root: &Path) {
    let extensions = vec!["txt".to_string()];
    let (tx, _rx) = tokio::sync::mpsc::channel(10);
    crate::commands::embed::build_index_with_embedder(
        root.to_path_buf(),
        Arc::new(wilkes_core::embed::MockEmbedder::default()),
        crate::commands::embed::BuildIndexOptions {
            model_dir: ctx.data_dir.join("models"),
            index_dir: ctx.data_dir.clone(),
            reporter: BuildReporter::without_journal(tx),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            documents: crate::commands::embed::collect_indexable_paths(root, &extensions),
            scope: wilkes_core::embed::index::db::BuildScope::WholeRoot,
            chunk_size: 600,
            chunk_overlap: 128,
            supported_extensions: extensions,
        },
    )
    .await
    .unwrap();
}

fn running_embed_task() -> EmbedTaskHandle {
    EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel: CancellationToken::new(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            Ok(())
        })),
        settlement: Arc::new(BuildSettlement::new()),
    }
}

/// Spin until `ctx` holds a running build, which a spawned runner reaches
/// asynchronously.
async fn until_build_running(ctx: &Arc<AppContext>) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !ctx.embed_task_is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the build never took the slot");
}

/// Work that runs until its cancel flag is raised, as a projection's
/// catch-up does between documents.
async fn until_cancelled(cancel_flag: Arc<AtomicBool>) -> anyhow::Result<()> {
    while !cancel_flag.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    anyhow::bail!("stopped by the flag")
}

fn pretend_a_build_is_running(ctx: &AppContext) {
    *ctx.embed_task.lock() = Some(EmbedTaskHandle {
        operation: EmbedOperation::Build,
        cancel: CancellationToken::new(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        join: BuildHandle::new(tokio::spawn(async { Ok(()) })),
        settlement: Arc::new(BuildSettlement::new()),
    });
}

/// A one-page PDF that typesets `text` in Helvetica.
fn write_text_pdf(path: &Path, text: &str) {
    let stream = format!("BT\n/F1 12 Tf\n50 80 Td\n({text}) Tj\nET\n");
    let objects = [
        "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n".to_string(),
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
            stream.len(),
            stream
        ),
        "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
            .to_string(),
    ];
    let mut pdf = "%PDF-1.4\n".to_string();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(pdf.len());
        pdf.push_str(object);
    }
    let xref_offset = pdf.len();
    pdf.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        offsets.len() + 1
    ));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        objects.len() + 1
    ));
    std::fs::write(path, pdf).unwrap();
}

/// A text of `passages` passages of reading order's length, each phrase
/// appearing in the first so many of them.
fn reading_order_book(passages: usize, uses: &[(&str, usize)]) -> String {
    (0..passages)
        .map(|passage| {
            let said: String = uses
                .iter()
                .filter(|(_, count)| *count > passage)
                .map(|(phrase, _)| format!("{phrase}. "))
                .collect();
            format!(
                "{said:<width$}",
                width = wilkes_core::reading_order::PASSAGE_CHARS
            )
        })
        .collect()
}

/// Names a folder by an axis of its own, so a test can say which folder a
/// name embedding points at.
struct FolderNameEmbedder;

impl Embedder for FolderNameEmbedder {
    fn embedding_space_identity(&self) -> wilkes_core::embed::EmbeddingSpaceIdentity {
        wilkes_core::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }

    fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("tidy-up embeds folder names in the query role only")
    }

    fn embed_query(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| match *text {
                "Recommendation" => Ok(vec![1.0, 0.0, 0.0]),
                "Law" => Ok(vec![0.0, 1.0, 0.0]),
                "Inner" => Ok(vec![0.0, 0.0, 1.0]),
                other => anyhow::bail!("unexpected folder name {other}"),
            })
            .collect()
    }

    fn model_id(&self) -> &str {
        "folder-name-test"
    }

    fn dimension(&self) -> usize {
        3
    }

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Candle
    }
}

/// A build is an act the user performs. A search that discovered an
/// unindexed root used to begin one on the strength of that discovery,
/// which put hours of inference behind an MCP tool call — invisible, and
/// stoppable only by quitting the application.
/// Nothing announced a build. The wait is what makes the absence mean
/// something: a build would have emitted this within it.
async fn assert_no_reindex(events: &Arc<Mutex<Vec<(String, serde_json::Value)>>>) {
    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    let events_guard = events.lock().unwrap();
    assert!(
        !events_guard
            .iter()
            .any(|e| e.0 == "manager-event" && e.1 == serde_json::json!("Reindexing")),
        "a search started an index build: {events_guard:?}"
    );
}
