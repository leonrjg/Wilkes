# Web Mode Specification

Wilkes currently runs as a Tauri desktop app. This specification adds an optional web mode where Wilkes runs as an HTTP server in Docker and users interact through a browser. The desktop app remains unchanged.

## Design Principle

Once files exist on disk, every operation (search, preview, list, settings) is identical between desktop and web. The only divergence is **how files become available**: the desktop app reads the user's local filesystem directly; the web app receives files via upload. This spec isolates that seam cleanly so neither mode is aware of the other.

## Scope

### New artifacts
- `crates/server/` -- Axum HTTP server wrapping `wilkes-api`
- `ui/src/services/http.ts` -- HTTP implementation of `SearchApi`
- `ui/src/components/UploadZone.tsx` -- file upload component (web mode)
- `Dockerfile`

### Modified artifacts
- `Cargo.toml` -- add `crates/server` to workspace members
- `wilkes-api` settings functions -- accept a `path` parameter instead of hardcoding `dirs::config_dir()`
- `ui/src/services/api.ts` -- extract `pickDirectory` into separate `SourceApi`
- `ui/src/services/tauri.ts` -- implement `SourceApi`
- `ui/src/components/App.tsx` -- runtime mode detection, conditional source component, replace all direct `tauriApi` references with the `api`/`source` variables
- `ui/src/components/PreviewPane.tsx` -- support HTTP URLs for PDF loading
- `justfile` -- add server dev/build commands

### Untouched
- `wilkes-core` -- zero changes
- `wilkes-desktop` -- zero changes (passes `dirs::config_dir()` to the updated `wilkes-api` settings functions)
- All components except `App.tsx` and `PreviewPane.tsx`

---

## 1. Interface Changes

### 1.1 Split `SearchApi`

`pickDirectory` is the only platform-divergent method. Extract it into a separate interface.

**`ui/src/services/api.ts`** becomes:

```typescript
// Shared across desktop and web. All methods are identical.
export interface SearchApi {
  search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string>;
  cancelSearch(searchId: string): Promise<void>;
  preview(matchRef: MatchRef): Promise<PreviewData>;
  getSettings(): Promise<Settings>;
  updateSettings(patch: Partial<Settings>): Promise<Settings>;
  listFiles(root: string): Promise<FileEntry[]>;
  openFile(path: string): Promise<PreviewData>;
  resolvePdfUrl(path: string): string;
}

// Desktop: native directory picker.
// Web: file upload returning a server-side root path.
export interface SourceApi {
  type: "desktop" | "web";
}

export interface DesktopSourceApi extends SourceApi {
  type: "desktop";
  pickDirectory(): Promise<string | null>;
}

export interface WebSourceApi extends SourceApi {
  type: "web";
  uploadFiles(files: File[]): Promise<string>;
  deleteFile(path: string): Promise<void>;
  deleteAll(): Promise<void>;
}
```

### 1.2 `resolvePdfUrl`

Currently `PreviewPane` calls `convertFileSrc(path)` to build an asset-protocol URL. This is Tauri-specific. The new `resolvePdfUrl(path)` method on `SearchApi` abstracts this:

- **Tauri implementation**: returns `convertFileSrc(path)` (current behavior).
- **HTTP implementation**: returns `/asset?path=<encoded_path>`.

`PreviewPane` calls `api.resolvePdfUrl(path)` instead of `convertFileSrc(path)` directly. This is the only change to `PreviewPane`.

---

## 2. Backend: `crates/server`

An Axum HTTP server that wraps the same `wilkes-api` functions used by `crates/desktop`.

### 2.1 Crate setup

**`crates/server/Cargo.toml`**:
```toml
[package]
name = "wilkes-server"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "wilkes-server"

[dependencies]
wilkes-api = { path = "../api" }
wilkes-core = { path = "../core" }
axum = { version = "0.8", features = ["multipart"] }
tokio = { version = "1", features = ["full"] }
tokio-stream = "0.1"
tower-http = { version = "0.6", features = ["fs", "cors"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
anyhow = "1"
```

Add `"crates/server"` to workspace members in the root `Cargo.toml`.

