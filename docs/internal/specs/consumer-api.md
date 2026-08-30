# Consumer API

Status: specified, not yet implemented.

Supersedes `managed-semantic-corpus-api.md`, which described the same
guarantees as an adapter for one named consumer. That document is withdrawn;
where a rule below is unchanged from it, the rule is unchanged, not merely
restated.

This spec governs every route a programmatic consumer calls: the corpus
lifecycle, chunk addressing, embedding, export, the catalogue mirror, and the
embedder capability manifest. It does not govern the browser UI's own routes
(search, files, bookmarks, tags, generation), except where a route named here
is one the UI also calls.

## The invariant

**One vocabulary for one question.** Wilkes currently answers "what is in this
passage", "how close is this vector to that passage", and "what does this
corpus embed with" twice: once on a generic route keyed by SQLite rowid and an
engine/model/dimension tuple, and once on a managed route keyed by `chunk_ref`
and `embedding_space_id`. The two are not different contracts for different
audiences. Neither generic route has an in-repo caller: the UI, the desktop
shell, and the MCP agent call none of `/api/embed/text`, `/api/embed/centroid`,
`/api/embed/similarity`, or `/api/export/*`. Both halves serve sidecar
consumers, and the managed half is the superset in every pair.

So the absorption is not a merge of two designs. It is the deletion of the
weaker one.

Two concrete rules follow, and both are mechanically checkable:

1. **No request type accepts a SQLite rowid.** After this change, `grep` for
   `i64` in a deserialized field of a chunk-addressing request returns nothing.
   A rowid is a position in one build of one index; a consumer that persists
   one has stored a pointer that a rebuild silently repoints. `chunk_ref` is
   derived from the rendition and the ordinal, so it survives a rebuild or
   refuses.

   Rowids may still appear *within* a response as a correlation key — the topic
   and bookmark-cluster payloads name a representative chunk that is also in
   the same payload. That is an intra-response identifier, not an address the
   caller redeems on another route later, and it stays.

2. **No route path names a consumer.** `underdog` is one application's name
   compiled into route paths, into 30-odd function names, and into five string
   comparisons against a manifest field. The concept those name is a managed
   semantic corpus, which is a thing Wilkes offers, not a thing Underdog is.

## What is absorbed, and what is not

Sixteen routes live under `/api/integrations/underdog/`. They divide three
ways, and the division is not by how they were built but by what they are.

**Unscoped services (6 routes).** `catalogue/search`, `catalogue/sync`,
`catalogue/status`, `catalogue/acquire`, and `embed/models`. The code already
argues this against itself: the catalogue routes deliberately bypass
`managed_context` because there is no corpus to pin and pinning one "would be
theatre", and the capability manifest is documented as unscoped for the same
reason. A mirror of public textbook catalogues, and a description of the
embedders on this machine, have nothing to do with any particular consumer.
They move to `/api/catalogue/*` and `/api/embed/capabilities`.

**Duplicated chunk operations (4 routes).** `chunks/resolve`,
`chunks/accumulate`, `chunks/similarity`, `embed/text`. Each has a generic twin
that answers the same question in the weaker vocabulary. The managed form wins
in every case and the twin is deleted.

**The corpus contract (6 routes).** `workspace`, `spaces`, `documents/import`,
`status`, `backup`, `restore`. These carry write refusal, idempotency binding,
generation pinning, and projection fan-out. They are *not* duplicates of
`/api/workspaces`, and folding them into it would grow that route a mode where
it refuses writes and demands an idempotency key — the same defect pointing the
other way. They keep their contract and lose only the consumer's name in their
path, becoming `/api/corpora/*`.

One route is in none of these groups. `chunks/search` — probe-and-top-k search
returning stable refs — has no generic twin at all; `/api/search` in semantic
mode is file-oriented and streams over SSE. It is a gap in the consumer
surface, not a duplication, and it is promoted to `/api/chunks/search` with its
space pin made conditional like every other chunk route.

## Scope: how a request names an index

