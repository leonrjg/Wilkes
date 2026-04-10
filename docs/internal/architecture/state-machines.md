# State Machines

This document captures the explicit state machines in Wilkes: machines with named states, reducer actions, enum variants, or clear command/event-driven transitions in the runtime.

It intentionally excludes smaller implicit UI flows such as upload progress, modal visibility, or preview loading.

## Scope

The explicit machines covered here are:

1. `SemanticPanel` reducer and derived phase machine
2. `useSemanticStore` root indexing machine
3. `AppContext` embed task lifecycle
4. `AppContext` startup restore machine
5. `WorkerRuntime` / `WorkerManager` lifecycle
6. Worker request mode dispatch
7. `IndexWatcher` incremental reindex flow

## 1. Semantic Panel Machine

Code:

- `ui/src/components/SemanticPanel.tsx`

This machine is split across:

- reducer state: `activeOp`, `pendingBuild`, `buildRequest`, `isCancelling`, `indexStatus`, `error`
- derived phase: `not_downloaded | downloading | ready | building | indexed | engine_mismatch`

### State meanings

| State | Meaning |
| --- | --- |
| `not_downloaded` | Selected model is not available locally, or no semantic settings are active. |
| `downloading` | Model download is in progress. |
| `ready` | Model is available and the app can build an index for the current root. |
| `building` | Index build is in progress. |
| `indexed` | A compatible index exists and contains indexed content. |
| `engine_mismatch` | An index exists, but its engine/model differs from the currently selected engine/model. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| Any | `init_loaded` / `models_loaded` / `index_loaded` | Derived by `derivePhase` | Recomputed | Non-terminal bookkeeping actions can move the derived phase indirectly. |
| `not_downloaded` | User clicks primary action | `effectiveSelected != null` | `downloading` | Reducer queues a pending build, then `api.downloadModel(...)` starts. |
| `downloading` | `progress` with download payload | None | `downloading` | Progress updates only. |
| `downloading` | `onEmbedDone("Download")` with `pendingBuild` present | None | `building` | `launch_pending_build` moves queued build into `buildRequest` and starts build. |
| `downloading` | `onEmbedDone("Download")` with no `pendingBuild` | None | Recomputed, usually `ready` | Download completed but no immediate build was queued. |
| `downloading` | User clicks cancel | None | Cancelling substate, then recomputed | `cancel_started`, then `cancel_completed` clears active op. |
| `downloading` | `op_error` | None | Recomputed, usually `not_downloaded` | Error clears `activeOp` and leaves model/index status to subsequent fetches. |
| `ready` | User clicks primary action | `effectiveSelected != null` | `building` | `api.buildIndex(...)` starts; progress event moves reducer into active build state. |
| `ready` | Fresh compatible index loaded | Indexed files/chunks > 0 | `indexed` | Happens after fetch or build completion. |
| `building` | `progress` with build payload | None | `building` | Progress updates only. |
| `building` | `onEmbedDone("Build")` | None | `indexed` or `ready` | Depends on fetched `indexStatus`. Normal success is `indexed`. |
| `building` | User clicks cancel | None | Cancelling substate, then recomputed | Cancels backend build. |
| `building` | `op_error` | None | Recomputed, usually `ready` or `not_downloaded` | Error clears active build state. |
| `indexed` | User changes engine/model selection to mismatch existing index | Selected engine/model differs from `indexStatus` | `engine_mismatch` | Purely derived transition. |
| `indexed` | User clicks delete index | None | `ready` or `not_downloaded` | `index_deleted` clears index status; semantic root store is also updated. |
| `engine_mismatch` | User clicks primary action | None | `building` | UI labels this as "Save model", but the effect is a reindex. |
| `engine_mismatch` | User re-selects engine/model matching existing index | None | `indexed` | Purely derived transition. |

### Notes

- Cancellation is represented as flags inside the reducer, not as a top-level phase.
- `buildRequest` is a short-lived handoff state between download completion and build dispatch.

## 2. Semantic Root Indexing Machine

Code:

- `ui/src/stores/useSemanticStore.ts`

This store tracks whether semantic search is ready for the current root and whether the current root is blocked, checking, or building.

### States