### 2.2 Startup

```
wilkes-server [--port PORT] [--data-dir DIR] [--dist-dir DIR]
```

- `--port`: listen port, default `3000`. Also configurable via `WILKES_PORT`.
- `--data-dir`: root storage directory, default `/data`. Also configurable via `WILKES_DATA_DIR`.
- `--dist-dir`: path to the frontend `dist/` directory, default `./dist`. Also configurable via `WILKES_DIST_DIR`.

Directory structure under `data-dir`:
```
/data/
  uploads/          -- single persistent folder for all uploaded files
  settings.json     -- persisted settings
```

On startup the server:
1. Creates `data-dir/uploads/` if missing.
2. Serves the frontend from a bundled `dist/` directory (embedded or adjacent).
3. Registers all API routes.

### 2.3 HTTP API

All endpoints accept and return JSON unless noted. Errors return `{ "error": string }` with appropriate status codes.

#### `POST /api/search`

Start a search. Returns an SSE event stream.

**Request body**: `SearchQuery` (JSON).

**Response**: `Content-Type: text/event-stream`. Two event types:

```
event: result
data: <FileMatches JSON>

event: complete
data: <SearchStats JSON>
```

The connection closes after the `complete` event.

To cancel: the client closes the connection. The server detects the dropped receiver and aborts the search task. No separate cancel endpoint is needed because SSE already provides a clean cancellation signal via connection close.

**Implementation**: calls `wilkes_api::commands::search::start_search(query)`, which returns a `SearchHandle` (not a bare channel). The server calls `handle.next()` in a loop, serializing each `FileMatches` as an SSE `result` event. When `next()` returns `None`, calls `handle.finish()` to collect non-fatal errors (`Vec<String>`). The server must assemble the `SearchStats` itself (file count, match count, timing, errors) — `wilkes-api` does not produce `SearchStats` directly. Sends `complete` with the assembled stats.

#### `POST /api/preview`

**Request body**: `MatchRef` (JSON).

**Response**: `PreviewData` (JSON).

**Implementation**: calls `wilkes_api::commands::preview::preview(match_ref)`.

#### `GET /api/settings`

**Response**: `Settings` (JSON).

**Implementation**: calls `wilkes_api::commands::settings::get_settings(path)` with `data-dir/settings.json`.

#### `PATCH /api/settings`

**Request body**: `Partial<Settings>` (JSON patch).

**Response**: merged `Settings` (JSON).

**Implementation**: calls `wilkes_api::commands::settings::update_settings(path, patch)` with `data-dir/settings.json`.

#### `GET /api/files?root=<path>`

**Response**: `FileEntry[]` (JSON).

**Implementation**: calls `wilkes_api::commands::files::list_files(root)`.

#### `POST /api/file`

**Request body**: `{ "path": string }` (JSON).

**Response**: `PreviewData` (JSON).

**Implementation**: calls `wilkes_api::commands::files::open_file(path)`.

#### `POST /api/upload`

Upload files to the server. All uploads go into a single persistent folder (`data-dir/uploads/`). Uploading is additive -- new files are added alongside existing ones. If a file with the same relative path already exists, it is overwritten.

**Request**: `multipart/form-data`. Fields:
- `files`: one or more files. Relative paths are preserved from the `filename` field if the browser provides them (for directory uploads via `webkitdirectory`).

**Response**:
```json
{
  "root": "/data/uploads",
  "file_count": 42
}
```

**Implementation**:
1. Stream each multipart part to `data-dir/uploads/`, preserving relative directory structure.
2. Overwrite existing files at the same path.
3. Return the root path and count of files written.

**Constraints**:
- Maximum total upload size configurable via `Settings` (default 500 MB). The server checks cumulative size of the uploads directory before accepting new files.

#### `DELETE /api/upload?path=<relative_path>`

Delete a specific file or directory from the uploads folder.

**Request**: query param `path` -- relative path within `data-dir/uploads/`.

**Response**: `204 No Content`.

**Validation**: canonicalize the resolved path and reject if it does not fall under `data-dir/uploads/`. Reject attempts to delete the uploads root itself (use a separate `DELETE /api/upload/all` for that).