Every chunk, embed, and export route takes one optional `scope` object and
nothing else for addressing:

```json
{ "workspace_id": "...", "expected_embedding_space_id": "..." }
```

- `workspace_id` absent means the active workspace. This is not a filter. Chunk
  refs are per-index, so answering from whichever workspace happens to be open
  would return confident numbers about the wrong passages.
- A managed corpus is addressed by putting its `corpus_id` in `workspace_id`.
  They are already the same token — `context_for(corpus_id)` resolves today —
  and pretending otherwise was the adapter's doing, not the data model's.

One resolver replaces `WorkspaceManager::context_for` and
`underdog_space_context`, and it is the only way a consumer route opens an
index:

| Workspace kind | `expected_embedding_space_id` | Behaviour |
|---|---|---|
| `User` | absent | Open it. Responses report the space id the index carries. |
| `User` | present | Open it; `409 EMBEDDING_SPACE_MISMATCH` unless the index's own space id is equal. |
| `ApplicationManaged` | absent | `409 EMBEDDING_SPACE_MISMATCH`, unless the corpus holds no index at all. |
| `ApplicationManaged` | present | Resolve the id to the projection that owns those vectors — possibly the canonical workspace itself — and `409 EMBEDDING_SPACE_STALE` if that projection's `indexed_generation` differs from the corpus's `corpus_generation`. |

The pin is optional on a user workspace and required on a managed corpus
because on a managed corpus it *routes* as well as verifies. That is one rule
with a stated reason, not two mechanisms: in both cases a supplied pin is
honoured exactly, and in neither case is a mismatch ever served.

Import remains the single exception, and for the reason it always was: a corpus
has no coordinate system before its first vectors exist, and the id a build
will produce cannot be derived from configuration. Import may omit the pin when
the corpus holds no space; sending one then is a `409`, as is omitting one for
a corpus that has one.

## Chunk refs on ordinary indexes

The ordinary indexing path already writes stable identities. `index_file` calls
`write_file_with_recipe`, which computes `source_sha256`, `snapshot_id`,
`rendition_id`, and a per-chunk `chunk_ref` into the same nullable columns a
managed import fills. The difference between an ordinary document and an
admitted one is `admission_state`, an idempotency key, provenance, and a
retained source snapshot — not the addressing.

So `/api/chunks/*` serves user workspaces without any new writing. Two indexes
cannot serve it, and both refuse rather than degrade:

- An index migrated from before schema v10 has `chunk_ref IS NULL` on every
  row and `exact_identity` absent. It answers `409 INDEX_IDENTITY_UNVERIFIED`,
  naming a rebuild as the remedy. Its vectors remain usable by Wilkes locally,
  where the engine/model/dimension tuple matching the runtime is enough; that
  tuple is not, and never was, vector-compatibility proof for a consumer.
- A ref the index does not hold is `404 CHUNK_REF_NOT_FOUND`. Not an omission
  from the reply: a caller that named a passage which does not exist has a
  stale reference and must learn so, not receive a shorter list.

## Routes

### Corpus lifecycle — `/api/corpora`

| Method | Path | Replaces |
|---|---|---|
| PUT | `/api/corpora` | `PUT integrations/underdog/workspace` |
| PUT | `/api/corpora/spaces` | `PUT integrations/underdog/spaces` |
| GET | `/api/corpora/status?corpus_id=` | `GET integrations/underdog/status` |
| POST | `/api/corpora/documents/import` | `POST integrations/underdog/documents/import` |
| POST | `/api/corpora/backup` | `POST integrations/underdog/backup` |
| POST | `/api/corpora/restore` | `POST integrations/underdog/restore` |

Request and response shapes are unchanged except that `EnsureManagedWorkspace`
gains a required `owner` field:

```json
{ "owner": "underdog", "corpus_key": "store-018f",
  "embedding": { ... }, "chunk_size": 1000, "chunk_overlap": 100 }
```

