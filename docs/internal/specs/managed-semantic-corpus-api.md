# Managed semantic corpus API

Status: implemented for the Phase 1 protected-workspace contract.

Wilkes exposes a narrow adapter for Underdog. The adapter is deliberately
separate from generic workspace routes: a corpus token selects an
application-managed workspace, but cannot activate, rename, re-root,
reconfigure, or delete it.

## Identity and admission

`embedding_space_id`, `snapshot_id`, `rendition_id`, and `chunk_ref` are
opaque SHA-256-derived strings owned by Wilkes. Callers must persist and echo
them, never reconstruct them. Index rowids are not present on this API.

An import is returned only after Wilkes has:

1. copied the source into `managed_sources/<source_sha256>/`;
2. verified that the source did not change during the copy;
3. extracted a rendition under the configured extraction recipe;
4. copied an existing whole rendition only when source, extraction,
   rendition, ordered chunks, and embedding space match exactly, otherwise
   embedded the retained snapshot; and
5. committed every stable chunk reference and vector in one transaction with
   the document's `admission_state = ready`.

Identical source bytes reuse the retained snapshot. Managed snapshots and
ready index rows are not automatically collected.

Semantic-index schema v10 stores `IndexEmbeddingMetadata`: the historical
engine/model/dimension tuple plus an optional `exact_identity`. Migrated legacy
indexes keep `exact_identity = null`; Wilkes may continue to use their vectors
locally when the tuple matches its runtime, but managed admission never treats
that tuple as vector-compatibility proof. Import from such a workspace is a
normal reuse miss: Wilkes retains the source snapshot and embeds it in the
protected workspace without rebuilding or modifying the source index.

`idempotency_key` is durably bound to the admitted source, extraction recipe,
and rendition. Repeating the job returns that ready document; attempting to
reuse the key for different bytes or a different recipe is refused.

## Routes

- `PUT /api/integrations/underdog/workspace`
- `GET /api/integrations/underdog/status?corpus_id=...`
- `POST /api/integrations/underdog/documents/import`
- `POST /api/integrations/underdog/chunks/resolve`
- `POST /api/integrations/underdog/chunks/accumulate`
- `POST /api/integrations/underdog/chunks/similarity`
- `POST /api/integrations/underdog/embed/text`

Every operation after ensure names `corpus_id` and the
`expected_embedding_space_id` returned by ensure/status. A mismatch is a hard
`409`, never a fallback to the active workspace.

The import `source` is either an explicitly selected local path:

```json
{ "kind": "path", "path": "/selected/paper.pdf" }
```

or a file selected through an existing Wilkes library. The latter must name
both its workspace and authorized library root; Wilkes canonicalizes the root
and file and refuses paths outside that workspace's configured library:

```json
{
  "kind": "wilkes_file",
  "workspace_id": "workspace-id",
  "root": "/library",
  "path": "/library/paper.pdf"
}
```

The managed import response includes `corpus_id`, source bytes/media metadata,
snapshot and rendition identities, `extracted_content_sha256`, exact embedding
space metadata, resolved outline entries, `extraction` diagnostics, stable
chunks, and `embedding_work`. `chunk_count == embedding_work.reused +
embedding_work.computed`; raw vectors, source-workspace identities, and SQLite
rowids are absent. Underdog can recompute every returned text hash and the
extracted-content hash before recording the document.

Each outline entry carries `byte_offset` where Wilkes could establish one, and
an `anchor` naming what established it: `destination_coordinate` (the PDF
destination's own vertical position), `title_match` (the bookmark title found
in the destination page's text), `text_offset` (a heading, which *is* text at a
position), or `page` — the last meaning no offset was resolvable and the entry
resolves to the first passage of its page. `byte_offset` remains nullable and
consumers that ignore it keep working unchanged.

`extraction` reports what the document's own reading had to decide: how many
pages clustered into one body column and how many were too ambiguous to
reorder, how many marginalia blocks were moved after their page, how many
repeating head/foot runs were removed, and how the line-wrap hyphens resolved.
A document dominated by `ambiguous_column_pages`, or by `page` anchors, is one
whose structure Wilkes could only partly recover, and that is visible here
rather than discovered later as a section boundary in the wrong place.

The aggregate response contains an unnormalized sum of individually
L2-normalized member vectors and the computed member count. This lets callers
partition a large group across requests, add the sums and counts, and normalize
exactly once.

Status reports the exact stored space identity, ready/required/embedded counts,
whole-document reused/computed chunk totals, source/temporary/index/total bytes,
the time of the integrity query, and currently pending managed imports/runtime
builds. A document contributes to these counts only after `admission_state =
ready`.

## Stable errors

Managed failures return an `error` message and, when applicable, one of these
machine-readable `code` values:

- `MANAGED_WORKSPACE_NOT_FOUND`
- `MANAGED_WORKSPACE_CONFIGURATION_MISMATCH`
- `MANAGED_WORKSPACE_PROTECTED`
- `EMBEDDING_SPACE_MISMATCH`
- `EXTRACTION_RECIPE_MISMATCH`
- `SOURCE_CHANGED_DURING_IMPORT`
- `DOCUMENT_INDEX_INCOMPLETE`
- `CHUNK_REF_NOT_FOUND`
- `IDEMPOTENCY_KEY_CONFLICT`

Generic workspace APIs retain their compatibility shapes, including rowid
exports and normalized centroids. Their vector loading and managed aggregate
operations share the same core normalized-vector accumulator; managed code
does not introduce a second vector arithmetic definition.
