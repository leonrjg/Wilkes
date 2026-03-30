# Wilkes — MVP Specification

This document resolves ambiguities in the architecture plan and defines the contracts, types, and behavior needed to begin implementation. Read `architecture.md` first for context and motivation.

## Design Decisions (supplements architecture.md)

The following decisions were made during specification and have been backported into `architecture.md`.

1. **SearchProvider trait.** The provider takes `&SearchQuery` (which includes `root`, filters), an `&ExtractorRegistry`, and a `SearchResultTx` channel. It owns the directory walk so it can use `grep-searcher`'s built-in parallel walker for text files.

2. **Two-path search pipeline.** Text and binary/structured files take different paths:
   - **Text files**: `GrepSearchProvider` walks and searches directly. No extraction step. SourceMap is trivially line-based.
   - **Binary/structured files (PDF)**: Files are discovered during the walk, extracted via `ContentExtractor`, and searched in-memory. SourceMap carries page/bbox info.

3. **Type generation.** `ui/src/lib/types.ts` is auto-generated from Rust types using the `specta` crate (which integrates with Tauri v2 via `tauri-specta`).

4. **pdfium bundling.** `pdfium-render` requires a platform-specific `pdfium` shared library (~30 MB). It will be bundled via Tauri's resource mechanism and loaded at runtime. The Cargo build script will download the correct binary for the target platform from `nickel-nickel/nickel-pdfium-binaries` during `cargo build`.

---

## Complete Type Definitions

### Core Types (`crates/core/src/types.rs`)

```rust
use std::ops::Range;
use std::path::PathBuf;

// ── Query ────────────────────────────────────────────────────

/// A fully described search request.
pub struct SearchQuery {
    /// The text or regex pattern to find.
    pub pattern: String,
    /// Whether `pattern` is a regex (true) or literal (false).
    pub is_regex: bool,
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Root directory to search.
    pub root: PathBuf,
    /// Restrict to these extensions (e.g. ["pdf", "rs"]). Empty = all supported.
    pub file_type_filters: Vec<String>,
    /// Maximum number of results to return (0 = unlimited).
    pub max_results: usize,
}

// ── Results ──────────────────────────────────────────────────

/// A single matching location within a file.
pub struct Match {
    /// Byte range within the file's extracted/original text that matched.
    pub text_range: Range<usize>,
    /// The matched text itself.
    pub matched_text: String,
    /// Context: a few characters/lines around the match for display.
    pub context_before: String,
    pub context_after: String,
    /// Where this match lives in the original file.
    pub origin: SourceOrigin,
}

/// All matches in a single file.
pub struct FileMatches {
    pub path: PathBuf,
    pub file_type: FileType,
    pub matches: Vec<Match>,
}

/// Broad file classification used for UI routing.
pub enum FileType {
    PlainText,
    Pdf,
    // Future: Office, Image, etc.
}

// ── Source Mapping ───────────────────────────────────────────

pub struct SourceMap {
    pub segments: Vec<SourceSegment>,
}

pub struct SourceSegment {
    /// Byte range in the extracted text buffer.
    pub text_range: Range<usize>,
    /// Corresponding location in the original file.
    pub origin: SourceOrigin,
}

#[derive(Clone)]
pub enum SourceOrigin {
    TextFile { line: u32, col: u32 },
    PdfPage { page: u32, bbox: Option<BoundingBox> },
}

#[derive(Clone)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

// ── Extraction ───────────────────────────────────────────────

pub struct ExtractedContent {
    pub text: String,
    pub source_map: SourceMap,
    pub metadata: FileMetadata,
}

pub struct FileMetadata {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub mime: Option<String>,
    pub title: Option<String>,   // e.g. PDF title metadata
    pub page_count: Option<u32>, // PDF only
}

// ── Preview ──────────────────────────────────────────────────

/// Identifies a specific match for preview purposes.
pub struct MatchRef {
    pub path: PathBuf,
    pub origin: SourceOrigin,
}

/// Data the frontend needs to render a preview.
pub enum PreviewData {
    Text {
        content: String,
        language: Option<String>, // for syntax highlighting
        highlight_line: u32,
        highlight_range: Range<usize>, // character offset range within the line
    },
    Pdf {
        /// Raw PDF bytes (the frontend uses pdf.js to render).
        /// For large files, the backend may return only the relevant page range.
        data: Vec<u8>,
        page: u32,
        highlight_bbox: Option<BoundingBox>,
    },
}

// ── Settings ─────────────────────────────────────────────────

pub struct Settings {
    /// Directories the user has bookmarked for quick access.
    pub bookmarked_dirs: Vec<PathBuf>,
    /// Respect .gitignore files when walking directories.
    pub respect_gitignore: bool,
    /// Maximum file size to search (bytes). Files larger than this are skipped.
    pub max_file_size: u64,
    /// Number of context lines/chars around each match.
    pub context_lines: u32,
    /// UI theme.
    pub theme: Theme,
}

pub enum Theme {
    System,
    Light,
    Dark,
}

// ── Capabilities ─────────────────────────────────────────────

/// Advertises what a SearchProvider supports. Allows the UI to
/// show/hide controls that don't apply to the active provider.
pub struct SearchCapabilities {
    pub supports_regex: bool,
    pub supports_case_sensitivity: bool,
    pub is_indexed: bool,
    pub supported_file_types: Vec<String>,
}
```