#### `DELETE /api/upload/all`

Delete all uploaded files (clear the uploads folder).

**Response**: `204 No Content`.

#### `GET /asset`

Serve a file from disk for PDF viewing.

**Query params**: `path` -- absolute path to the file.

**Response**: file contents with appropriate `Content-Type` (e.g., `application/pdf`).

**Validation**: only serves files under `data-dir/uploads/`. Rejects path traversal.

### 2.4 Settings path override

`wilkes-api` currently resolves the settings path via `dirs::config_dir()`. The settings functions are updated to accept a `path` parameter instead. The desktop crate passes the `dirs::config_dir()` path; the server crate passes `data-dir/settings.json`. This is the only change to `wilkes-api`.

### 2.5 Path security

All endpoints that accept file paths (`/asset`, `/api/files`, `/api/upload` DELETE) validate that resolved paths fall within `data-dir/`. The server canonicalizes paths and rejects anything outside the boundary.

---

## 3. Frontend: HTTP Service

### 3.1 `ui/src/services/http.ts`

Implements `SearchApi` and `WebSourceApi`.

```typescript
export class HttpSearchApi implements SearchApi {
  async search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string> {
    // POST /api/search with query body
    // Parse SSE stream:
    //   "result" events -> onResult(JSON.parse(data))
    //   "complete" event -> onComplete(JSON.parse(data))
    // Return an ID (can be the AbortController reference)
    // Store AbortController keyed by ID for cancelSearch
  }

  async cancelSearch(searchId: string): Promise<void> {
    // Abort the stored AbortController for this search ID
    // This closes the SSE connection, signaling the server to stop
  }

  async preview(matchRef: MatchRef): Promise<PreviewData> {
    // POST /api/preview
  }

  async getSettings(): Promise<Settings> {
    // GET /api/settings
  }

  async updateSettings(patch: Partial<Settings>): Promise<Settings> {
    // PATCH /api/settings
  }

  async listFiles(root: string): Promise<FileEntry[]> {
    // GET /api/files?root=encodeURIComponent(root)
  }

  async openFile(path: string): Promise<PreviewData> {
    // POST /api/file
  }

  resolvePdfUrl(path: string): string {
    return `/asset?path=${encodeURIComponent(path)}`;
  }
}

export class HttpSourceApi implements WebSourceApi {
  type = "web" as const;

  async uploadFiles(files: File[]): Promise<string> {
    // POST /api/upload as multipart/form-data
    // Returns root path from response
  }

  async deleteFile(path: string): Promise<void> {
    // DELETE /api/upload?path=encodeURIComponent(path)
  }

  async deleteAll(): Promise<void> {
    // DELETE /api/upload/all
  }
}
```

### 3.2 `ui/src/services/tauri.ts` changes

Move `pickDirectory` implementation into a `TauriSourceApi`:

```typescript
export class TauriSourceApi implements DesktopSourceApi {
  type = "desktop" as const;

  async pickDirectory(): Promise<string | null> {
    // existing implementation
  }
}
```

Add `resolvePdfUrl` to the existing `TauriSearchApi`:

```typescript
resolvePdfUrl(path: string): string {
  return convertFileSrc(path);
}
```

Remove `pickDirectory` from `TauriSearchApi`.

---

## 4. Frontend: Components

### 4.1 `App.tsx` -- mode detection

At startup, detect the runtime environment once:

```typescript
const isTauri = "__TAURI_INTERNALS__" in window;
```

Instantiate the appropriate API pair:

```typescript
const api: SearchApi = isTauri ? new TauriSearchApi() : new HttpSearchApi();
const source: SourceApi = isTauri ? new TauriSourceApi() : new HttpSourceApi();
```

Pass `source` down only to the directory/source selection component. Pass `api` to everything that needs search/preview/settings (same as today, just through the interface).

Render conditionally:

```tsx
{source.type === "desktop" ? (
  <DirectoryPicker ... />
) : (
  <UploadZone ... />
)}
```

### 4.2 `UploadZone.tsx` -- new component

