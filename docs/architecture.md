# Wilkes — Architecture Plan

## Context

Existing file search tools all have platform or feature gaps: Recoll is clunky on macOS with no native PDF support, Clapgrep is Linux-only (GTK4/Adwaita), DocFetcher lacks highlighting, Baloo is KDE-only, and most others are terminal-based. The goal is a desktop-first multiplatform file search GUI with first-party PDF support, full match lists with navigation, and highlighting — designed so it can later become a self-hosted web app and support semantic search.

## Architecture Overview

```
┌──────────────────────────────────────────┐
│              UI (Web Tech)               │
│       React + PDF.js + CodeMirror        │
├──────────────────────────────────────────┤
│           Transport Adapter              │
│      Tauri IPC  ←→  HTTP/WebSocket       │
├──────────────────────────────────────────┤
│         API Layer (Commands)             │
│    Search · Preview · Settings · Index   │
├──────────────────────────────────────────┤
│         Core Engine (Rust)               │
│   SearchProvider · ContentExtractor      │
│   SourceMap · ResultStream               │
├──────────────────────────────────────────┤
│            Storage / I/O                 │
│   Filesystem · Tantivy · Vector DB       │
└──────────────────────────────────────────┘
```

Every boundary is a trait or interface. Concrete implementations are swappable.

## Project Structure

```
Wilkes/                         # workspace root
├── Cargo.toml                  # Rust workspace definition
├── justfile                    # dev commands
│
├── crates/
│   ├── core/                   # Pure library — no I/O opinions
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types.rs        # SearchQuery, SearchResult, Match, SourceMap
│   │       ├── search/
│   │       │   ├── mod.rs      # SearchProvider trait
│   │       │   ├── grep.rs     # Live ripgrep-style search
│   │       │   ├── indexed.rs  # Tantivy full-text (future)
│   │       │   └── semantic.rs # Embedding-based (future)
│   │       └── extract/
│   │           ├── mod.rs      # ContentExtractor trait + registry
│   │           ├── pdf.rs      # PDF extraction with position info
│   │           └── office.rs   # OOXML / ODF (future)
│   │
│   ├── api/                    # Transport-agnostic command handlers
│   │   └── src/
│   │       ├── lib.rs
│   │       └── commands/
│   │           ├── search.rs   # execute search, stream results
│   │           ├── preview.rs  # load file content for preview pane
│   │           └── settings.rs # user preferences
│   │
│   ├── desktop/                # Tauri shell (thin)
│   │   ├── src/main.rs         # Tauri setup, wires api commands to IPC
│   │   ├── tauri.conf.json
│   │   └── Cargo.toml
│   │
│   └── server/                 # Axum web server shell (future, thin)
│       ├── src/main.rs         # HTTP routes that call the same api commands
│       └── Cargo.toml
│
├── ui/                         # Shared frontend
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   └── src/
│       ├── main.tsx
│       ├── App.tsx
│       ├── services/
│       │   ├── api.ts          # SearchApi interface
│       │   ├── tauri.ts        # TauriSearchApi (invoke)
│       │   └── http.ts         # HttpSearchApi (fetch) — future
│       ├── components/
│       │   ├── SearchBar.tsx
│       │   ├── ResultList.tsx  # Virtualized match list
│       │   ├── PreviewPane.tsx # Text + PDF preview with highlights
│       │   └── DirectoryPicker.tsx
│       └── lib/
│           └── types.ts        # Auto-generated from Rust types via tauri-specta
│
└── docs/                       # Documentation
    ├── architecture.md         # This file
    └── specification.md        # MVP spec — contracts, types, behavior
```

## Key Abstractions

### 1. SearchProvider (Rust trait)

```rust
use tokio::sync::mpsc;

pub type SearchResultTx = mpsc::Sender<FileMatches>;

pub trait SearchProvider: Send + Sync {
    fn search(
        &self,
        query: &SearchQuery,
        extractors: &ExtractorRegistry,
        tx: SearchResultTx,
    ) -> Result<()>;
    fn capabilities(&self) -> SearchCapabilities;
}
```

The provider owns the directory walk so it can use `ignore` crate's parallel walker for text files and delegate to extractors for binary/structured files.

MVP: `GrepSearchProvider` using the `grep-searcher` + `grep-regex` crates (the library behind ripgrep). Future: `TantivySearchProvider`, `SemanticSearchProvider`, `HybridSearchProvider`.

### 2. ContentExtractor (Rust trait)

```rust
pub trait ContentExtractor: Send + Sync {
    fn can_handle(&self, path: &Path, mime: Option<&str>) -> bool;
    fn extract(&self, path: &Path) -> Result<ExtractedContent>;
}

pub struct ExtractedContent {
    pub text: String,
    pub source_map: SourceMap,
    pub metadata: FileMetadata,
}
```

Extractors are registered in a registry. The search pipeline asks the registry which extractor handles a file, extracts text, then searches it.