### Frontend Types (`ui/src/lib/types.ts`)

Auto-generated by `tauri-specta`. The generated file will be committed to the repo so the UI can be developed without running the Rust build. CI will verify the generated file is up to date.

---

## Trait Contracts

### SearchProvider

```rust
use tokio::sync::mpsc;

/// A channel-based result stream. The provider sends FileMatches
/// as they are found; the consumer (api layer) forwards them to the frontend.
pub type SearchResultTx = mpsc::Sender<FileMatches>;

pub trait SearchProvider: Send + Sync {
    /// Begin searching. Results are sent to `tx` as they are discovered.
    /// Returns when the search is complete or an error occurs.
    /// The caller may drop `rx` to signal cancellation; the provider
    /// must check `tx.is_closed()` and stop promptly.
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
    ) -> Result<()>;

    fn capabilities(&self) -> SearchCapabilities;
}
```

The provider owns the directory walk so it can:
- Use `ignore` crate's parallel walker for text files (fast path).
- Delegate to `ExtractorRegistry` for non-text files (PDF path).
- Respect `query.file_type_filters` and `Settings.respect_gitignore`.

### ContentExtractor

```rust
pub trait ContentExtractor: Send + Sync {
    /// Returns true if this extractor handles the given file.
    /// Called with the path and an optional MIME type (from `infer` crate).
    fn can_handle(&self, path: &Path, mime: Option<&str>) -> bool;

    /// Extract searchable text and a source map from the file.
    fn extract(&self, path: &Path) -> Result<ExtractedContent>;
}
```

### ExtractorRegistry

```rust
pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn ContentExtractor>>,
}

impl ExtractorRegistry {
    pub fn register(&mut self, extractor: Box<dyn ContentExtractor>);

    /// Returns the first extractor that can handle the file, or None.
    pub fn find(&self, path: &Path, mime: Option<&str>) -> Option<&dyn ContentExtractor>;
}
```

Priority order: extractors are tried in registration order. Register more specific extractors first (e.g. PDF before a future catch-all).

---

## Search Pipeline (Revised)

```
User types query
      │
      ▼
SearchBar ──→ api.search(query)
      │
      ▼
[Transport: Tauri IPC]
      │
      ▼
api::commands::search(query)
      │
      ├── 1. Create mpsc channel (tx, rx)
      ├── 2. Spawn blocking task:
      │       GrepSearchProvider::search(query, extractors, tx)
      │         │
      │         ├── Walk directory (ignore crate, respects .gitignore)
      │         ├── For each file:
      │         │     ├── [text file] Search directly with grep-searcher
      │         │     │    └── Build Match with SourceOrigin::TextFile
      │         │     ├── [pdf file]  ExtractorRegistry::find() → PdfExtractor
      │         │     │    ├── extract() → ExtractedContent + SourceMap
      │         │     │    ├── Search extracted text in-memory
      │         │     │    └── Map offsets → SourceOrigin::PdfPage via SourceMap
      │         │     └── [unknown]  Skip
      │         └── Send FileMatches to tx as they're found
      │
      └── 3. Forward rx items to frontend as Tauri events (streaming)
      │
      ▼
ResultList renders incrementally (virtualized)
      │
      ▼
User clicks a match
      │
      ▼
api.preview(MatchRef)
      │
      ├── [TextFile] Read file, detect language, return PreviewData::Text
      └── [PdfPage]  Read PDF bytes, return PreviewData::Pdf
      │
      ▼
PreviewPane renders
      ├── Text: CodeMirror, scroll to line, highlight range
      └── PDF:  pdf.js, render page, overlay highlight rect
```

