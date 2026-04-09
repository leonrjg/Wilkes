# High-Coverage Refactor Spec

## Purpose

Design the remaining refactors needed to make the hard-to-test files in this workspace realistically capable of reaching `95%` to `100%` line coverage in eventual tests, without changing production behavior.

This document is an implementation spec for future agents. It does **not** include tests. It defines the seams, helper boundaries, and module splits that should be introduced so tests can later drive the code directly.

## Current Baseline

From the latest Tarpaulin run:

- Workspace: `2785/3674` lines covered (`75.80%`)
- Highest remaining miss buckets:
  - [`crates/server/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/server/src/main.rs): `192/372`
  - [`crates/core/src/embed/engines/candle.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/candle.rs): `102/252`
  - [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs): `391/511`
  - [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs): `72/180`
  - [`crates/core/src/embed/engines/fastembed.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/fastembed.rs): `61/151`
  - [`crates/core/src/embed/index/watcher.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/index/watcher.rs): `49/96`
  - [`crates/worker/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/worker/src/main.rs): `57/97`
  - [`crates/core/src/embed/worker/process.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/process.rs): `98/117`
  - [`crates/core/src/embed/worker/runtime.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/runtime.rs): `118/131`

## Implementation Status

Completed in the current workspace:

- [`crates/server/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/server/src/main.rs)
  - Added `crates/server/src/config.rs`
  - Added `crates/server/src/http/errors.rs`
  - Added `crates/server/src/http/search.rs`
  - Added `crates/server/src/http/state.rs`
  - Moved upload, asset, search, and config planning logic behind narrower helpers
- [`crates/core/src/embed/engines/candle.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/candle.rs)
  - Added artifact resolution, device planning, runtime factory, installer fetcher, and embedder build-plan seams
- [`crates/core/src/embed/engines/fastembed.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/fastembed.rs)
  - Added catalog, execution-plan, runtime-factory, and HF size-planning helpers
- [`crates/core/src/embed/index/watcher.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/index/watcher.rs)
  - Added pure event-classification helpers for changed and removed paths
- [`crates/worker/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/worker/src/main.rs)
  - Added input-line classification, request-kind classification, event-sink, and loader seams
  - Split request dispatch into build/embed/info helpers
- [`crates/core/src/embed/worker/process.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/process.rs)
  - Added command-planning and stdout-protocol parsing helpers
  - Moved stderr forwarding into a dedicated helper
- [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs)
  - Added a restore-state loading coordinator
- [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs)
  - Added a small desktop platform abstraction and exit-event helper

Still pending from the original priority list:

- [`crates/worker/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/worker/src/main.rs)
  - Still needs the requested module split into `worker/loop.rs`, `worker/handler.rs`, `worker/cache.rs`, and `worker/io.rs`
- [`crates/core/src/embed/worker/process.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/process.rs)
  - Still needs the exact `CommandSpawner`, `WorkerStdoutReader`, and `WorkerStdinWriter` trait boundaries from the spec
- [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs)
  - Still needs the explicit `IndexBuildFinalizer` seam
- [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs)
  - Still needs the `desktop/platform.rs` split
- [`crates/core/src/embed/worker/runtime.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/runtime.rs)

## Refactor Rules

These rules apply to every file below.

1. Preserve behavior exactly.
   - No user-visible CLI/API changes.
   - No changes to file layout, model selection, cache paths, event names, or HTTP routes unless explicitly called out below.

2. Do not create test-only production APIs.
   - Avoid helpers that exist only under `#[cfg(test)]`.
   - Prefer production-owned seams with small responsibilities.

3. Separate pure logic from effects.
   - Branching, normalization, planning, and classification should be moved into pure functions or small data structs.
   - Filesystem, subprocess, network, library runtime construction, and framework wiring should be pushed behind injected operations.

4. Own the abstraction boundary.
   - Do not try to mock third-party concrete constructors directly.
   - Introduce traits or helper functions owned by this crate, and call the third-party library only from the real implementation.

5. Keep state transitions observable.
   - For orchestration code, expose decision points as enums or plan structs so later tests can validate them without executing the full side effect.

6. Prefer narrow modules over giant files.
   - If a file currently mixes routing, parsing, IO, and startup, split it.

