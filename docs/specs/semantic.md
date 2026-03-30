# Semantic Search Architecture

## Overview

Semantic search allows natural-language queries against an offline vector index. It is
opt-in: the user selects a model, downloads it, and triggers an index build. All inference
runs locally; no data leaves the machine.

The `Embedder` trait decouples the embedding backend from the indexing and search pipeline,
so fastembed-rs, llama.cpp, or any future local backend can be swapped in without touching
downstream code. All inference is synchronous and local; remote API backends are out of scope.

---

## New components

### `wilkes-core::embed`

#### `embed/mod.rs` — trait

```rust
pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    fn model_id(&self) -> &str;
    fn dimension(&self) -> usize;
}
```

Only one embedder is live at a time because each model occupies significant memory.
The active embedder is stored as `Mutex<Option<Arc<dyn Embedder>>>` in app state
(see `ActiveEmbedderState` in `wilkes-desktop`).

`SemanticSearchProvider` takes `Arc<dyn Embedder>` in its constructor — the caller
(i.e. `start_search`) clones the `Arc` from `ActiveEmbedderState` before constructing
the provider.

#### `embed/fastembed.rs` — first `Embedder` implementation

Wraps [fastembed-rs](https://github.com/Anush008/fastembed-rs), which provides ONNX
Runtime-based inference and built-in model download from Hugging Face Hub. The model
variant is a constructor argument; all fastembed-supported models share identical
scaffolding.

Supported models (constructor variants of `EmbedderModel`):

| Variant | Model | Size | Notes |
|---|---|---|---|
| `MiniLML6V2` | all-MiniLM-L6-v2 | ~90 MB | Fast, lower accuracy |
| `BgeBaseEn` | bge-base-en-v1.5 | ~430 MB | Good default |
| `BgeLargeEn` | bge-large-en-v1.5 | ~1.3 GB | Best English accuracy |
| `MultilingualE5Large` | multilingual-e5-large | ~2.3 GB | Non-English documents |

#### `embed/chunk.rs` — document chunker

Splits `ExtractedContent` (from the existing extraction pipeline) into `Chunk` records:

```rust
pub struct Chunk {
    pub text: String,
    pub byte_range: ByteRange,    // into ExtractedContent.text
    pub origin: SourceOrigin,     // resolved via source_map.resolve()
    pub file_path: PathBuf,
}
```

Strategy: fixed token window (256 tokens) with overlap, respecting sentence boundaries
where possible. Reuses extraction entirely — no new I/O.

#### `embed/index.rs` — persistent ANN store

Backed by `sqlite-vec`. Interface:

```rust
pub struct SemanticIndex { /* sqlite connection + vec vtable */ }

impl SemanticIndex {
    /// Open an existing index. Returns `Err` if no index exists at `data_dir` or
    /// if `model_id`/`dimension` in the stored metadata mismatches `embedder`.
    pub fn open(data_dir: &Path, embedder: &dyn Embedder) -> anyhow::Result<Self>;

    /// Full build: creates the database at `data_dir`, indexes every path, and
    /// returns the open index. Internally calls `index_file` for each path.
    pub fn build(
        data_dir: &Path,
        paths: &[PathBuf],
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        tx: ProgressTx,
    ) -> anyhow::Result<Self>;

    /// Extract, chunk, and embed a file without holding the index lock.
    /// Pass the result to `write_file` under the lock.
    pub fn prepare_file(
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
    ) -> anyhow::Result<PreparedFile>;

    /// Write previously prepared chunks into the index, removing any existing
    /// chunks for that path first. This is the only step that requires `&mut self`.
    /// The watcher calls `prepare_file` + `write_file` separately so extraction
    /// and embedding happen outside the lock.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()>;

    /// Convenience wrapper: `prepare_file` then `write_file`. Used by `build`.
    /// Do not call from the watcher — extraction would block queries for its duration.
    pub fn index_file(
        &mut self,
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
    ) -> anyhow::Result<()>;

    /// Remove all chunks for the given path. No-op if the file was not indexed.
    pub fn remove_file(&mut self, path: &Path) -> anyhow::Result<()>;

    pub fn query(
        &self,
        embedding: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<IndexedChunk>>;

    /// Reports index metadata. Does not re-validate model_id/dimension —
    /// validation is the responsibility of `open`.
    pub fn status(&self) -> IndexStatus;
    pub fn delete(self) -> anyhow::Result<()>;
}

pub struct PreparedFile {
    pub path: PathBuf,
    /// Pairs of (chunk metadata, embedding vector), ready to write.
    pub chunks: Vec<(Chunk, Vec<f32>)>,
}

pub struct IndexedChunk {
    pub file_path: PathBuf,
    pub chunk_text: String,
    /// Byte range into `ExtractedContent.text` (not into the original file).
    /// Mapped to `Match.text_range` only for plain-text chunks; None for PDF chunks.
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
    pub score: f32,
}

pub struct IndexStatus {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub built_at: Option<u64>,  // unix timestamp
    /// Model that built this index. Validated on open; mismatch returns an error.
    pub model_id: String,
    /// Embedding dimension stored in the index. Validated against the live
    /// embedder's `dimension()` on open to catch model switches.
    pub dimension: usize,
}
```

SQLite schema: `chunks(id, file_path, chunk_idx, byte_start, byte_end, origin_json)` with
a `sqlite-vec` virtual table on the embedding column. A separate `meta` table stores
`model_id` and `dimension` (written at build time, read and validated at open time —
`SemanticIndex::open()` returns `Err` if either field mismatches the provided embedder).

#### `embed/installer.rs` — model lifecycle trait and progress types

Each embedding backend implements `EmbedderInstaller`, which owns the download and
construction steps for that backend. This keeps `Embedder` (inference only) decoupled
from model management.

```rust
pub trait EmbedderInstaller: Send + Sync {
    fn is_available(&self, data_dir: &Path) -> bool;
    async fn install(&self, data_dir: &Path, tx: ProgressTx) -> anyhow::Result<()>;
    fn uninstall(&self, data_dir: &Path) -> anyhow::Result<()>;
    /// Construct the live embedder. Called after `install` succeeds.
    fn build(&self, data_dir: &Path) -> anyhow::Result<Arc<dyn Embedder>>;
}
```

`FastembedInstaller` delegates to fastembed-rs's internal download and cache.
A future `OnnxInstaller` (llama.cpp, candle, etc.) would use `LocalModelManager`
(see below) to manage model files at `data_dir`.

Progress types, shared across download and build phases:

```rust
pub enum EmbedProgress {
    Download(DownloadProgress),
    Build(IndexBuildProgress),
}

pub type ProgressTx = mpsc::Sender<EmbedProgress>;

pub struct DownloadProgress {
    pub bytes_received: u64,
    pub total_bytes: u64,
    pub done: bool,
}

pub struct IndexBuildProgress {
    pub files_processed: usize,
    pub total_files: usize,
    pub done: bool,
}
```

#### `embed/downloader.rs` — raw ONNX file manager (non-fastembed backends)

`LocalModelManager` is an implementation detail used by non-fastembed `EmbedderInstaller`
implementations. Not called directly from `wilkes-api` or `wilkes-desktop`.

```rust
pub struct LocalModelManager;

impl LocalModelManager {
    pub async fn download(
        url: &str,
        dest: &Path,
        tx: ProgressTx,
    ) -> anyhow::Result<()>;

    pub fn is_downloaded(dest: &Path) -> bool;
    pub fn delete(dest: &Path) -> anyhow::Result<()>;
}
```

Download streams from the given URL and verifies the checksum.

---

### `wilkes-core::search::semantic`

`SemanticSearchProvider` implements the existing `SearchProvider` trait:

```rust
pub struct SemanticSearchProvider {
    embedder: Arc<dyn Embedder>,
    index: Arc<Mutex<Option<SemanticIndex>>>,
}
```

1. Embeds `query.pattern` using `self.embedder`
2. Calls `SemanticIndex::query()`
3. Converts `IndexedChunk` results into `FileMatches` / `Match`:
   - `matched_text` = `chunk_text`
   - `origin` = already correct
   - `text_range` = `None` for PDF chunks; `Some(extraction_byte_range)` for plain-text
     chunks. Highlight positioning for PDFs routes through `origin.bbox`, not
     `text_range`, consistent with the grep path.
4. Respects `query.max_results`
5. Reports `is_indexed: true`, `requires_index: true`, `semantic_index_built: true`
   in `capabilities()`
6. The `extractors` parameter received from the `SearchProvider` trait is unused at
   query time (extraction happens during index build). Accept it and ignore it.

---

### `wilkes-core::embed::watcher` — incremental index updates

Watches a directory for filesystem changes and updates the index incrementally.
Applies to **both desktop and server/web**:

- **Desktop**: watches the user-selected search root.
- **Server/web**: watches the directory where uploaded files land. The trigger is the
  same OS-level inotify/FSEvents/kqueue event — the distinction is only in which
  directory is watched and who initiates the watch.

```rust
pub struct IndexWatcher {
    // Internal: notify Watcher handle + debounce state
}

impl IndexWatcher {
    /// Start watching `root`. Events are processed on a background thread.
    /// `index` must be the open SemanticIndex for that root.
    pub fn start(
        root: PathBuf,
        index: Arc<Mutex<Option<SemanticIndex>>>,
        extractors: Arc<ExtractorRegistry>,
        embedder: Arc<dyn Embedder>,
    ) -> anyhow::Result<Self>;

    /// Stop the watcher. Subsequent calls are no-ops.
    pub fn stop(&mut self);
}
```

**Event dispatch:**

| Filesystem event | Action |
|---|---|
| Created | `prepare_file(path)` + `write_file(prepared)` |
| Modified | `prepare_file(path)` + `write_file(prepared)` (removes existing chunks first — idempotent) |
| Removed | `remove_file(path)` |
| Renamed(old, new) | `remove_file(old)` + `prepare_file(new)` + `write_file(prepared)` |

**Debouncing:** Events are debounced with a 500 ms quiet-period before acting.
Rapid writes (e.g. a file being streamed in) produce many events; only the final
settled state triggers indexing.

**Partially-written files:** After the quiet-period, attempt to open the file
exclusively. If it fails (write still in progress), retry with exponential backoff
up to 5 s before logging a non-fatal error and skipping. This is the primary
platform-specific complexity; behaviour is consistent across desktop and server.

**Concurrency:** `SemanticIndex` is wrapped in `Arc<Mutex<_>>` so the watcher
thread and query path share the same instance safely. Queries are not blocked for
long — `index_file` holds the lock only during the SQLite write, not during
extraction or embedding.

---

## Changes to existing components

### `types.rs`

```rust
// New
pub enum EmbedderModel { MiniLML6V2, BgeBaseEn, BgeLargeEn, MultilingualE5Large }

pub struct SemanticSettings {
    pub enabled: bool,
    pub model: EmbedderModel,
    pub index_path: Option<PathBuf>,
}

// Match: text_range is now Option<ByteRange> (breaking change).
// Some(range) for plain text files — byte range is file-relative and used for highlight.
// None for PDF — highlight position is carried by origin.bbox; text_range is meaningless.
//
// Refactor sites:
//   - GrepSearchProvider: wrap text_range in Some() when constructing Match
//   - PreviewPane.tsx: guard on text_range presence before computing highlight offsets
//   - api.ts / tauri.ts: update Match type definition
pub text_range: Option<ByteRange>,

// SearchQuery: add field
pub mode: SearchMode,   // #[serde(default)] → SearchMode::Grep

// SearchMode: new
pub enum SearchMode { Grep, Semantic }

// SearchCapabilities: add fields
// Both default to false so existing serialized responses from GrepSearchProvider
// remain valid without schema changes.
#[serde(default)]
pub requires_index: bool,
#[serde(default)]
pub semantic_index_built: bool,

// Settings: add field
pub semantic: SemanticSettings,  // #[serde(default)]
```

### `wilkes-api/src/commands/search.rs`

`start_search` signature becomes:

```rust
pub fn start_search(query: SearchQuery, data_dir: Option<PathBuf>) -> SearchHandle
```

`data_dir` is `None` for grep (ignored) and `Some(app_data_path)` for semantic.
The desktop's `search` Tauri command passes `app.path().app_data_dir()`.

Dispatch:

1. Read `query.mode`
2. For `SearchMode::Semantic`: lock `ActiveEmbedderState` and clone the `Arc`. If
   `None`, return an error immediately — no embedder is loaded, so a semantic search
   cannot proceed. Otherwise pass the cloned `Arc<dyn Embedder>` and the
   `Arc<Mutex<Option<SemanticIndex>>>` from `SemanticIndexState` to
   `SemanticSearchProvider`. The provider locks at query time and returns `Err` if
   the `Option` is `None` (index not built).
3. For `SearchMode::Grep`: unchanged (registry lookup and index open are skipped)

`SearchHandle` and the streaming contract are unchanged.

### `wilkes-api/src/commands/embed.rs` (new)

Four async functions, not yet Tauri commands (wired in desktop):

```rust
pub async fn download_model(installer: &dyn EmbedderInstaller, data_dir: PathBuf, tx: ProgressTx);
pub async fn build_index(
    root: PathBuf,
    installer: &dyn EmbedderInstaller,
    data_dir: PathBuf,
    cancel: CancellationToken,
    tx: ProgressTx,
);
pub async fn get_index_status(data_dir: &Path) -> anyhow::Result<IndexStatus>;
pub async fn delete_index(data_dir: &Path) -> anyhow::Result<()>;
```

`build_index` spawns the entire build on a dedicated blocking thread
(`spawn_blocking` or `std::thread::spawn`). Embedding and extraction are
CPU-bound and synchronous — there is nothing to interleave, so a single
blocking thread is the correct granularity. The loop checks `cancel` before
each `index_file` call; if cancelled, it returns early with a partial index.
Progress is reported through `tx`. The `async` signature simply awaits the
thread's `JoinHandle`.

### `wilkes-desktop/src/lib.rs`

New app state:

```rust
/// Tracks the active download or index build so it can be cancelled.
struct EmbedState(Mutex<Option<EmbedTaskHandle>>);
/// The loaded embedder, shared with SemanticSearchProvider via Arc.
/// Only one embedder is live at a time; each model occupies significant memory.
struct ActiveEmbedderState(Mutex<Option<Arc<dyn Embedder>>>);
/// The open index, shared with the watcher and query path.
/// `None` when no index has been built yet.
struct SemanticIndexState(Arc<Mutex<Option<SemanticIndex>>>);
/// The active filesystem watcher. Stopped and replaced when the root changes.
struct WatcherState(Mutex<Option<IndexWatcher>>);

pub struct EmbedTaskHandle {
    cancel: CancellationToken,
    join: JoinHandle<anyhow::Result<()>>,
}

// The Tauri command creates a CancellationToken, passes a clone to
// build_index, and stores the other in EmbedState. Calling cancel()
// on the stored token causes the build loop to exit before the next file.
```

All four registered via `app.manage()` in `tauri::Builder`.

The watcher is started after a successful `build_index` or `open` and stopped when
the root directory changes or the index is deleted.

`build_index` must stop the active watcher before starting the build and restart it
afterward. A watcher event firing against a half-written index during rebuild would
corrupt it.

New Tauri commands (all emit progress events):

| Command | Events emitted |
|---|---|
| `download_model(model)` | `embed-progress` (EmbedProgress::Download), `embed-done`, `embed-error` |
| `build_index(root, model)` | `embed-progress` (EmbedProgress::Build), `embed-done`, `embed-error` |
| `get_index_status()` | — (returns IndexStatus directly) |
| `delete_index()` | — |

Event payloads:

```rust
// embed-done: signals completion of download_model or build_index.
pub struct EmbedDone {
    pub operation: EmbedOperation,  // Download | Build
}

// embed-error: signals a fatal failure.
pub struct EmbedError {
    pub operation: EmbedOperation,
    pub message: String,
}

pub enum EmbedOperation { Download, Build }
```

All four registered in `invoke_handler`.

---

## Crate dependency changes

**`wilkes-core/Cargo.toml`**
- `fastembed` — feature-gated behind `features = ["fastembed"]`; brings in its own HF Hub download internally
- `rusqlite` + `sqlite-vec`
- `notify` + `notify-debouncer-mini` — filesystem watcher, used by `IndexWatcher`
- `hf-hub` — only needed if/when a non-fastembed installer is added; omit for now

**`wilkes-desktop/Cargo.toml`**
- Enable `wilkes-core/fastembed`

The `fastembed` feature gate ensures the server binary and any future non-desktop target do
not pull in the ONNX Runtime.

---

## Open issues (excluded from initial implementation)

**Single index, single root**

The current design supports exactly one index stored at `data_dir`. `SearchQuery.root`
is per-query, so if the user builds an index for directory A and runs a semantic search
with root B, the index silently returns results from A. At minimum, the index should
store its root and `start_search` should reject a semantic query whose root doesn't
match. Multi-root or per-root indexes are not yet designed.

**Debouncer selection**

The watcher uses `notify-debouncer-mini`. For the upload case — where a file arrives
in chunks and triggers multiple events — `notify-debouncer-full` may be more correct,
as it coalesces related events (e.g. `Create` + `Modify` → `Create`). The tradeoff
between the two needs evaluation against real upload behaviour before committing.

---

## What does not change

- `ContentExtractor` / `ExtractorRegistry` — semantic indexing reuses them as-is
- `FileMatches` / `SourceOrigin` — semantic results fit these types without modification
- `Match` — `text_range` changes to `Option<ByteRange>` (see `types.rs` above); all other fields unchanged
- `SearchHandle` and the Tauri streaming event protocol (`search-result-{id}`, `search-complete-{id}`)
- All existing Tauri commands