| State | Meaning |
| --- | --- |
| `idle` | No directory is selected. |
| `checking` | The store is fetching current index status for the current directory. |
| `missing` | No usable semantic index exists for the current directory. |
| `ready` | A usable semantic index exists for the current directory. |
| `building` | A build is in progress for `buildRoot`. |
| `error` | Reading status or starting a build failed. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| Any | `refreshCurrentRootStatus()` | `directory` is empty | `idle` | Clears `indexStatus`, `buildRoot`, `blockedRoot`, `error`. |
| Any except `building` | `refreshCurrentRootStatus()` | `directory` exists and `buildRoot !== directory` | `checking` | Transitional fetch state. |
| `building` | `refreshCurrentRootStatus()` | `buildRoot === directory` | `building` | Remains building while status is polled. |
| `checking` / `building` | Status fetch succeeds | Index usable for directory | `ready` | Also clears `blockedRoot`. |
| `checking` | Status fetch succeeds | Index unusable and not building current root | `missing` | Current directory lacks a usable index. |
| `building` | Status fetch succeeds | Index unusable and `buildRoot === directory` | `building` | Build still considered in progress. |
| `checking` | Status fetch fails | `buildRoot !== directory` | `error` | Status read failed. |
| `building` | Status fetch fails | `buildRoot === directory` | `building` | Errors are suppressed into "still building". |
| Any | `ensureCurrentRootIndexed()` | No directory | `idle` | Delegates to `refreshCurrentRootStatus`. |
| Any | `ensureCurrentRootIndexed()` | `blockedRoot === directory` and not fresh attempt | No transition | Guard prevents repeated auto-builds after deletion/cancel-like cases. |
| Any | `ensureCurrentRootIndexed()` | `preferSemantic == false` | `ready` or `missing` | Only refreshes; does not auto-build. |
| `missing` / `checking` / `error` | `ensureCurrentRootIndexed()` | `preferSemantic == true`, semantic settings exist, not already building | `building` | Sets `buildRoot`, clears `blockedRoot`, starts `api.buildIndex(...)`. |
| `building` | `handleIndexUpdated()` | Refreshed status now usable | `ready` | Also clears `buildRoot` and replays deferred search. |
| `building` | `handleIndexUpdated()` | Refreshed status still unusable | `building` or `missing` | Depends on refresh result. |
| Any | `handleCurrentRootIndexRemoved()` | `directory` exists | `missing` | Also sets `blockedRoot = directory` to prevent immediate rebuild loop. |
| Any | `handleCurrentRootIndexRemoved()` | No directory | `idle` | Clears ready state. |

### Notes

- `blockedRoot` is effectively a guard-state memory, not a visible status.
- Directory and `preferSemantic` subscriptions are transition triggers from outside the store.

## 3. AppContext Embed Task Lifecycle

Code:

- `crates/api/src/context.rs`

This machine governs background semantic operations tracked by `embed_task`.

### States

| State | Meaning |
| --- | --- |
| `Idle` | No embed task is active. |
| `Downloading` | A model download task is active. |
| `Building` | An index build task is active. |
| `CancellingDownload` | Download cancellation has been requested; task will be aborted. |
| `CancellingBuild` | Build cancellation has been requested; task observes cancellation cooperatively. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| `Idle` | `start_download_model(selected)` | `prepare_download_model` succeeds | `Downloading` | Stores `EmbedTaskHandle { operation: Download, ... }`. |
| `Idle` | `start_build_index(root, selected)` | `prepare_build_index` succeeds | `Building` | Stops watcher, emits `Reindexing`, stores build task handle. |
| `Downloading` | Download completes successfully | Model probe/load succeeds | `Idle` | Emits `embed-done { operation: "Download" }`, clears task. |
| `Downloading` | Download completes with error | None | `Idle` | Emits `embed-error`, clears task. |
| `Downloading` | `cancel_embed()` | None | `CancellingDownload` then `Idle` | Requests worker shutdown, emits cancel-as-error, aborts join handle. |
| `Building` | Build task finishes successfully | `finish_build_index(...)` succeeds | `Idle` | Stores embedder/index, restarts watcher, updates settings, emits `embed-done`. |
| `Building` | Build task finishes with error | None | `Idle` | Emits `embed-error`, clears task. |
| `Building` | `cancel_embed()` | None | `CancellingBuild` then `Idle` | Cancellation token is signaled; task cleans up temp DB files and emits cancelled events. |
| `Building` | `start_search(Semantic)` | Build still running | `Building` | Search is rejected with "currently being built". |
| Any active state | `shutdown()` | None | `Idle` | Stops watcher, cancels embed, kills worker. |