### File type detection

The search provider determines file type using a two-step check:
1. Extension-based: `.pdf` → PDF, known text extensions (`.txt`, `.md`, `.rs`, `.py`, `.js`, `.ts`, `.json`, `.toml`, `.yaml`, `.xml`, `.html`, `.css`, `.c`, `.cpp`, `.h`, `.java`, `.go`, `.rb`, `.sh`, etc.) → PlainText.
2. For unknown extensions: use the `infer` crate to sniff magic bytes. If it's a recognized binary format with an extractor, route there. Otherwise skip.

### Cancellation

When the user modifies the query while a search is in progress:
1. The frontend drops the previous `AsyncIterable` / event listener.
2. The API layer drops the `rx` end of the channel.
3. The search provider detects `tx.is_closed()` and stops walking.
4. A new search begins.

Debounce: the frontend debounces keystrokes by 200ms before issuing a new search.

---

## API Commands

These are the Tauri commands exposed via `#[tauri::command]` in `crates/desktop/` and implemented in `crates/api/`.

### `search`

```
Input:  SearchQuery
Output: Stream of FileMatches (via Tauri event channel)
```

- Spawns a background task with the active `SearchProvider`.
- Streams results as Tauri events named `search-result`.
- Sends a final `search-complete` event with stats (total files scanned, total matches, elapsed time).
- Returns a `search_id: String` that can be used to cancel.

### `cancel_search`

```
Input:  { search_id: String }
Output: ()
```

Drops the channel, causing the provider to stop.

### `preview`

```
Input:  MatchRef
Output: PreviewData
```

Synchronous (non-streaming). Reads the file and returns the data needed to render the preview.

### `get_settings` / `update_settings`

```
Input:  () / Partial<Settings>
Output: Settings / Settings
```

Settings are stored as JSON in the platform-appropriate config directory (`dirs::config_dir()/wilkes/settings.json`). `update_settings` merges the patch and returns the full new settings.

### `pick_directory`

```
Input:  ()
Output: Option<PathBuf>
```