### 3. SourceMap (the critical piece for navigation/highlighting)

```rust
pub struct SourceMap {
    pub segments: Vec<SourceSegment>,
}

pub struct SourceSegment {
    pub text_range: Range<usize>,   // byte range in extracted text
    pub origin: SourceOrigin,       // where it came from
}

pub enum SourceOrigin {
    TextFile { line: u32, col: u32 },
    PdfPage { page: u32, bbox: Option<BoundingBox> },
    // Future: OfficePart { part_type, index, position }
}
```

When a match is found at byte offset N in extracted text, the SourceMap resolves it to a line number (text) or page + bounding box (PDF). The frontend uses this to navigate and highlight.

### 4. Frontend Transport Abstraction

```typescript
interface SearchApi {
  search(query: SearchQuery): AsyncIterable<FileMatches>;
  preview(file: string, match: MatchRef): Promise<PreviewData>;
  settings(): Promise<Settings>;
  updateSettings(patch: Partial<Settings>): Promise<void>;
}
```

`TauriSearchApi` uses `@tauri-apps/api` invoke/events. `HttpSearchApi` uses fetch + SSE/WebSocket. Selected by build target or environment variable. Same UI, different transport.

## Technology Choices

| Concern | Choice | Why |
|---------|--------|-----|
| Backend language | Rust | Performance, safety, great ecosystem for search/PDF |
| Desktop shell | Tauri v2 | Native, lightweight, web frontend = reusable for web app |
| Live search | `grep-searcher` crate | ripgrep's library — proven, fast, streaming |
| PDF extraction | `pdfium-render` | Google's PDF engine, cross-platform, gives text + positions |
| Future indexing | Tantivy | Rust-native Lucene equivalent, same-process, no server |
| Frontend framework | React + TypeScript | Ecosystem depth: virtualized lists, pdf.js wrappers, etc. |
| Frontend build | Vite | Fast, Tauri-integrated |
| CSS | Tailwind | Utility-first, works everywhere |
| PDF rendering (UI) | pdf.js | Industry standard, highlight overlay support |
| Code preview (UI) | CodeMirror 6 | Lightweight, line-gutter click, highlight API |
| Web server (future) | Axum | Tokio-based, pairs naturally with the async Rust backend |

## Search Flow (MVP)

```
User types query
       │
       ▼
  SearchBar → api.search(query)
       │
       ▼
  [Transport: Tauri IPC or HTTP]
       │
       ▼
  api::commands::search
       │
       ├── Create mpsc channel (tx, rx)
       ├── Spawn blocking task:
       │     GrepSearchProvider::search(query, extractors, tx)
       │       │
       │       ├── Walk directory (ignore crate, respects .gitignore)
       │       ├── For each file:
       │       │     ├── [text file] Search directly with grep-searcher
       │       │     │    └── Build Match with SourceOrigin::TextFile
       │       │     ├── [pdf file]  ExtractorRegistry::find() → PdfExtractor
       │       │     │    ├── extract() → ExtractedContent + SourceMap
       │       │     │    ├── Search extracted text in-memory
       │       │     │    └── Map offsets → SourceOrigin::PdfPage via SourceMap
       │       │     └── [unknown]  Skip
       │       └── Send FileMatches to tx as they're found
       │
       └── Forward rx items to frontend as Tauri events (streaming)
       │
       ▼
  ResultList renders matches (virtualized)
       │
       ▼
  User clicks a match
       │
       ▼
  PreviewPane loads file
       ├── Text: CodeMirror, scroll to line, highlight match
       └── PDF: pdf.js, scroll to page, draw highlight rect over bbox
```

## MVP Scope

1. **Live search** over a user-selected directory (recursive, respects .gitignore)
2. **Text files**: search with line numbers, snippet context
3. **PDF files**: extract text with page mapping, search, show page numbers
4. **Result list**: file path, line/page number, match snippet with highlight
5. **Preview pane**: click a result to see the file with the match highlighted
6. **Directory picker**: choose which folder to search
7. **Basic filters**: file type, case sensitivity, regex toggle

## Future Phases (architecture supports, not in MVP)

- **Indexing**: add `TantivySearchProvider`, background indexer, incremental updates
- **Office docs**: add `OfficeExtractor` (docx/xlsx/pptx via Rust crates)
- **Web app mode**: implement `crates/server/`, use `HttpSearchApi` in UI
- **Semantic search**: add `SemanticSearchProvider`, embedding pipeline, vector store
- **File watching**: inotify/FSEvents to keep index current

## Verification

1. `cargo build --workspace` compiles all crates
2. `cargo test --workspace` passes unit tests (extractors, source map resolution, search matching)
3. `just dev` launches the Tauri app, shows the search UI
4. Search a test directory containing .txt and .pdf files
5. Verify: results show correct line numbers (text) and page numbers (PDF)
6. Click a text result → CodeMirror opens at the right line with highlight
7. Click a PDF result → pdf.js renders the page with highlight overlay
