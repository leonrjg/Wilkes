use super::*;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use tempfile::tempdir;

mod build;
mod chunks;
mod files;
mod managed;
mod query;
mod roots;
mod schema;
mod status;
mod topics;

struct CountingEmbedder {
    calls: AtomicUsize,
}

impl CountingEmbedder {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Embedder for CountingEmbedder {
    fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
        crate::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }

    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(texts.len(), Ordering::Relaxed);
        Ok(vec![vec![1.0]; texts.len()])
    }

    fn model_id(&self) -> &str {
        "counting"
    }

    fn dimension(&self) -> usize {
        1
    }

    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Candle
    }
}

fn txt_indexing() -> IndexingConfig {
    IndexingConfig {
        chunk_size: 100,
        chunk_overlap: 0,
        supported_extensions: vec!["txt".to_string()],
    }
}

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn test_chunk(path: &Path, text: &str) -> Chunk {
    Chunk {
        file_path: path.to_path_buf(),
        text: text.to_string(),
        byte_range: ByteRange {
            start: 0,
            end: text.len(),
        },
        origin: SourceOrigin::TextFile { line: 1, col: 1 },
    }
}

fn formula_region(start: usize, end: usize) -> ReadingRegion {
    ReadingRegion {
        area_id: "p1-i0".to_string(),
        page: 1,
        bbox: BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 24.0,
        },
        text_range: ByteRange { start, end },
    }
}

/// A recorder that stands in for the two expensive halves of a build, so
/// the order they run in is observable without either model.
#[derive(Clone, Default)]
struct BuildLog(Arc<std::sync::Mutex<Vec<String>>>);

impl BuildLog {
    fn record(&self, event: impl Into<String>) {
        self.0.lock().unwrap().push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// An extractor that reads nothing and remembers being asked. A non-empty
/// `analyzer_identity` is what makes the build treat its documents as
/// carrying figures — the same declaration a real analyzer-bearing
/// extractor makes.
struct RecordingExtractor {
    log: BuildLog,
    analyzer_identity: String,
}

impl crate::extract::ContentExtractor for RecordingExtractor {
    fn can_handle(&self, path: &std::path::Path, _mime: Option<&str>) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("doc")
    }

    fn image_analyzer_identity(&self) -> &str {
        &self.analyzer_identity
    }

    fn release_image_analyzer(&self) {
        self.log.record("release");
    }

    fn extract(&self, path: &std::path::Path) -> anyhow::Result<crate::types::ExtractedContent> {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        self.log.record(format!("read:{name}"));
        Ok(crate::types::ExtractedContent {
            text: format!("the text of {name}"),
            source_map: crate::types::SourceMap {
                segments: Vec::new(),
            },
            metadata: crate::types::FileMetadata {
                path: path.to_path_buf(),
                size_bytes: 0,
                mime: None,
                title: None,
                page_count: None,
            },
            images: Vec::new(),
        })
    }

    fn outline(&self, _path: &std::path::Path) -> anyhow::Result<crate::types::DeclaredOutline> {
        Ok(crate::types::DeclaredOutline::default())
    }
}

struct RecordingEmbedder(BuildLog);

impl Embedder for RecordingEmbedder {
    fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
        crate::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.0.record("embed");
        Ok(vec![vec![1.0]; texts.len()])
    }
    fn model_id(&self) -> &str {
        "recording"
    }
    fn dimension(&self) -> usize {
        1
    }
    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Candle
    }
}

/// Build `paths` with a registry whose extractor records every read, and
/// return what happened in order.
fn build_recording(analyzer_identity: &str) -> Vec<String> {
    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let paths: Vec<PathBuf> = ["a", "b"]
        .iter()
        .map(|name| {
            let path = root.join(format!("{name}.doc"));
            fs::write(&path, "source bytes").unwrap();
            path
        })
        .collect();
    let data_dir = dir.path().join("data");
    fs::create_dir(&data_dir).unwrap();

    let log = BuildLog::default();
    let mut registry = ExtractorRegistry::new();
    registry.register(Box::new(RecordingExtractor {
        log: log.clone(),
        analyzer_identity: analyzer_identity.to_string(),
    }));

    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    SemanticIndex::build(
        &data_dir,
        &root,
        &paths,
        &registry,
        &RecordingEmbedder(log.clone()),
        BuildOptions::new(
            BuildReporter::without_journal(tx),
            Arc::new(AtomicBool::new(false)),
        ),
        &IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: vec!["doc".to_string()],
        },
    )
    .unwrap();
    log.events()
}

/// An embedder that raises a cancel flag once it has embedded `after`
/// batches, so a build can be stopped mid-corpus at a known point.
struct CancelAfter {
    cancel: Arc<AtomicBool>,
    after: usize,
    calls: AtomicUsize,
}

impl Embedder for CancelAfter {
    fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
        crate::embed::EmbeddingSpaceIdentity::for_test(
            self.engine(),
            self.model_id(),
            self.dimension(),
        )
    }
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.after {
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(texts.iter().map(|_| vec![1.0]).collect())
    }
    // One file per batch, so "stopped after the first document" is a thing
    // this test can actually arrange.
    fn preferred_batch_size(&self) -> Option<usize> {
        Some(1)
    }
    fn model_id(&self) -> &str {
        "counting"
    }
    fn dimension(&self) -> usize {
        1
    }
    fn engine(&self) -> EmbeddingEngine {
        EmbeddingEngine::Candle
    }
}
