# Implementation plan: content bookmarks

Status: **plan only — not yet implemented.**

A bookmarks feature: select text in the PDF viewer → an "Add bookmark" button
appears → the bookmark is saved with its location (page + bbox) and quoted text.
A toggleable bookmarks pane lists and searches bookmarks, scoped to the current
file by default with a "show all" toggle, and offers "copy as markdown" per
bookmark.

## Invariant

A bookmark is **a persisted `MatchRef` + its quoted text**. It reuses the existing
`SourceOrigin` location model and the existing `selectMatch` navigation path, so
"go to bookmark" is identical to "click a search result." No second navigation or
location mechanism is introduced. Storage lives in its own `bookmarks.json`, fully
decoupled from `settings.json`.

Because the page is stored inside `origin` (`PdfPage { page, bbox }`) at capture
time, retrieving a bookmark's page is a field read — no PDF parsing or
re-derivation. A selection spanning a page boundary is pinned to the page of the
selection's **start** (anchor node's `[data-page-number]`).

Before adding anything, the pre-existing *directory* bookmarks are renamed to
**favorites** so the word "bookmark" has exactly one meaning.

## Confirmed decisions

- Pane: **toggleable independent third column**, rendered only when opened.
- Placement: **defaults to the right** of the reader, and is **movable** — the user
  can dock it left or right (a toggle, persisted). Not free drag-and-drop.
- Capture scope: **PDF only** for v1 (`SourceOrigin::TextFile` keeps text files a
  later drop-in).
- Storage: **dedicated `bookmarks.json`** in `data_dir`.
- Rename existing directory bookmarks → **favorites** in the same effort.
- **Note editing** — shipped. Notes are added/edited/cleared inline in the pane,
  persisted via a dedicated `update_bookmark_note` command / `PATCH
  /api/bookmarks/:id` endpoint, and included in the markdown export.

---

## Phase 0 — Rename directory-bookmarks → favorites (do first, isolated)

Pure rename, no behavior change.

- `crates/core/src/types.rs` — `Settings.bookmarked_dirs` → `favorites`; add
  `#[serde(alias = "bookmarked_dirs")]` so existing settings files still load.
- `crates/api/src/context.rs` (+ tests, struct literals ~line 1447+) —
  `bookmarked_dirs:` → `favorites:`.
- `ui/src/lib/types.ts` — `Settings.bookmarked_dirs` → `favorites`.
- `ui/src/stores/useSettingsStore.ts` — `bookmarks`→`favorites`,
  `addBookmark`→`addFavorite`, `removeBookmark`→`removeFavorite`; update
  `load`/`replaceSettings` mappings.
- `ui/src/App.tsx` + `ui/src/components/DirectoryPicker.tsx` (+ test) — rename
  props `bookmarks`/`onBookmarkAdd`/`onBookmarkRemove` →
  `favorites`/`onFavoriteAdd`/`onFavoriteRemove`.

---

## Phase 1 — Backend data model & store

### `crates/core/src/types.rs`

