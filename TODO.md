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
the live MCP server, measuring `catalog_elapsed_ms` as concurrency rises.

Measured *after* the single-path catalog fix landed — use this as the baseline:

| concurrency | 1 | 2 | 3 | 6 | 8 |
|---|---|---|---|---|---|
| catalog time | 3.0ms (1.0x) | 3.5ms (1.2x) | 5.0ms (1.7x) | 14.0ms (4.7x) | 13.5ms (4.5x) |

The original reading, before that fix, was 5.0ms growing to 13.0ms (2.6x). The
absolute cost at low concurrency dropped because a one-file query no longer
walks the root, but the ceiling under load did not move — so the contention is
*more* visible now, not less. A single-entry listing still takes both
process-wide locks; only the work done while holding them got smaller.

Contention is real but sub-linear, so this is a drag rather than a hard
serialiser. It is why three concurrent low-CPU calls reached only a 1.66x
speedup instead of the ~3x the workload allows.

### Why it was not fixed in the same pass

The single-path catalog resolution shipped separately and is contained within
the search path. Narrowing these locks reaches into `MetadataCache` and
`ResearchStore` ownership and changes who may hold what for how long, so it
deserves its own change with its own tests rather than riding along.

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

## Findings from an embedding experiment against the managed-corpus API (2026-11-05)

**Status:** open — met while eos compared ReasonIR-8B and OBLIQ-IR-3B against
Jina v5 for prerequisite retrieval, on the Mac mini instance (`192.168.1.8:2020`,
`~/Desktop/Wilkes` at `5477032d`). The corpus was 6,032 one-passage text
documents (owner `eos-retriever-experiment`, corpus `c1d478b6-…`). Each item
below cost time in the run; the causes of the first two are not established,
only the symptoms.

### 1. A ReasonIR-8B projection build started nothing and reported nothing

`PUT /api/corpora/spaces` with `{"engine":"SBERT","model":"reasonir/ReasonIR-8B","dimension":4096}`
created the projection workspace (`4d554b4f-…`, `workspace.json` and an empty
`managed_sources/` at 18:45) and then did nothing visible for 12 minutes: no
`semantic_index.db`, no Python worker for the model's environment (`2519f877…`,
which the download had already created), the base worker idle. The request
was still open when the client gave up (`http-requests`: cancelled after
705 s); the workspace is still empty an hour later. Status reported the space
neither as building nor as failed.

The weights were fully cached by then, and the same environment loads the
model in 16–18 s and embeds these documents at ~3.5/s when driven directly
(`SentenceTransformer(..., trust_remote_code=True, torch_dtype="auto")` on MPS),
so the model itself is not the obstacle. Jina searches against the same corpus
were in flight throughout (item 3) — whether they held something the catch-up
waits on is a hypothesis to check, not a finding.

**Direction:** a projection catch-up needs observable progress (documents
embedded / required, the worker it is waiting for) and a terminal state when it
stops, and should not depend on the HTTP request staying open.

### 2. A projection build during a model download stopped half way, silently

The first `PUT /api/corpora/spaces` (Jina v5, cached) ran while the ReasonIR
download held the embed task. It returned after the client's 300 s timeout
with the space `ready: false`; the index stopped growing at 18:35 (5.4 MB of
an eventual ~40 MB) and nothing resumed it. A second identical PUT, after the
download finished, caught up in 379 s and reported `ready: true`.

**Direction:** either queue the catch-up behind the download or report that it
is blocked; a space that is not ready and not progressing should say why.

### 3. Managed search takes ~8 s per query on a 6,032-passage projection

`POST /api/chunks/search`, one text probe, `top_k` 51, two requests in flight:
median 8,445 ms over 301 searches. A `sample` of `wilkes-desktop` during the
run had the busy thread in `SemanticIndex::managed_chunk_search`; the Python
worker (which embeds the probe) was idle. 6,032 × 1,024 floats is a few
milliseconds of dot products, so the time is going somewhere else in that
function.

### 4. The managed-corpus contract misleads a consumer building its first corpus

Three things a first-time caller cannot learn from the replies:

- `PUT /api/corpora` takes an `embedding`, but the canonical corpus holds no
  vectors by design; status says `ready: true` with `spaces: []`, and search
  fails with `EMBEDDING_SPACE_MISMATCH … holds no embedding space` until a
  `PUT /api/corpora/spaces` is made. Nothing in the replies points there.
- `POST /api/corpora/documents/import` returns an `embedding_space_id`, and
  sending that id back as `expected_embedding_space_id` on the next import is
  refused (`corpus=none, request=space-…`, 409). Omitting it works only while
  the corpus has no projection.

**Direction:** stop returning a space id from import on the canonical corpus,
or accept it; and have the corpus reply say that search needs a projection.

### 5. Query-role vectors are reachable only through a projection search

`/api/embed/text` answers in the passage role only (by design, since its
vectors are stored), and the query role is reached only as a side effect of
`/api/chunks/search`. So scoring queries against anything that is not already a
Wilkes projection — or embedding with a model no index uses — means building a
whole projection first. For an 8B model on this machine that is ~30 minutes
for 6,000 short texts, which is what pushed the experiment to drive the
model's Python environment directly instead.

**Direction:** an endpoint that embeds text in a named role with a named
model, without a corpus, returning vectors that are explicitly not for storage.