## Success Standard

The goal is not just “cleaner code.” The goal is code that makes eventual tests cheap and deterministic enough to reach `95%` to `100%` coverage in the targeted files.

For each file below, the refactor is only complete if:

- the previously effect-heavy branches are reachable without real network/model/runtime dependencies, or
- the remaining effectful code is reduced to thin glue that can be covered by a few integration tests.

## Priority Order

Implement in this order:

1. [`crates/server/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/server/src/main.rs)
2. [`crates/core/src/embed/engines/candle.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/candle.rs)
3. [`crates/core/src/embed/engines/fastembed.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/fastembed.rs)
4. [`crates/core/src/embed/index/watcher.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/index/watcher.rs)
5. [`crates/worker/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/worker/src/main.rs)
6. [`crates/core/src/embed/worker/process.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/process.rs)
7. [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs)
8. [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs)
9. [`crates/core/src/embed/worker/runtime.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/runtime.rs)

## 1. Server Entrypoint And Handlers

Target: [`crates/server/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/server/src/main.rs)

### Why It Is Still Hard To Test

This file currently combines:

- path confinement
- upload normalization
- asset authorization
- SSE loop behavior
- HTTP handlers
- configuration parsing
- router construction
- startup side effects

That shape forces tests to hit large chunks of unrelated behavior together.

### Required Refactor

Split the file into these modules:

- `crates/server/src/http/state.rs`
- `crates/server/src/http/errors.rs`
- `crates/server/src/http/search.rs`
- `crates/server/src/http/uploads.rs`
- `crates/server/src/http/assets.rs`
- `crates/server/src/http/embed.rs`
- `crates/server/src/http/worker.rs`
- `crates/server/src/http/router.rs`
- `crates/server/src/config.rs`
- keep `main.rs` as thin startup wiring

### Required Seams

#### A. Path And Upload Planning

Extract pure helpers:

- `sanitize_relative_upload_path(raw: &str) -> PathBuf`
- `validate_delete_target(rel: &Path) -> Result<DeleteTarget, ServerError>`
- `asset_access_plan(path: &Path, uploads_dir: &Path) -> Result<AuthorizedAsset, ServerError>`
- `confined_root_for_search(raw: &str, uploads_dir: &Path) -> Result<PathBuf, ServerError>`

Introduce small structs:

- `DeleteTarget { canonical: PathBuf, kind: DeleteKind }`
- `AuthorizedAsset { canonical: PathBuf, content_type: &'static str }`
- `UploadWritePlan { dest: PathBuf, create_parent: Option<PathBuf> }`

The handlers should stop embedding path-sanitization logic inline.

#### B. Filesystem Operations

Add a small server-owned trait:

```rust
trait ServerFs {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    async fn remove_file(&self, path: &Path) -> io::Result<()>;
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    async fn dir_size(&self, path: &Path) -> anyhow::Result<u64>;
}
```

The real implementation should delegate to `tokio::fs`.

Purpose:

- upload overflow branch
- asset not found / outside uploads
- delete file vs directory
- recreate uploads root
- canonicalization failure branches

all become directly testable without spinning a whole router.

#### C. SSE Event Forwarding

Extract the `embed_events_handler` loop into a reusable coordinator:

- `enum EventStreamAction`
  - `SendKeepalive`
  - `SendEvent { name: String, payload: String }`
  - `DropLagged`
  - `Stop`

- `fn next_embed_stream_action(...) -> EventStreamAction`

Also add:

- `async fn run_embed_event_forwarder<S: EventSink>(...)`

where `EventSink` is a tiny abstraction over the output `mpsc::Sender<Result<Event, Infallible>>`.

This allows tests to cover:

- keepalive path
- lagged event path
- closed broadcast path
- output send failure path

without standing up a live SSE response.

#### D. Search Streaming

Extract the search handler’s spawned task into:

- `async fn forward_search_results(...)`

with a small output sink trait for:

- `error`
- `result`
- `complete`

Tests should later be able to drive:

- `start_search` failure
- serialization failure
- result forwarding stop
- completion emission

#### E. Config Parsing

Replace `parse_config()` with:

- `struct RawConfigInputs`
- `fn parse_config_from(args: &[String], env: &dyn ConfigEnv) -> Config`

Where:

```rust
trait ConfigEnv {
    fn var(&self, key: &str) -> Option<String>;
}
```

This should cover all CLI/env precedence branches without touching process-global env.

#### F. Router Construction

Extract router creation into:

- `fn build_router(state: Arc<AppState>, dist_dir: PathBuf) -> Router`

Keep `main()` responsible only for:

- logging init
- config load
- filesystem prep
- context creation
- router build
- listener bind
- serve call

### Non-Goals

- Do not change routes.
- Do not change status codes or JSON error shapes.

## 2. Candle Loader And Installer

Target: [`crates/core/src/embed/engines/candle.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/candle.rs)

### Why It Is Still Hard To Test

This file mixes:

- HF cache inspection
- config parsing
- tokenizer loading
- weight mmap creation
- model-type dispatch
- runtime model construction
- install/download logic

The remaining misses are mostly not pure logic misses. They are “cannot reach branch without real Candle runtime and model artifacts” misses.

### Required Refactor

Split into these focused modules:

- `candle/catalog.rs`
- `candle/cache.rs`
- `candle/config.rs`
- `candle/runtime.rs`
- `candle/install.rs`
- `candle/embedder.rs`

Keep `candle.rs` as a façade if desired.

### Required Seams

#### A. Artifact Resolution

Extract:

- `struct CandleArtifacts { config_path, tokenizer_path, weights_path, pooling_config_path }`
- `fn resolve_cached_artifacts(...) -> anyhow::Result<CandleArtifacts>`
- `fn read_config_text(path: &Path) -> anyhow::Result<String>`
- `fn parse_model_type(config_text: &str) -> anyhow::Result<ModelTypePeek>`
- `fn parse_dimension_from_config(config_text: &str) -> anyhow::Result<usize>`

These should be fully pure or filesystem-only and later testable with temp files.

#### B. Runtime Factory Boundary

Introduce a crate-owned factory trait:

```rust
trait CandleRuntimeFactory {
    fn load_var_builder(&self, weights_path: &Path, dtype: DType, device: &Device) -> anyhow::Result<CandleVarBuilder>;
    fn build_loaded_model(&self, model_type: &str, config_text: &str, vb: CandleVarBuilder) -> anyhow::Result<(LoadedModel, usize)>;
    fn load_tokenizer(&self, tokenizer_path: &Path) -> anyhow::Result<Tokenizer>;
}
```

Where `CandleVarBuilder` is a local wrapper or type alias owned by the module if possible.

The real implementation is the only place allowed to call:

- `VarBuilder::from_mmaped_safetensors`
- `BertModel::load`
- `JinaBertModel::new`
- `ModernBert::load`
- `Tokenizer::from_file`

Purpose:

- future tests can force weight-load failure, config-parse mismatch, tokenizer failure, unsupported model type handling, and successful dispatch without real model files.

#### C. Device And Provider Planning

Extract:

- `enum CandleDevicePlan { Cpu, MetalPreferred }`
- `fn select_device_plan(device: &str) -> CandleDevicePlan`
- `fn select_dtype_for_plan(plan: &CandleDevicePlan) -> DType`

Then have a small real executor:

- `fn realize_device(plan: CandleDevicePlan) -> Device`

This keeps the fallback logic testable without requiring real Metal availability.

#### D. Installer Download Boundary

Introduce:

```rust
trait HfModelFetcher {
    fn download_required_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()>;
    fn fetch_optional_files(&self, model_id: &str, files: &[&str]) -> anyhow::Result<()>;
}
```

The real implementation should own the `ApiBuilder` and `repo.get()` calls.

Refactor `install()` to:

- emit initial progress
- call a helper that computes the required and optional download plan
- invoke the fetcher
- invoke aux config fetch
- emit final progress

All branching around download failures, optional pooling config, and aux config should become easy to test later.

#### E. Embedder Construction Plan

Extract:

- `struct CandleEmbedderBuildPlan`
  - model id
  - dimension
  - pooling
  - query prefix
  - passage prefix
  - device plan
  - dtype

- `fn build_embedder_plan(...) -> anyhow::Result<CandleEmbedderBuildPlan>`

Then a tiny finalizer:

- `fn assemble_candle_embedder(plan, model, tokenizer, device) -> CandleEmbedder`

### Non-Goals

- Do not change model catalog content.
- Do not change `MODEL_FILES`.
- Do not change embedding behavior.

## 3. FastEmbed Loader And Installer

Target: [`crates/core/src/embed/engines/fastembed.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/fastembed.rs)