Add near `MatchRef`:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub path: PathBuf,
    pub origin: SourceOrigin,   // reuses PdfPage{page,bbox} / TextFile{line,col}
    pub quote: String,
    pub created_at: String,     // RFC3339 via chrono (already a dep)
    #[serde(default)]
    pub note: Option<String>,
}
```

### `crates/api/src/commands/bookmarks.rs` (new — mirrors `settings.rs`)

Pure async file functions, all errors propagated (no silent suppression):

- `load(path) -> Vec<Bookmark>` (missing file ⇒ `vec![]`).
- `save(path, &[Bookmark])` (create parent dir, write pretty JSON).
- `add(path, NewBookmark) -> Bookmark` — generates `id` (uuid) + `created_at`
  (chrono), appends, saves, returns it. Honors and trims the supplied `note`.
- `remove(path, id)`.
- `update_note(path, id, note) -> Bookmark` — sets or clears the note (blank ⇒
  `None`); errors on unknown id.

Also:
- Add `pub mod bookmarks;` to `crates/api/src/commands/mod.rs`.
- Add `uuid = { version = "1", features = ["v4"] }` to `crates/api/Cargo.toml`
  (already used in desktop/server).

### `crates/api/src/context.rs`

- Add field `pub bookmarks_path: PathBuf` and `bookmarks_lock: tokio::sync::Mutex<()>`;
  set in `AppContext::new` (constructor gains the path, mirroring `settings_path`).
- Business methods, each taking `bookmarks_lock` for writes:
  `list_bookmarks()`, `add_bookmark(NewBookmark)`, `remove_bookmark(id)`.

---

## Phase 2 — Command surface (both hosts)

### Desktop — `crates/desktop/src/lib.rs`

- At the `AppContext::new(...)` call site: pass `data_dir.join("bookmarks.json")`.
- Add thin `#[tauri::command]` wrappers (mirroring `get_settings`/`update_settings`
  at lines 318–324) delegating to `app_context(&app)`: `list_bookmarks`,
  `add_bookmark`, `remove_bookmark`.
- Register all three in `invoke_handler!` (line ~440).

### Server — `crates/server/src/main.rs`

- Alongside `settings_path` (line ~634): add
  `bookmarks_path = config.data_dir.join("bookmarks.json")`, pass to `AppContext::new`.
- Handlers next to `get_settings_handler` (line ~141), using `State<…>` + `state.ctx`:
  - `GET /api/bookmarks` → `list_bookmarks`
  - `POST /api/bookmarks` → `add_bookmark`
  - `DELETE /api/bookmarks/:id` → `remove_bookmark`
- Register in the `Router` (line ~656).

---

## Phase 3 — Frontend service layer

- `ui/src/lib/types.ts` — add `Bookmark` + `NewBookmark`
  (`{path, origin, quote, note?}`) matching Rust.
- `ui/src/services/api.ts` — add to `SearchApi`: `listBookmarks()`,
  `addBookmark(nb)`, `removeBookmark(id)`.
- `ui/src/services/tauri.ts` — `invoke("list_bookmarks")`, etc.
- `ui/src/services/http.ts` — `fetch("/api/bookmarks", …)` for each verb.

### `ui/src/stores/useBookmarksStore.ts` (new)

Zustand store: `bookmarks: Bookmark[]`, `filterText`, `scopePath: string | null`
(file scope vs "all"), `paneOpen: boolean`.
Actions: `load()`, `add(nb)`, `remove(id)`, `setFilter`, `setScope`, `togglePane`.
Single source of truth; **search is an in-memory filter** over `quote`/`path` (no
second search mechanism). `load()` runs on app start.

---

## Phase 4 — Selection capture (PDF only)

### `ui/src/components/preview/PdfViewer.tsx`

- On `mouseup` within the scroll container, read `window.getSelection()`. If
  non-empty and anchored inside a `[data-page-number]` element:
  - **page** = that element's `dataset.pageNumber`.
  - **bbox** = selection `getBoundingClientRect()` minus the page element's rect,
    each component divided by `pageScale` (`renderedWidth / pageMetric.width`) —
    the exact inverse of the overlay math at `PdfViewer.tsx:277`. Stored in native
    PDF page units, matching how `highlight_bbox` is consumed.
  - **quote** = `selection.toString()`.
- Render a small floating "Add bookmark" button at the selection's top-right; on
  click, call a new `onAddBookmark({page, bbox, quote})` prop.
- `PreviewPane.tsx` passes `onAddBookmark` that builds
  `NewBookmark { path: selectedMatch.path, origin: { PdfPage: { page, bbox } }, quote }`
  and calls `useBookmarksStore.add`. A toast confirms.

Text-file capture is intentionally deferred; the `SourceOrigin::TextFile` variant
makes it a later drop-in.

---

## Phase 5 — The pane (toggleable third column)