### Notes

- Download cancellation is abrupt (`join.abort()`); build cancellation is cooperative.
- The UI also observes a related event stream: `embed-progress`, `embed-done`, `embed-error`, and manager events such as `Reindexing`.

## 4. AppContext Startup Restore Machine

Code:

- `crates/api/src/context.rs`

This machine restores semantic state from disk on startup.

### States

| State | Meaning |
| --- | --- |
| `Start` | Restore process has begun. |
| `NoIndex` | No readable on-disk index status exists. |
| `ResetStaleSelection` | Settings point at an index whose engine/model no longer matches selected settings. |
| `PlanReady` | Restore plan is valid and can be executed. |
| `EmbedderRestored` | Model is available and embedder has been rebuilt. |
| `IndexRestored` | Index has been reopened successfully. |
| `Restored` | Runtime state and watcher are fully restored. |
| `Abort` | Restore ended without loading semantic runtime state. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| `Start` | `load_restore_db_status()` | No DB status readable | `NoIndex` | May also clear stale settings if semantic settings still claim an index. |
| `NoIndex` | Return from restore | None | `Abort` | Nothing is restored into memory. |
| `Start` | `prepare_restore_state_plan(settings, db_status)` | DB status matches settings selection | `PlanReady` | Produces `RestoreStatePlan`. |
| `Start` | `prepare_restore_state_plan(settings, db_status)` | DB status mismatches selected engine/model | `ResetStaleSelection` | Explicit enum variant in code. |
| `ResetStaleSelection` | Clear semantic settings | None | `Abort` | Prevents restoring stale runtime state. |
| `PlanReady` | `restore_embedder(...)` succeeds | None | `EmbedderRestored` | Installer probes and builds embedder from local files. |
| `PlanReady` | `restore_embedder(...)` fails | None | `Abort` | Missing model files or build failure. |
| `EmbedderRestored` | `restore_index(...)` succeeds | None | `IndexRestored` | Reopens semantic DB with expected dimension. |
| `EmbedderRestored` | `restore_index(...)` fails | None | `Abort` | Restore cannot continue. |
| `IndexRestored` | `finish_restore_state(...)` | None | `Restored` | Stores embedder/index, maybe starts watcher, updates semantic settings. |

### Notes

- `RestoreStatePreparation` is the explicit enum for the branch between `Ready` and `ResetStaleSelection`.
- This machine is one-shot and runs during startup.

## 5. Worker Runtime / Worker Manager Lifecycle

Code:

- `crates/core/src/embed/worker/manager.rs`
- `crates/core/src/embed/worker/runtime.rs`

This machine owns the external worker process and keeps `WorkerStatus` synchronized.

### States

| State | Meaning |
| --- | --- |
| `Idle` | No active worker process. |
| `Starting` | A request needs a worker process and spawn is in progress. |
| `Active` | A worker process is alive and can accept requests. |
| `Restarting` | Existing worker is being shut down before spawning a replacement. |
| `TimedOut` | Idle timeout fired and active worker is being cleared. |
| `ShuttingDown` | User or owner requested shutdown of the active worker. |
| `Panicked` | Runtime loop panicked and supervision is resetting command channel and status. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| `Idle` | `ManagerCommand::Submit` | Request requires worker | `Starting` | `ensure_worker(...)` decides a worker is needed. |
| `Starting` | Spawn succeeds | None | `Active` | Emits `ManagerEvent::WorkerStarting`, updates status active. |
| `Starting` | Spawn fails | None | `Idle` | Sends `WorkerEvent::Error` back to caller. |
| `Active` | `ManagerCommand::Submit` | Same engine as active worker | `Active` | Reuses process; may hot-swap tracked model/device metadata. |
| `Active` | `ManagerCommand::Submit` | Different engine | `Restarting` | Existing worker is shut down before replacement spawn. |
| `Restarting` | Shutdown complete and spawn succeeds | None | `Active` | Status moves back to active with new engine/model/device. |
| `Restarting` | Replacement spawn fails | None | `Idle` | Error returned to caller. |
| `Active` | `ManagerCommand::ShutdownWorker` | None | `ShuttingDown` | Clears active worker and status. |
| `ShuttingDown` | Shutdown completes | None | `Idle` | Also reachable from channel close or timeout cleanup. |
| `Active` | Idle timeout expires | None | `TimedOut` | `handle_idle_timeout()` clears worker. |
| `TimedOut` | Cleanup completes | None | `Idle` | Status becomes inactive. |
| Any | Command channel closes | Active worker exists | `ShuttingDown` | Cleanup before loop exit. |
| Any | Runtime task panics | None | `Panicked` | Supervisor resets status and swaps in a fresh command channel. |
| `Panicked` | Supervisor restart | None | `Idle` | Fresh runtime loop starts with no active worker. |