### Why It Is Still Hard To Test

The remaining misses are concentrated in:

- provider selection
- `TextInitOptions` construction
- `TextEmbedding::try_new`
- install-vs-cached logic
- HF sibling filtering

### Required Refactor

Split into:

- `fastembed/catalog.rs`
- `fastembed/cache.rs`
- `fastembed/runtime.rs`
- `fastembed/install.rs`

### Required Seams

#### A. Catalog Boundary

Wrap `TextEmbedding::list_supported_models()` behind:

```rust
trait FastembedCatalog {
    fn supported_models(&self) -> Vec<FastembedModelRecord>;
}
```

with a local data type:

- `FastembedModelRecord`
  - model enum id string
  - model code
  - model file
  - additional files
  - description
  - dimension

Everything else in the file should operate on `FastembedModelRecord`, not directly on the third-party type.

#### B. Provider Planning

Extract:

- `enum FastembedExecutionPlan`
  - `CpuOnly`
  - `CoreMlThenCpu`

- `fn execution_plan_for_device(device: &str) -> FastembedExecutionPlan`

- `fn build_text_init_request(...) -> FastembedInitRequest`

where `FastembedInitRequest` is a local struct containing:

- model record
- cache dir
- show progress flag
- execution plan

The real adapter should map `FastembedInitRequest` into `TextInitOptions`.

#### C. Runtime Factory

Introduce:

```rust
trait FastembedRuntimeFactory {
    fn try_new(&self, request: FastembedInitRequest) -> anyhow::Result<TextEmbedding>;
}
```

This should isolate all direct `TextEmbedding::try_new` calls.

#### D. Installer Download Planning

Refactor `install()` to:

- resolve model info from catalog
- check cache state via a helper
- emit progress start
- build init request
- call runtime factory only when needed
- fetch aux config
- emit progress done

The “already cached, skip runtime init” branch must remain behaviorally identical.

#### E. HF Model Size Logic

Refactor:

- `fetch_model_size`

into:

- `fn relevant_hf_filenames(record: &FastembedModelRecord) -> HashSet<String>`
- `fn hf_sibling_matches_relevant(...) -> bool`
- `fn sum_matching_hf_sizes(...) -> anyhow::Result<u64>`

This should make all path suffix matching and empty-result behavior fully pure.

### Non-Goals

- Do not change which execution providers are used.
- Do not change the quantized batch-size behavior.

## 4. Index Watcher