### `ui/src/components/BookmarksPane.tsx` (new)

- Virtualized list (`@tanstack/react-virtual`, as `ResultList` does).
- Top bar: filter input (reusing `ResultList`'s pattern) + scope toggle
  **"This file" / "All"** (defaults to current `selectedMatch.path`).
- Each row: quote snippet, `p.N` badge (`origin.PdfPage.page`), delete button,
  **"Copy as markdown"**.
- Row click → `useSearchStore.selectMatch({ path, origin })` → viewer navigates +
  highlights via existing machinery.

### `ui/src/lib/utils/bookmarkMarkdown.ts` (new, unit-tested)

`toMarkdown(bookmark)` →

```
> {quote}

— [{fileName}]({path}), p.{page}
```

Copy via `navigator.clipboard.writeText`.

### `ui/src/App.tsx` — layout + resize generalization + movable dock

- Render a **conditional third flex column**, only when `paneOpen`, with its own
  `bookmarksWidth` state, `minWidth`, and its own resize handle.
- **Movable dock:** read `bookmarks_dock: "left" | "right"` from settings
  (default `"right"`). Right ⇒ render the bookmarks column + handle *after* the
  reader; left ⇒ *before* it (between sidebar and reader). Conditional JSX order —
  no free drag-and-drop.
- **Generalize the resizer:** the current handler hardcodes `newWidth = e.clientX`
  (only valid for the x=0-anchored sidebar, `App.tsx:91`). Refactor to a factory
  `startResize({ getWidth, setWidth, direction })` that captures `startX`/`startWidth`
  on mousedown and applies the signed **delta** (`direction` = ±1 for a
  reader-facing handle on the pane's left vs right edge). Used by both the sidebar
  divider and the bookmarks divider; removes the left-anchor assumption cleanly.
- Clamp min widths so three columns survive narrow windows.

### `Settings` — dock persistence

- Add `bookmarks_dock: BookmarkDock` (`"left" | "right"`, default `"right"`) to the
  Rust `Settings` (`crates/core/src/types.rs`, `#[serde(default)]`) and TS `Settings`
  (`ui/src/lib/types.ts`). Set via the existing `updateSettings` patch path; no new
  command. Surfaced in `useSettingsStore` like other settings fields.

### `ui/src/components/SearchBar.tsx` + pane header

- Add a bookmark toggle button in the `settingsSlot` region (next to the gear)
  bound to `useBookmarksStore.togglePane`.
- Add a small dock-left/dock-right toggle in the `BookmarksPane` header that patches
  `bookmarks_dock`.

---

## Phase 6 — Tests

- **Rust:** `commands/bookmarks.rs` unit tests (add/remove/load-missing round-trips
  via `tempdir`, same style as `settings.rs`); `AppContext` bookmark-method tests;
  one `*_handler` test in `server/main.rs`; one `*_for_ctx` test in `desktop/lib.rs`.
- **Frontend:** `useBookmarksStore.test.ts`; `bookmarkMarkdown.test.ts`;
  `BookmarksPane.test.tsx` (filter, scope toggle, copy, row-click → `selectMatch`);
  extend `PdfViewer.test.tsx` for the selection→bbox inversion; serialization
  round-trip in `http.test.ts`/`tauri.test.ts`. Dock left/right ordering + signed
  resize delta covered in an `App` layout test.

---

## Sequencing

1. Phase 0 rename (isolated, commit-able alone).
2. Phases 1–2 backend end-to-end (+ tests).
3. Phase 3 service layer.
4. Phases 4–5 UI.
5. Phase 6 fills in alongside each phase.

## Out of scope (flagged, not silently dropped)

- Text-file bookmarks (model supports it; no capture UI yet).
- Tags / folders / reordering.
- Free drag-and-drop pane positioning (only a left/right dock toggle ships).
- Jumping from the all-bookmarks view navigates via `selectMatch` but does **not**
  auto-switch the mounted file list — acceptable, noted.