`owner` is matched against `WorkspaceKind::ApplicationManaged { owner, .. }`
instead of the literal `"underdog"` at each of the five comparison sites, and
the created manifest's display name is derived from it rather than hardcoded.
Because the on-disk manifests already carry `owner: "underdog"`, a consumer
that sends its own name matches its existing corpus: **no manifest migration is
required.**

The corpus contract itself is unchanged by this spec. It is carried forward
below rather than left in the withdrawn document, so that one document governs.

#### Protection

A corpus token selects an application-managed workspace but cannot activate,
rename, re-root, reconfigure, or delete it. The workspace is *not* hidden from
the person using Wilkes: it is listed with `read_only: true` and its `owner`,
can be activated, and its documents open and search like any other's — they are
the user's own files. What is refused is every write: renaming the workspace,
changing its roots or semantic configuration, adding, renaming, moving or
deleting documents, and building or deleting its index. The protection lives on
those calls — `AppContext::ensure_writable`, `update_scoped_settings`, and the
refusal to watch or reindex a managed root — not on the workspace's visibility.
`/api/corpora/*` remains the corpus's only writer.

#### Identity and admission

`embedding_space_id`, `snapshot_id`, `rendition_id`, and `chunk_ref` are opaque
SHA-256-derived strings owned by Wilkes. Callers persist and echo them, never
reconstruct them. Index rowids are not present on this API — which rule 1 of
the invariant now extends to every other route in this document.

The canonical import returns only after Wilkes has:

1. copied the source into `managed_sources/<source_sha256>/`;
2. verified the source did not change during the copy;
3. extracted a rendition under the configured extraction recipe;
4. copied an existing whole rendition only when source, extraction, rendition,
   ordered chunks, and embedding space match exactly, and otherwise embedded
   the retained snapshot; and
5. committed every stable chunk reference and vector in one transaction with
   `admission_state = ready`.

Admission then projects the canonical extracted text and exact chunk
descriptors into each additional embedding space. A projection never copies the
source or repeats extraction or chunking; it computes only its own vectors and
index rows.

Fan-out is attempted on admission and reported per space, but does not decide
admission. The canonical corpus is the membership authority, so a model that is
unavailable or slow leaves its own projection behind rather than refusing the
document. Catching up is idempotent — every admitted snapshot is offered under
a content-derived key, so a projection is only ever missing work, never holding
half of it — and `PUT /api/corpora/spaces` performs it for one space, as does
the next import for all of them. A projection whose membership digest differs
from the canonical corpus cannot serve until it has.

Identical source bytes reuse the retained snapshot. Managed snapshots and ready
index rows are not automatically collected.

Semantic-index schema v10 stores `IndexEmbeddingMetadata`: the historical
engine/model/dimension tuple plus an optional `exact_identity`. Managed
admission never treats that tuple as vector-compatibility proof. Import from a
legacy workspace is a normal reuse miss: Wilkes retains the source snapshot and
embeds it in the protected workspace without rebuilding or modifying the source
index.

`idempotency_key` is durably bound to the admitted source, extraction recipe,
and rendition. Repeating the job returns that ready document; reusing the key
for different bytes or a different recipe is refused.

#### Import source

Either an explicitly selected local path:

```json
{ "kind": "path", "path": "/selected/paper.pdf" }
```

or a file selected through an existing Wilkes library, which must name both its
workspace and authorized library root. Wilkes canonicalizes both and refuses
paths outside that workspace's configured library:

```json
{ "kind": "wilkes_file", "workspace_id": "workspace-id",
  "root": "/library", "path": "/library/paper.pdf" }
```

#### Import response

Carries `corpus_id`, source bytes and media metadata, snapshot and rendition
identities, `extracted_content_sha256`, exact embedding space metadata,
resolved outline entries, `extraction` diagnostics, stable chunks, and
`embedding_work`, where `chunk_count == embedding_work.reused +
embedding_work.computed`. Raw vectors, source-workspace identities, and SQLite
rowids are absent. Every returned text hash and the extracted-content hash are
recomputable by the caller before it records the document.