Replaces `DirectoryPicker` in web mode. Provides the mechanism for users to get files onto the server.

**Behavior**:
1. On mount, if `last_directory` is already set in settings (from a previous session), the app loads it the same way desktop does -- `UploadZone` receives it as a prop and displays the existing file list via `listFiles(root)`.
2. Renders a drop zone that accepts files or a directory (via `<input webkitdirectory>`).
3. On drop/select, calls `source.uploadFiles(files)`. New files are added alongside existing ones; duplicates are overwritten.
4. Receives the server-side `root` path from the response.
5. Sets `root` in app state and persists it via `updateSettings({ last_directory: root })` -- same flow as `DirectoryPicker`.
6. Shows upload progress via XHR progress events.
7. Displays the current file list with per-file delete buttons (calls `source.deleteFile(path)`).
8. Provides a "clear all" action that calls `source.deleteAll()` and resets state.

**Constraints**:
- Maximum total upload size configurable via settings (default 500 MB), enforced server-side.

### 4.3 `PreviewPane.tsx` -- PDF URL change

Replace the direct `convertFileSrc(path)` call with `api.resolvePdfUrl(path)`. This is a single-line change. The `api` instance is already available in the component's scope (passed as prop or via context).

### 4.4 `DirectoryPicker.tsx` -- no changes

Remains exactly as-is. Only rendered in desktop mode.

---

## 5. Docker

### 5.1 Dockerfile

Multi-stage build:

```dockerfile
# Stage 1: Build Rust server
FROM rust:1.87-bookworm AS rust-builder
# Install mupdf system dependencies
# Copy workspace, build --release --bin wilkes-server

# Stage 2: Build frontend
FROM node:22-bookworm AS ui-builder
# Copy ui/, npm ci, npm run build

# Stage 3: Runtime
FROM debian:bookworm-slim
# Install runtime libs (libmupdf, etc.)
# Copy wilkes-server binary from rust-builder
# Copy ui/dist from ui-builder
VOLUME /data
EXPOSE 3000
CMD ["wilkes-server", "--data-dir", "/data", "--dist-dir", "/app/dist", "--port", "3000"]
```

### 5.2 Static file serving

The server serves the frontend `dist/` directory at the root path (`/`). API routes are prefixed with `/api/`. The `/asset` route is at the top level for simplicity.

Route layout:
```
/               -- serves index.html (SPA fallback)
/assets/*       -- serves Vite-built static assets
/api/*          -- JSON API endpoints
/asset          -- file serving for PDFs
```

---

## 6. Implementation Order

### Phase 1: Interface refactor
1. Split `SearchApi` / `SourceApi` in `api.ts`.
2. Update `tauri.ts` to match (move `pickDirectory` to `TauriSourceApi`, add `resolvePdfUrl`).
3. Update `App.tsx` to use the new interfaces.
4. Update `PreviewPane.tsx` to use `resolvePdfUrl`.
5. Verify desktop app works identically.

### Phase 2: Server crate
1. Scaffold `crates/server` with Axum.
2. Implement endpoints in order: settings, files/list, file/open, preview, asset, search (SSE), upload, upload delete.
3. Static file serving for `dist/`.
4. Path security validation.

### Phase 3: Web frontend
1. Implement `HttpSearchApi` and `HttpSourceApi` in `http.ts`.
2. Build `UploadZone` component.
3. Wire up mode detection in `App.tsx`.
4. Test in browser against running server.

### Phase 4: Docker
1. Write Dockerfile.
2. Verify build and runtime.
3. Add `justfile` targets for server dev and Docker build.

---

## 7. Resolved Decisions

1. **Upload size limit**: configurable via `Settings` (persisted in `data-dir/settings.json`), default 500 MB.
2. **Upload model**: single persistent folder (`data-dir/uploads/`). Users add files incrementally and delete individually. No batch concept. Duplicate filenames are overwritten.
3. **Frontend bundling**: serve from filesystem (`--dist-dir` flag, default `./dist`). The Dockerfile copies the built frontend adjacent to the binary. Embedding via `rust-embed` can be added behind a cargo feature later if standalone binary distribution becomes a goal.