### Notes

- Request handling also has a nested dispatch step after the runtime is active; that is captured separately below as request mode dispatch.
- `WorkerStatus` is a denormalized snapshot of the current runtime state.

## 6. Worker Request Mode Dispatch

Code:

- `crates/core/src/embed/worker/ipc.rs`
- `crates/worker/src/main.rs`
- `crates/worker/wilkes_python_worker/__main__.py`

This machine selects which worker behavior to execute for a single request.

### States

| State | Meaning |
| --- | --- |
| `Build` | Build or update semantic index. |
| `Embed` | Generate embeddings for input text. |
| `Info` | Return model metadata such as dimension/max sequence length. |
| `Unknown` | Unsupported mode. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| Request received | `mode == "build"` | Rust worker only | `Build` | Selected by `classify_worker_request`. |
| Request received | `mode == "embed"` | None | `Embed` | Supported by both Rust and Python workers. |
| Request received | `mode == "info"` | None | `Info` | Supported by both Rust and Python workers. |
| Request received | Any other mode | None | `Unknown` | Emits an error event. |

### Notes

- The Rust IPC struct comments still mention `"build" or "embed"`, but runtime dispatch also supports `"info"`.
- The Python worker does not implement `build`; unknown modes become errors there.

## 7. IndexWatcher Incremental Reindex Flow

Code:

- `crates/core/src/embed/index/watcher.rs`

This machine reacts to debounced filesystem events and updates the index incrementally.

### States

| State | Meaning |
| --- | --- |
| `Watching` | Watcher is active and waiting for debounced fs events. |
| `Classifying` | Debounced event batch is being classified into changed vs removed paths. |
| `Removing` | Removed files are being deleted from the index. |
| `Reindexing` | Changed files are being re-extracted and re-embedded. |
| `WatchError` | A watcher event batch returned an error. |
| `Stopped` | Watcher has been dropped or explicitly stopped. |

### Transition table

| Current state | Event / trigger | Guard | Next state | Notes |
| --- | --- | --- | --- | --- |
| `Watching` | Debounced event batch received | None | `Classifying` | Batch may contain mixed change/remove paths. |
| `Classifying` | Removed paths present | None | `Removing` | Removes stale files from index first. |
| `Removing` | Changed paths also present | None | `Reindexing` | Removal and reindex can happen in same batch. |
| `Classifying` | Changed paths present, none removed | None | `Reindexing` | Emits `on_reindex()` before processing files. |
| `Classifying` | No relevant paths | None | `Watching` | Batch is ignored. |
| `Reindexing` | All changed paths processed | None | `Watching` | Emits `on_reindex_done()` after processing batch. |
| `Watching` | Watch result is `Err(...)` | None | `WatchError` | Logs watcher error. |
| `WatchError` | Error handled | None | `Watching` | Watcher thread continues. |
| Any non-stopped state | `stop()` / drop | None | `Stopped` | Drops debouncer and joins worker thread. |

### Notes

- Within `Reindexing`, each path has its own micro-flow: wait for exclusive open, extract, write to index, or log error.
- `on_reindex` and `on_reindex_done` are the hooks that become manager events in higher layers.

## Cross-machine relationships

These machines are coupled in a few important places:

- `WorkerRuntime` supports `AppContext` download/build tasks.
- `AppContext` emits embed and manager events consumed by `SemanticPanel` and `useGlobalEvents`.
- `useSemanticStore` reacts to completed builds and unlocks deferred semantic searches.
- `IndexWatcher` produces `Reindexing` / `ReindexingDone`, which drive both UI toast behavior and semantic-store refresh.

## Suggested follow-ups

If we want to formalize these further, the best candidates are:

1. Extract `SemanticPanel` into an explicit finite-state machine where cancellation is a first-class state.
2. Unify `useSemanticStore` and `AppContext` build lifecycle terminology so UI and backend use the same state names.
3. Document the mismatch between worker IPC comments and actual supported modes (`build`, `embed`, `info`).