Target: [`crates/core/src/embed/index/watcher.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/index/watcher.rs)

### Why It Is Still Hard To Test

The file still bundles:

- watcher creation
- event-batch filtering
- changed/removed path classification
- callback triggering
- file update/removal application
- retry-open logic

### Required Refactor

Split into:

- `watcher/start.rs`
- `watcher/classify.rs`
- `watcher/apply.rs`

### Required Seams

#### A. Event Classification

Extract pure helpers:

- `fn should_consider_path(path: &Path, supported_extensions: &[String]) -> bool`
- `fn classify_event_paths(events: &[DebouncedEvent], supported_extensions: &[String]) -> ClassifiedPaths`

with:

- `struct ClassifiedPaths { changed: Vec<PathBuf>, removed: Vec<PathBuf> }`

These later tests should drive:

- ignored extension with existing file
- removed path
- file path vs directory path
- mixed event batches

#### B. Index Mutation Boundary

Introduce:

```rust
trait IndexMutation {
    fn remove_file(&mut self, path: &Path) -> anyhow::Result<()>;
    fn write_prepared_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()>;
}
```

and a helper that adapts `SemanticIndex`.

Then extract:

- `fn apply_removed_paths(...)`
- `fn apply_changed_paths(...)`

#### C. Preparation Boundary

Wrap `SemanticIndex::prepare_file` in a local trait:

```rust
trait FilePreparer {
    fn prepare(...) -> anyhow::Result<PreparedFile>;
}
```

This allows tests to cover:

- open retry failure
- prepare failure
- write failure
- remove failure

without requiring a full live index.

#### D. Watcher Backend Boundary

Add:

```rust
trait WatchBackendFactory {
    fn create(&self, root: &Path, tx: Sender<DebounceEventResult>) -> anyhow::Result<WatchBackendHandle>;
}
```

This is only for making startup/watch failure deterministic. The thread loop should consume an abstract receiver, not construct the debouncer inline.

### Non-Goals

- Do not change debounce timing.
- Do not change callback ordering (`on_reindex` before changes, `on_reindex_done` after).

## 5. Worker Entrypoint

Target: [`crates/worker/src/main.rs`](/Users/leonrjg/claude/Wilkes/crates/worker/src/main.rs)

### Why It Is Still Hard To Test

The file mixes:

- stdin line loop
- request parse behavior
- event emission
- embedder cache invalidation
- request-mode dispatch
- progress forwarding

### Required Refactor

Split into:

- `worker/loop.rs`
- `worker/handler.rs`
- `worker/cache.rs`
- `worker/io.rs`

### Required Seams

#### A. Input Loop

Extract:

- `enum WorkerLoopAction`
  - `Stop`
  - `ParseError(String)`
  - `Dispatch(WorkerRequest)`

- `fn classify_input_line(line: &str) -> WorkerLoopAction`

This should cover empty-line stop and malformed JSON without driving a full process main loop.

#### B. Event Sink

Wrap `emit()` behind:

```rust
trait WorkerEventSink {
    fn emit(&self, event: WorkerEvent);
}
```

The main loop should no longer call `println!` directly.

#### C. Embedder Loader Boundary

Wrap `dispatch::load_embedder_local` behind:

```rust
trait LocalEmbedderLoader {
    fn load(&self, key: &LoadedEmbedderKey) -> anyhow::Result<Arc<dyn Embedder>>;
}
```

Then refactor `get_or_load_embedder` into:

- cache key comparison
- invalidation decision helper
- load-and-store helper

#### D. Request Handling Plan

Replace the monolithic `match req.mode.as_str()` body with:

- `enum WorkerRequestKind { Build(BuildPlan), Embed(EmbedPlan), Info(InfoPlan), Unknown(String) }`
- `fn classify_worker_request(req: &WorkerRequest) -> anyhow::Result<WorkerRequestKind>`

Then implement:

- `handle_build_plan`
- `handle_embed_plan`
- `handle_info_plan`

The build branch’s progress forwarder should become a reusable helper.

### Non-Goals

- Do not change worker protocol.
- Do not change emitted event ordering.

## 6. Worker Process

Target: [`crates/core/src/embed/worker/process.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/process.rs)

### Why It Is Still Hard To Test

The remaining misses are around:

- spawn command creation
- stderr logging task setup
- stdin write failure
- stdout protocol read loop
- non-protocol lines
- read failure behavior

### Required Refactor

Introduce these boundaries:

- `ProcessCommandPlan`
- `trait CommandSpawner`
- `trait WorkerStdoutReader`
- `trait WorkerStdinWriter`

### Required Seams

#### A. Command Planning

Extract:

- `fn build_command_plan(paths: &WorkerPaths, req: &WorkerRequest) -> Result<ProcessCommandPlan, String>`

with all environment-variable decisions represented as plain data.

Then add:

- `fn apply_command_plan(plan: &ProcessCommandPlan) -> Command`

This allows later tests to cover SBERT env population and worker-bin selection without spawning a subprocess.

#### B. Protocol Reader

Extract:

- `enum ProtocolReadOutcome`
  - `Emit(WorkerEvent)`
  - `IgnoreNonProtocolLine`
  - `ClosedStdout`
  - `ReadError(String)`

- `fn parse_worker_stdout_line(line: &str) -> ProtocolReadOutcome`

Then the read loop becomes a thin executor around the helper.

#### C. Stderr Forwarder

Move the stderr task into:

- `spawn_stderr_forwarder(stderr)`

