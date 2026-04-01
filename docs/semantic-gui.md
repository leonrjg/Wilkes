# Semantic Search GUI

GUI layer for the semantic search backend specified in `docs/specs/semantic.md`.
Desktop-only (`isTauri` guard); the web build is unaffected.

---

## Scope

Two user-facing concerns:

1. **Model management** — download a model, track progress, see what is installed.
2. **Search mode** — switch between Grep and Semantic per-query from the search bar.

These are independent enough to be implemented separately.

---

## Model states

The backend moves through a linear sequence of states per model:

```
not_downloaded → downloading → ready
                    ↑ cancelable
```

Index build is a separate operation that runs after the model is ready:

```
no_index → building → indexed
              ↑ cancelable
```

The UI derives its display from `SemanticSettings` (from `getSettings`) and live
`EmbedProgress` / `EmbedDone` / `EmbedError` events. No additional state is invented.

---

## New components

### `SemanticPanel`

A self-contained panel rendered inside a settings popover. Owns all model-management
state and Tauri event subscriptions. App.tsx does not need to know about embed state.

**Responsibilities:**
- Load `settings.semantic` on mount via `getSettings`.
- Subscribe to `embed-progress`, `embed-done`, `embed-error` on mount; unsubscribe on unmount.
- Render model selector, action buttons, progress bar, and index status.
- Call `downloadModel`, `buildIndex`, `cancelEmbed`, `deleteIndex` via `TauriSearchApi`.
- After `embed-done { operation: "Build" }`, call `getIndexStatus` and refresh display.

**Does not:**
- Persist anything itself — all persistence is via `updateSettings`.
- Live in App state — it is fully self-contained.

**Model selector**

A radio group of the four `EmbedderModel` variants. Display names and sizes are
static constants in the component (they come from the spec, not the backend):

| Variant | Display name | Size |
|---|---|---|
| `MiniLML6V2` | all-MiniLM-L6-v2 | ~90 MB |
| `BgeBaseEn` | bge-base-en-v1.5 | ~430 MB |
| `BgeLargeEn` | bge-large-en-v1.5 | ~1.3 GB |
| `MultilingualE5Large` | multilingual-e5-large | ~2.3 GB |

Changing the selection calls `updateSettings({ semantic: { ...current, model: selected } })`.
If a different model is already ready, the user must delete the index first before
downloading the new one — the selector is disabled while downloading or building.

**Action buttons**

Exactly one action is available at a time:

| State | Button |
|---|---|
| not downloaded | Download |
| downloading | Cancel (calls `cancelEmbed`) |
| ready, no index | Build Index |
| building | Cancel (calls `cancelEmbed`) |
| indexed | Delete Index |

**Progress bar**

Visible only while `downloading` or `building`. Derived from `EmbedProgress`:
- `Download`: `bytes_received / total_bytes`
- `Build`: `files_processed / total_files`

Hidden on `EmbedDone` or `EmbedError`.

**Index status**

When an index exists (`IndexStatus.built_at !== null`), show:
- Files indexed, total chunks, build timestamp, model ID.

Sourced from `getIndexStatus()`, refreshed after each successful build.

---

### `SemanticToggle`

A single button in `SearchBar`, rendered after the existing `Aa` and `.*` toggles.
Label: `~` (tilde, conveying approximate/semantic match).

**Enabled** when `settings.semantic.enabled && semanticIndexBuilt`.
**Disabled** (greyed out, tooltip: "Set up semantic search in Settings") otherwise.

When active, `SearchQuery.mode` is set to `"Semantic"`. When inactive, `"Grep"` (the
default — no change to existing `buildQuery` logic beyond adding the `mode` field).

The toggle state is local to `SearchBar` (like `isRegex` and `caseSensitive`).
Switching mode re-triggers search immediately if a pattern is present, following the
same pattern as the existing option toggles.

---

## Integration points

### `App.tsx`

Minimal changes:

1. Add a settings button (gear icon) to the top bar that opens a popover containing
   `SemanticPanel`. The popover is self-contained; App.tsx only controls open/closed.
2. Pass `semanticReady` (bool) down to `SearchBar` so `SemanticToggle` can be
   enabled/disabled. Derive it from `settings.semantic` loaded in the existing
   `getSettings` effect: `settings.semantic.enabled && semanticIndexBuilt`.
3. `semanticIndexBuilt` must be re-read after `SemanticPanel` reports a successful
   build. Use a `refreshSemanticReady` callback passed to `SemanticPanel` and called
   on `embed-done { operation: "Build" }`.

### `SearchBar`

1. Accept `semanticReady: boolean` prop.
2. Add `SemanticToggle` component (inline or separate file) — a `Toggle` with the
   label `~`, disabled when `!semanticReady`.
3. Add `mode` field to `buildQuery`:

```ts
mode: isSemanticMode ? "Semantic" : "Grep",
```

4. `isSemanticMode` resets to `false` when `semanticReady` becomes false (model
   deleted), using a `useEffect`.

### `SearchApi` / `TauriSearchApi`

The embed methods (`downloadModel`, `buildIndex`, etc.) already exist in
`TauriSearchApi` (see `ui/src/services/tauri.ts`). No changes needed to `SearchApi`
interface — embed is desktop-only and `SemanticPanel` imports `TauriSearchApi`
directly, casting from `api` when `isTauri`.

---

## What does not change

- `ResultList`, `PreviewPane`, `ExtensionFilter`, `DirectoryPicker`, `UploadZone` —
  no awareness of search mode needed; results are already `FileMatches[]` regardless
  of backend.
- `HttpSearchApi` — semantic is desktop-only; no server-side embed API.
- `SearchStats` display — same fields, no additions.
- Tauri event protocol — events are already defined and match the backend spec.