Each outline entry carries `byte_offset` where Wilkes could establish one and
an `anchor` naming what established it: `destination_coordinate` (the PDF
destination's own vertical position), `title_match` (the bookmark title found
in the destination page's text), `text_offset` (a heading, which *is* text at a
position), or `page` — the last meaning no offset was resolvable and the entry
resolves to the first passage of its page. `byte_offset` stays nullable and
consumers that ignore it keep working.

`extraction` reports what the document's own reading had to decide: how many
pages clustered into one body column and how many were too ambiguous to
reorder, how many marginalia blocks moved after their page, how many repeating
head/foot runs were removed, and how line-wrap hyphens resolved. A document
dominated by `ambiguous_column_pages`, or by `page` anchors, is one whose
structure Wilkes could only partly recover — visible here rather than
discovered later as a section boundary in the wrong place.

#### Spaces and status

`PUT /api/corpora/spaces` accepts `{ corpus_id, embedding }`, creates or reuses
an internal projection workspace, backfills it from canonical admitted
renditions, and returns `{ embedding_space_id, embedding_space_identity, ready,
indexed_generation, workspace_id, primary }`. Internal projection workspaces
are registry members for lifecycle purposes but are omitted from the ordinary
workspace list, and never become independent corpus ids on this API.

The primary `embedding_space_id` is `null` until the corpus holds an index, and
once reported does not change for the life of that index.

Status reports the exact primary stored space identity, ready/required/embedded
counts, whole-document reused/computed chunk totals, source/temporary/index/
total bytes, the time of the integrity query, pending managed imports and
runtime builds, the canonical `corpus_generation`, and every registered
`spaces[]` projection with its independently computed `indexed_generation` and
readiness. A document contributes to these counts only after
`admission_state = ready`.

#### Backup and restore

Backup always represents one restorable logical corpus. Where the selected
space is an internal projection, Wilkes combines the canonical retained sources
and manifest identity with a transactionally snapshotted database for that
projection. Restore installs that selected space as the corpus's initial
projection; other spaces are derived caches, re-established from the canonical
renditions when requested. A backup request never accepts an arbitrary
destination.

### Chunk addressing — `/api/chunks`

All four take `scope`. All four answer with `embedding_space_id`, `engine`,
`model_id`, and `dimension`.

| Method | Path | Replaces |
|---|---|---|
| POST | `/api/chunks/resolve` | `underdog/chunks/resolve` **and** `/api/export/chunk-text` |
| POST | `/api/chunks/accumulate` | `underdog/chunks/accumulate` **and** `/api/embed/centroid` |
| POST | `/api/chunks/similarity` | `underdog/chunks/similarity` **and** `/api/embed/similarity` |
| POST | `/api/chunks/search` | `underdog/chunks/search` |

`resolve` takes `{ scope, chunk_refs }` and returns the text, `text_sha256`,
byte range, origin, and ordinal of each — ascending by ordinal, which is
reading order, not the order asked for. It needs no `root`/`path`, which is why
it replaces `export/chunk-text` rather than sitting beside it: a ref already
names its document.

`accumulate` takes `{ scope, groups }` and returns, per group, the
**unnormalized sum of individually L2-normalized member vectors** and the
member count. This is the surviving form and `/api/embed/centroid`'s normalized
mean is deleted, because the mean is derivable from the sum and the reverse is
not: a caller partitioning a large group across requests adds the sums and
counts and normalizes exactly once.

`similarity` takes `{ scope, probes, chunk_refs }` where each probe is
`{ vector, scope: [chunk_ref] }`, and answers both directions plus a per-probe
mean over the scope it named.

`search` takes `{ scope, probes, top_k, min_similarity }`. A probe is either
`{ "vector": [...] }` or `{ "text": "..." }`, untagged. Text probes are
embedded in the **query** role, which is the only way that role is reachable —
`/api/embed/text` answers in the passage role because the vectors it returns
are stored. All text probes in one request embed in one batch, because two
embed calls are two chances for a model to be swapped between them. Hits carry
`chunk_ref`, `snapshot_id`, `rendition_id`, `ordinal`, `similarity`.