Opens the native directory picker dialog (Tauri's `dialog` plugin) and returns the selected path.

---

## Frontend Components

### `SearchBar`
- Text input for the query.
- Toggle buttons: case sensitivity, regex mode.
- Directory selector (shows current path, click to pick).
- File type filter chips.
- Debounces input by 200ms.

### `ResultList`
- Receives streaming `FileMatches` and renders incrementally.
- Grouped by file: file path header, then individual matches beneath.
- Each match row shows: line/page number, snippet with highlighted match text.
- Virtualized with `@tanstack/react-virtual` for performance with large result sets.
- Clicking a match triggers preview.

### `PreviewPane`
- Split-pane layout (result list left, preview right).
- **Text files**: CodeMirror 6 in read-only mode. Scrolls to the matched line. Highlights the match range using CodeMirror's `Decoration` API.
- **PDF files**: `react-pdf` (pdf.js wrapper). Scrolls to the matched page. Draws a semi-transparent highlight rectangle over the bounding box using an absolutely positioned overlay div.
- Shows file metadata header: file name, size, type.

### `DirectoryPicker`
- Calls `pick_directory` command.
- Shows bookmarked directories from settings for quick access.

---

## Crate Dependencies (MVP)

### `crates/core`
```toml
[dependencies]
grep-regex = "0.1"       # regex engine for grep-searcher
grep-searcher = "0.1"    # ripgrep's search library
ignore = "0.4"           # directory walker (respects .gitignore)
pdfium-render = "0.8"    # PDF text extraction with positions
infer = "0.16"           # file type detection by magic bytes
thiserror = "2"          # error types
tokio = { version = "1", features = ["sync"] }  # mpsc channel only
```

### `crates/api`
```toml
[dependencies]
wilkes-core = { path = "../core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt", "fs"] }
dirs = "5"               # platform config directory
specta = "2"             # TypeScript type generation
```

### `crates/desktop`
```toml
[dependencies]
wilkes-api = { path = "../api" }
tauri = { version = "2", features = ["protocol-asset"] }
tauri-specta = "2"       # auto-generates TS bindings
tauri-plugin-dialog = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

### `ui/`
```json
{
  "dependencies": {
    "react": "^19",
    "react-dom": "^19",
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-dialog": "^2",
    "react-pdf": "^9",
    "@tanstack/react-virtual": "^3",
    "codemirror": "^6",
    "@codemirror/view": "^6",
    "@codemirror/state": "^6",
    "@codemirror/language": "^6"
  },
  "devDependencies": {
    "typescript": "^5",
    "vite": "^6",
    "@vitejs/plugin-react": "^4",
    "tailwindcss": "^4",
    "@tailwindcss/vite": "^4"
  }
}
```

---

## Implementation Order

Each phase produces a shippable, testable increment.

### Phase 1 — Skeleton & Text Search
1. Initialize Cargo workspace, Tauri project, Vite/React UI.
2. Define all types in `crates/core/src/types.rs`.
3. Implement `ExtractorRegistry` and `ContentExtractor` trait.
4. Implement `GrepSearchProvider` — text-file direct search using `grep-searcher` + `ignore` walker (no extraction step for text; `SourceOrigin::TextFile` is built directly from grep results).
5. Implement `api::commands::search` with mpsc streaming over Tauri events.
6. Build `SearchBar`, `ResultList` (no virtualization yet), wire up to Tauri commands.
7. **Milestone**: can search a directory for text in `.txt`/`.md`/code files and see results.

### Phase 2 — Preview Pane
1. Implement `api::commands::preview` for text files.
2. Build `PreviewPane` with CodeMirror integration (read-only, highlight, scroll-to-line).
3. Add split-pane layout.
4. **Milestone**: click a text result, see the file with the match highlighted.

### Phase 3 — PDF Support
1. Implement `PdfExtractor` using `pdfium-render`: extract text per page, build `SourceMap` with page numbers and bounding boxes.
2. Set up pdfium binary download in the build script.
3. Register `PdfExtractor` in `ExtractorRegistry`. Update `GrepSearchProvider` to route PDF files through extraction.
4. Implement `api::commands::preview` for PDF (return bytes + page + bbox).
5. Add pdf.js rendering to `PreviewPane` with highlight overlay.
6. **Milestone**: search finds matches in PDFs, preview shows the page with highlight.

### Phase 4 — Polish
1. Add `@tanstack/react-virtual` to `ResultList`.
2. Implement search cancellation and debouncing.
3. Implement settings (persist, load, directory bookmarks).
4. Add `DirectoryPicker` with bookmarks.
5. File type filter UI.
6. Handle edge cases: empty results, large files, binary file skip, permission errors.
7. **Milestone**: feature-complete MVP.

---

## Testing Strategy

### Rust unit tests
- **SourceMap resolution**: given a byte offset and a SourceMap, assert correct SourceOrigin.
- **PdfExtractor SourceMap**: given a multi-page PDF, assert the SourceMap has one segment per page with correct byte ranges and page numbers.
- **PdfExtractor**: given a known test PDF, assert extracted text contains expected strings and page numbers are correct.
- **GrepSearchProvider**: given a temp directory with known files, assert correct matches are found with correct origins.

### Rust integration tests
- **Full search pipeline**: create a temp directory with `.txt` and `.pdf` files, run a search through the API layer, assert results stream correctly.

### Frontend tests
- **Component tests** (Vitest + Testing Library): SearchBar emits debounced queries, ResultList renders streamed results, PreviewPane switches between text and PDF modes.

### Manual verification
As described in `architecture.md` § Verification.
