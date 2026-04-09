# Coverage Refactor Plan

Current state:

- Tarpaulin coverage is now around `73.51%` overall.
- The refactor pass has already added exercised lines in the desktop, API context, worker runtime, and Hugging Face metadata paths.
- `crates/api/src/context.rs` is now at `301/458` covered lines in the latest workspace run.
- The remaining work should stay focused on the last high-yield branches, not on broad cleanup.

## Goal

Increase coverage on the hardest-to-test areas while keeping production behavior unchanged.

The remaining refactors should do one of two things:

1. Expose branch-heavy logic behind pure helpers or injected interfaces.
2. Remove direct dependencies on Tauri, live processes, or network-bound model metadata so tests can drive the code directly.

## Completed So Far

- Desktop command wiring has been split into small `*_for_ctx` helpers where it was most useful, including search orchestration and worker-status wrappers.
- `list_models`, `get_model_size`, `get_index_status`, and `delete_index` now have direct test coverage through the desktop seam.
- `AppContext` restore-state logic now has extracted helpers for stale-selection detection, indexing config, enabled-settings shaping, index opening, and watcher startup.
- `AppContext` embed-task orchestration now has helper seams for build/download preparation and task spawning, which unlocked direct tests for task-state guards and root validation.
- Worker runtime failure-path coverage now includes spawn failure, send failure, channel-close cleanup, and idle-timeout cleanup.
- Hugging Face metadata fetching now has injectable seams for request, response parsing, and model-size aggregation.
- The corresponding tests were added alongside each seam, and Tarpaulin now reports the higher covered-line count above.

## Remaining Refactors

### 1. Finish the desktop command split

Target: [`crates/desktop/src/lib.rs`](/Users/leonrjg/claude/Wilkes/crates/desktop/src/lib.rs)

What is already done:

- The search command now routes through a testable helper.
- `list_models`, `get_model_size`, `get_index_status`, `delete_index`, and worker status updates now have direct helper tests.
- `search_for_ctx`, `validate_open_path`, and the small settings/file wrappers are already extracted.

What still matters:

- Any remaining thin wrappers that still only delegate through `AppHandle` can still be split if they unlock a real branch or error path.

Expected payoff:

- Modest but reliable line gain.
- Good for branch coverage in wrappers and command wiring.

### 2. Split `AppContext` orchestration into testable pieces

Target: [`crates/api/src/context.rs`](/Users/leonrjg/claude/Wilkes/crates/api/src/context.rs)

What is already done:

- `restore_state_needs_reset` was extracted.
- The restore-state path now has helpers for indexing config, enabled settings, index opening, and watcher startup.
- Tests now cover the helper branches and the error paths that the seams expose.
- Build/download task preparation now has seams for running-task detection and root validation.
- Added direct tests for those new seams, which improved coverage in `context.rs` but only modestly at the workspace level.

What still matters:

- `spawn_build_index_task` and `spawn_download_model_task` still contain the dense side-effect branches.
- `restore_state` still owns the long install/build/open/watcher sequence and is the biggest remaining branch cluster.
- The doc’s broader request for narrower settings I/O seams is still open if coverage stalls again.

Expected payoff:

- High.
- This is the best remaining route for meaningful coverage growth because the file has many branches that are currently only reachable through real I/O and startup paths.

### 3. Expand worker runtime injection to cover failure branches

Target: [`crates/core/src/embed/worker/runtime.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/worker/runtime.rs)

What is already done:

- The spawner/session seam already covers the happy path and hot-swap behavior.
- Added tests now cover spawn failure, send failure, channel-close cleanup, and idle-timeout cleanup.

What still matters:

- `stdout` close while reading events is still a doc target if we want the runtime coverage to be more complete.
- Manager loop panic recovery is still open.
- Worker kill behavior can still be tightened if a future pass needs more runtime branch coverage.

Expected payoff:

- Medium to high.
- Good coverage gain because the runtime has several branch-heavy transitions that are now partially testable.

### 4. Make Hugging Face model metadata injectable

Target: [`crates/core/src/embed/models/hf_hub.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/models/hf_hub.rs)

What is already done:

- `fetch_hf_siblings` now goes through injectable request and parse helpers.
- `fetch_model_size` has an injectable aggregation helper.
- Tests now cover request failure, parse failure, successful parsing, size aggregation, and empty-result handling.

What still matters:

- `is_model_cached` and cache-path logic remain as they were, which is fine for now.
- No additional HF refactor is currently required unless coverage stalls again.

Expected payoff:

- Medium.
- This helps stabilize coverage for code that is otherwise hard to exercise consistently.

### 5. Defer model-loader refactors unless coverage stalls again

Targets:

- [`crates/core/src/embed/engines/candle.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/candle.rs)
- [`crates/core/src/embed/engines/fastembed.rs`](/Users/leonrjg/claude/Wilkes/crates/core/src/embed/engines/fastembed.rs)

Why this is lower priority:

- The easy cache and catalog branches are already covered.
- The remaining misses are mostly runtime loading, inference setup, or dependency-heavy paths.
- These are expensive to refactor and less likely to produce a good coverage return right away.

Necessary changes only if needed later:

- Separate catalog/metadata parsing from model initialization.
- Add a narrow seam around runtime construction so failure branches can be tested without loading real models.

Expected payoff:

- Low to medium.
- Only worth it if the higher-value seams above stop moving the overall number.

## Recommended Order

1. Split the remaining `AppContext` orchestration logic.
2. Add the missing runtime failure-path coverage in worker runtime.
3. Finish any leftover desktop wrapper extraction only if it unlocks a real branch.
4. Leave Candle and FastEmbed for last.

## Success Criteria

The refactor pass is successful only if it increases exercised lines, not just code cleanliness.

For each refactor:

- Add a focused test in the same change.
- Verify the affected crate with `cargo test`.
- Re-run Tarpaulin and compare absolute covered lines, not just the percentage.
- Compare against the full workspace baseline, not a single crate.

If a refactor makes the code cleaner but does not unlock new exercised lines, it should be reconsidered.