### Embedding — `/api/embed`

| Method | Path | Change |
|---|---|---|
| POST | `/api/embed/text` | Takes `scope`; response gains `embedding_space_id`. Replaces `underdog/embed/text`. |
| GET | `/api/embed/capabilities` | New. Replaces `/api/embed/engines`, `/api/embed/models`, and `underdog/embed/models`. |
| GET | `/api/embed/model-size` | Unchanged. |

`embedding_space_id` on `/api/embed/text` is `null` when the addressed
workspace holds no index — embedding text does not require one — and non-null
whenever the caller supplied a pin, since a pin implies an index.

`/api/embed/capabilities` returns the existing `EmbedderCapabilityManifest`
(`{ engines, roles, models }`), which is already built from `list_models` and
is a superset of `ModelDescriptor` in every field but two. `EmbedderCapability`
therefore gains `is_default` and `is_recommended`, and the UI's model picker
migrates from the three deleted endpoints to this one. The manifest's two
load-bearing nulls stay as documented: `dimension: null` for a hand-added model
whose width only a first load reveals, and `prefix_source` distinguishing
`discovered` / `curated` / `not_documented` / `undetermined`.
`supported_dimensions` continues to hold exactly one entry until Wilkes
implements a truncation contract.

`model-size` stays a separate route because it is an action — a network fetch —
and not a field the manifest can carry for an uninstalled model.

### Export — `/api/export`

| Method | Path | Change |
|---|---|---|
| POST | `/api/export/chunks` | Takes `scope`; `ExportedChunk.chunk_id` becomes `chunk_ref`, and the chunk gains `text_sha256`. |
| POST | `/api/export/outline` | Takes `scope`. Otherwise unchanged. |
| POST | `/api/export/files` | Takes `scope`. Otherwise unchanged. |
| — | `/api/export/chunk-text` | **Deleted** — see `/api/chunks/resolve`. |

Export routes keep their own confinement: they are bounded by the workspace's
configured library roots, not by the uploads directory. That distinction is
deliberate and unchanged — the uploads jail is right for a browser talking to a
shared server and wrong for a consumer asking about the library itself.

`ExportedChunk` becomes the export-only extension of the shared chunk shape:
the same `chunk_ref` / `ordinal` / `text` / `text_sha256` / `byte_range` /
`origin` that `resolve` returns, plus `embedding`. One definition of what a
chunk looks like on the wire, so a consumer cannot store one shape from one
route and fail to match it against the other.

### Catalogue — `/api/catalogue`

| Method | Path | Replaces |
|---|---|---|
| POST | `/api/catalogue/search` | `underdog/catalogue/search` |
| POST | `/api/catalogue/sync` | `underdog/catalogue/sync` |
| GET | `/api/catalogue/status` | `underdog/catalogue/status` |
| POST | `/api/catalogue/acquire` | `underdog/catalogue/acquire` |

The mirror is one per *installation*, in
`shared_data_dir/catalogue`, alongside the model cache and the managed
backups. It was per-workspace, on the argument that duplicating a few thousand
rows was cheaper than a second global storage location; the argument does not
survive the routes being unscoped. What the mirror holds is what four public
catalogues publish, which is the same answer for every workspace, so the
per-workspace copy charged a sync per workspace and still let a consumer that
synced under one workspace find an empty mirror under the next.

`search` gains one field: each result carries the `terms` its query reduced to
after stopword and length filtering, because an empty `hits` has two causes a
caller must not confuse — nothing matched, or nothing in the query was
searchable at all — and only the store can tell them apart. Otherwise the
shapes are unchanged: the 64-query cap and the grain filter stand, an unknown
grain name is a `400` that names it, and a query matching nothing is an empty
`hits` rather than a failed batch. `acquire` continues to delegate to
`wilkes_core::acquire::download_to_root` — the same downloader behind the MCP
`download` tool — and to land bytes in uploads rather than a library root,
because a library root is a place the user put their files.