This lets tests ignore it or replace it cleanly.

## 7. Remaining AppContext Orchestration

Target: [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs)

### Why It Is Still Still Hard To Finish

Most remaining misses are now concentrated in real-effect branches:

- watcher startup success/error
- built-index open/store/finalize flow
- spawned build/download task bodies
- restore-state success path with real install/open/watcher work

### Required Refactor

Do **not** add more giant helpers. Add a few final narrow seams:

#### A. Build Finalization Executor

Add a finalization trait:

```rust
trait IndexBuildFinalizer {
    async fn open_index(... ) -> Result<SemanticIndex, String>;
    fn start_watcher(...);
    async fn persist_semantic_settings(...);
    fn emit_done(...);
}
```

The existing `finish_build_index` should delegate through this boundary.

#### B. Restore Coordinator

Introduce:

- `struct RestoreLoadedState`
  - embedder
  - index
  - maybe watcher root
  - selected
  - db status

- `async fn load_restore_state(...) -> Option<RestoreLoadedState>`

So later tests can exercise “load failed at stage N” without calling the whole `restore_state()` body.

#### C. Download Probe Boundary

The newly added installer seam should remain and be used consistently for the download probe path.

### Non-Goals

- Do not change event names or task semantics.

## 8. Remaining Desktop Tauri Wiring

Target: [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs)

### Why It Is Still Hard To Finish

Most remaining misses are framework wiring:

- `AppHandle` state lookup
- OS opener command spawn
- Tauri emitter impl
- `run()` setup and shutdown closures

### Required Refactor

Extract a tiny platform layer:

- `desktop/platform.rs`

with:

```rust
trait DesktopPlatform {
    fn app_config_dir(&self) -> anyhow::Result<PathBuf>;
    fn app_data_dir(&self) -> anyhow::Result<PathBuf>;
    fn emit(&self, name: &str, payload: serde_json::Value);
    fn open_path(&self, path: &Path) -> Result<(), String>;
}
```

Also extract:

- `fn build_app_context(...) -> anyhow::Result<(Arc<AppContext>, Arc<ActiveSearches>, ...)>`
- `fn handle_exit_event(...)`

The goal is not to mock Tauri deeply. The goal is to make `run()` a tiny adapter over helpers that can later be tested with a fake platform.

## 9. Runtime Final Sweep

Target: [`crates/core/src/embed/worker/runtime.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/runtime.rs)

### Why It Is Still Not Fully Covered

Only a few paths remain:

- supervised loop restart/cancel edges
- submit serialization error
- no-op branches

### Required Refactor

Only if needed after higher-value work:

- extract `serialize_request_for_worker`
- extract `should_restart_worker`
- extract `restart_runtime_after_panic`

This file is already close enough that it should not take priority over the bigger misses above.

## Suggested Implementation Order By Agent

If multiple agents work in parallel, split ownership like this:

1. Agent A
   - `server/src/main.rs`
   - new `server/http/*` modules

2. Agent B
   - `embed/engines/candle.rs`
   - supporting `candle/*` modules

3. Agent C
   - `embed/engines/fastembed.rs`
   - supporting `fastembed/*` modules

4. Agent D
   - `embed/index/watcher.rs`
   - `worker/src/main.rs`

5. Agent E
   - `embed/worker/process.rs`
   - finish remaining `context.rs` seams
   - final `desktop/src/lib.rs` platform split

## Acceptance Criteria For The Refactor Pass

This refactor spec is satisfied only when:

- every file above has explicit owned seams around the currently concrete external calls
- path normalization / configuration / event classification logic is pure or nearly pure
- framework code is reduced to thin adapters
- startup and orchestration functions mostly delegate to extracted helpers
- later tests can cover error and success branches without:
  - real model downloads
  - real Candle/FastEmbed runtime construction
  - live OS subprocess dependence where not essential
  - full Tauri app startup

## Explicit Non-Goals

- Do not rewrite the architecture wholesale.
- Do not replace Candle, FastEmbed, Axum, or Tauri.
- Do not add behavioral feature changes during the refactor pass.
- Do not optimize for elegance at the expense of introducing new risk.

The desired outcome is boring, explicit, testable code.
