# TODO

## Narrow the metadata-cache and research-store locks in the catalog phase

**Status:** open — identified 2026-08-22 while profiling concurrent MCP search calls.

### What is wrong

`ContextState::list_files_filtered_with_ignore` takes two process-wide blocking
locks and holds each one across a loop over *every file in the root*:

- `crates/api/src/context.rs` — `cache.lock()` is held while iterating
  `response.files` and doing a `get_valid_with_primary` lookup per entry.
- `crates/api/src/context.rs` — `store.lock()` is held across
  `enrich_files(&mut response.files)` and the `eligible_paths` calls.

Both are `std::sync::Mutex`, taken inside an `async fn`. So they do two things
at once: they serialise concurrent searches against each other, and they block
the tokio worker thread they run on rather than yielding it.

### Evidence

Single-file searches (scan work ≈ 0, which isolates the catalog phase) against
the live MCP server, measuring `catalog_elapsed_ms` as concurrency rises:

| concurrency | 1 | 2 | 3 | 6 | 8 |
|---|---|---|---|---|---|
| catalog time | 1.0x | 1.3x | 1.4x | 1.9x | 2.6x |

Contention is real but sub-linear, so this is a drag rather than a hard
serialiser. It is why three concurrent low-CPU calls reached only a 1.66x
speedup instead of the ~3x the workload allows.

### Why it was not fixed in the same pass

The two changes shipped alongside this (single-path catalog resolution, and
corpus-scan admission control) are contained within the search path. Narrowing
these locks reaches into `MetadataCache` and `ResearchStore` ownership and
changes who may hold what for how long, so it deserves its own change with its
own tests rather than riding along.

Note that the single-path fix reduces the *exposure* — a `File`-scope query now
enriches a one-entry listing instead of the whole root — but it does not change
the locking itself. Any full-root listing (the file browser, collection
evaluation, indexing) still takes both locks for the whole traversal.

### Direction

The invariant to restore: **a lock is held for the duration of one lookup, not
for the duration of a traversal.**

- Collect the identities to look up, take the lock once to batch-resolve them,
  release, then apply the results — or give `MetadataCache` a batch-lookup entry
  point that owns its own locking.
- Same treatment for `ResearchStore::enrich_files` / `eligible_paths`.
- If a lock must be held across real work, move that work to
  `spawn_blocking` so it stops occupying an async worker, or switch the
  structure to a read-mostly one (`RwLock`, or a snapshot the readers clone).

Re-run the concurrency measurement above afterwards: catalog time should stay
approximately flat as concurrency rises instead of reaching 2.6x at 8-way.