`acquire` remains ungated by `ensure_writable`. The gate turns away the *user*
adding documents to a library another application owns; this is that
application fetching into Wilkes's own staging area on the way to import.

## Types

Nine types are deleted as duplicates. The survivor of each pair is the managed
form with its `Managed` prefix dropped, since after this change there is no
unmanaged form to distinguish it from.

| Deleted | Survivor |
|---|---|
| `ManagedEmbeddedTexts` | `EmbeddedTexts` (gains `embedding_space_id`) |
| `ChunkCentroids` | `ChunkAccumulations` / `ChunkAccumulation` |
| `ChunkTextExport`, `ChunkText` | `ChunkResolution` / `ChunkExport` |
| `ManagedChunkSimilarities` | `ChunkSimilarities` (now `chunk_ref`) |
| `ManagedProbeSimilarity` | `ProbeSimilarity` (now `nearest_chunk_ref`) |
| `ManagedChunkNearest` | `ChunkNearest` (now `chunk_ref`) |
| `SimilarityProbeRequest` | `ProbeRequest` (scope is `Vec<ChunkRef>`) |
| `ManagedChunkResolution` | `ChunkResolution` |
| `ManagedChunkExport` | `ChunkExport` |

`ManagedChunkSearch` / `ManagedProbeSearch` / `ManagedChunkSearchHit` lose the
prefix. `ManagedDocumentExport`, `ManagedCorpusBackup`, `ManagedBackupFile`,
`ManagedEmbeddingWork`, `ManagedWorkspaceStatus`, and
`ManagedEmbeddingSpaceStatus` keep theirs: they describe the managed corpus
contract, which continues to exist.

### Request caps

The two surfaces set different caps for the same operations, and the conflict
must be resolved rather than carried:

| Operation | Cap | Note |
|---|---|---|
| `accumulate` | 256 groups, 4,096 refs total | Unchanged; already shared. |
| `similarity` | 512 probes, 8,192 refs across searched set and scopes | Unchanged; already shared. |
| `search` | 512 probes, `top_k` ≤ 100 | Constants renamed without `MANAGED_`. |
| `resolve` | **512 refs** | Reconciled. |

`resolve` is the reconciliation. The generic route capped at 64 on the
reasoning that displaying a passage should cost a passage; the managed route
borrowed the similarity cap of 8,192, which is sized for an operation returning
two scalars per probe, not one returning full text. 512 is chosen against what
the reply actually weighs — roughly half a megabyte at typical chunk sizes,
comparable to the other consumer responses — and callers wanting more page.

## Errors

The ten stable codes are unchanged in meaning and become the shared vocabulary
for every route in this document, not only the corpus ones:

`MANAGED_WORKSPACE_NOT_FOUND`, `MANAGED_WORKSPACE_CONFIGURATION_MISMATCH`,
`MANAGED_WORKSPACE_PROTECTED`, `EMBEDDING_SPACE_MISMATCH`,
`EMBEDDING_SPACE_STALE`, `EXTRACTION_RECIPE_MISMATCH`,
`SOURCE_CHANGED_DURING_IMPORT`, `DOCUMENT_INDEX_INCOMPLETE`,
`CHUNK_REF_NOT_FOUND`, `IDEMPOTENCY_KEY_CONFLICT`.

One is added: **`INDEX_IDENTITY_UNVERIFIED`** (`409`), for an index whose
`exact_identity` is absent and whose chunk refs are therefore null. Its message
names a rebuild.

The codes stop being recovered from message text. `managed_err` currently
receives a `String` and searches it for a known substring, which makes the
machine-readable half of the contract depend on prose nobody is stopping from
being reworded. A route that can fail this way returns a typed error carrying
its code, and the HTTP layer maps the code to a status. The status mapping is
unchanged: `MANAGED_WORKSPACE_NOT_FOUND` and `CHUNK_REF_NOT_FOUND` are `404`;
`MANAGED_WORKSPACE_CONFIGURATION_MISMATCH`, `EMBEDDING_SPACE_MISMATCH`,
`EMBEDDING_SPACE_STALE`, `EXTRACTION_RECIPE_MISMATCH`,
`IDEMPOTENCY_KEY_CONFLICT`, and `INDEX_IDENTITY_UNVERIFIED` are `409`; the rest
are `400`.

Every consumer request body keeps `#[serde(deny_unknown_fields)]`, which the
managed bodies already carry and the generic ones do not. A consumer that
misspells `expected_embedding_space_id` must be told, not silently served
unpinned.

## What is deleted

Twenty-one routes go, fifteen arrive.

Deleted: all sixteen `/api/integrations/underdog/*`; `/api/embed/centroid`;
`/api/embed/similarity`; `/api/embed/engines`; `/api/embed/models`;
`/api/export/chunk-text`.

Added: six `/api/corpora/*`; four `/api/chunks/*`; four `/api/catalogue/*`;
`/api/embed/capabilities`.

Changed in shape but not path: `/api/embed/text`, `/api/export/chunks`,
`/api/export/outline`, `/api/export/files`.

**No compatibility aliases are published.** Both sides of this contract are
under one owner, so the cutover is a single coordinated release. An alias would
be precisely the second mechanism this spec exists to remove, and the deletion
of the rowid vocabulary is the whole point rather than a side effect.

## Conformance fixture

`fixtures/managed-semantic-corpus-v1.json` becomes
`fixtures/consumer-api-v2.json` with `schema_version: 2`. Both sides carry it,
and the server test that reads it through the types it serializes moves with
it. Its two references — the `include_str!` in the server test and the link in
`extraction-fidelity.md` — are updated in the same change.

The fixture gains coverage for what this spec adds: an `ensure_request` with
`owner`; a `scope` object in its pinned and unpinned forms; a
`chunks_resolve_request`/`response` pair; a `chunks_accumulate_response`
carrying `sum` and `member_count`; and a `chunks_search_request` holding one
vector probe and one text probe, since the untagged enum is the shape most
likely to drift silently.

It keeps the assertions that already earn their place: the two load-bearing
nulls in the capability manifest, the outline anchor and extraction
diagnostics, an import request against an empty corpus omitting its space id,
and the negative assertion that a backup request never accepts an arbitrary
destination.

## Implementation order

Each step leaves the tree building and the tests green.

1. **Rename the owner out of the data model.** `EnsureManagedWorkspace` gains
   `owner`; the five literal `"underdog"` comparisons in `workspace.rs` match
   the request's value; the `underdog_*` functions become `managed_*`. No
   route paths move yet, no manifests migrate.
2. **Unify index resolution.** One resolver replacing `context_for` and
   `underdog_space_context`, implementing the scope table above. This is the
   change the rest depends on.
3. **Type the errors.** Replace substring sniffing in `managed_err` with a
   typed code carried from the source; add `INDEX_IDENTITY_UNVERIFIED`.
4. **Move the unscoped services.** `/api/catalogue/*`; `/api/embed/capabilities`
   with `is_default`/`is_recommended` added to `EmbedderCapability`; migrate
   the UI model picker; delete `/api/embed/engines` and `/api/embed/models`.
5. **Absorb the chunk operations.** Add `/api/chunks/*`, delete the three rowid
   routes and the nine duplicate types, move `ExportedChunk` onto `chunk_ref`.
   This is the step that discharges rule 1 of the invariant.
6. **Move the corpus lifecycle** to `/api/corpora/*` and delete the
   `/api/integrations/underdog/` prefix entirely.
7. **Bump the fixture** to v2 and update its two references.

Steps 1–3 are internal and observable only as better errors. Steps 4–6 are the
wire break, and land together with Underdog's matching release.

`managed-semantic-corpus-api.md` is already withdrawn: its still-current rules
are carried in this document, and the rest described the adapter this spec
dissolves.
