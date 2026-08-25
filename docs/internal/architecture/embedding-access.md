# Embedding — capability, access, and what a consumer can reach

Status: assessment of what exists, 2026-08-25. Not a proposal to build.
Occasioned by: a measured retrieval failure in Underdog
(`docs/ACQUISITION.md` §12e), which asked "would a different embedding model
fix this?" and turned into "what can the consumer actually reach?"

Where a claim is **measured**, the measurement and its date are given. Where it
is **read off the code**, the file and line are given. Two of the conclusions
below reverse what the same investigation asserted an hour earlier, and both
reversals came from checking rather than reasoning.

---

## 0. Method

Read: `crates/core/src/embed/`, `crates/core/src/generate/`,
`crates/api/src/context.rs`, `crates/server/src/lib.rs`, fastembed 5.13.0's
model tables.

Measured: latency against the live workspace
(`9298f4c8…`, **5,484 chunks** in the managed index), release build,
`wilkes-server --port 2020`. `config_sentence_transformers.json` fetched live
from HuggingFace for ten candidate models.

---

## 1. There are three engines, and two of them take any model

`EmbeddingEngine::{SBERT, Candle, Fastembed}`
([types.rs:924](../../../crates/core/src/types.rs)). `supports_custom_models()`
is **true for SBERT and Candle**, false for Fastembed
([types.rs:960](../../../crates/core/src/types.rs)).

| engine | catalogue | custom models | device |
|---|---|---|---|
| Fastembed | fastembed's fixed enum (~44 entries) | no | CPU by default; `fastembed-coreml` feature |
| Candle | 9 curated, plus any HF repo | **yes** | Metal (`candle-metal`), CPU |
| SBERT | 9 curated | **yes** | auto |

Candle loads **BERT, JinaBERT and ModernBERT** architectures
([candle.rs:296-318](../../../crates/core/src/embed/engines/candle.rs)), which
covers most of the current open embedding field. Its curated list already names
`BAAI/bge-base-en-v1.5`, `mixedbread-ai/mxbai-embed-large-v1` and
`snowflake-arctic-embed-{xs,s,m,l}`.

**Consequence.** The question "which of fastembed's models should we pin?" is
the wrong question. Fastembed is the most constrained of the three engines and
the only one that cannot take a model by name.

---

## 2. The query/passage distinction exists, and is wired

Not a gap. `Embedder` carries `embed_query` and `embed_passages` as trait
methods ([embed/mod.rs:41-52](../../../crates/core/src/embed/mod.rs)), and
`aux_config.rs` reads the `prompts` map out of a model's
`config_sentence_transformers.json` and threads the prefixes through the worker.
Wilkes' own semantic search uses the roles correctly — `embed_query` for the
query, `embed_passages` for the index
(`search/semantic.rs`, `embed/index/db.rs`).

**Which models actually carry prompts** (fetched 2026-08-25):

| model | `prompts` |
|---|---|
| `Snowflake/snowflake-arctic-embed-m`, `-l`, `-m-v1.5` | query prefix present |
| `mixedbread-ai/mxbai-embed-large-v1` | query **and** passage present |
| `BAAI/bge-base-en-v1.5`, `bge-large-en-v1.5` | absent |
| `nomic-embed-text-v1.5`, `multilingual-e5-large-instruct` | absent |
| `Alibaba-NLP/gte-modernbert-base`, `nomic-ai/modernbert-embed-base` | absent |
| `sentence-transformers/all-MiniLM-L6-v2` (the current pin) | absent |
| `intfloat/e5-small-v2`, `thenlper/gte-small` | no file at all |

So the mechanism is real but sparsely fed: most retrieval models document their
prefixes in the README rather than in the config this parser reads. A model
whose prompts are not in that file gets no prefixes, silently, and the only
trace is a `tracing::debug!`.

---

## 3. What a managed consumer can reach

Underdog is the only managed consumer. Its whole surface is
`/api/integrations/underdog/*`, and the embedding-relevant part is:

| route | takes | role used |
|---|---|---|
| `embed/text` | texts | **`embed_passages`**, always |
| `chunks/search` | **vectors only** (`ManagedSearchProbe { vector }`, [lib.rs:474](../../../crates/server/src/lib.rs)) | — |
| `chunks/similarity` | **vectors only** | — |
| `chunks/accumulate`, `chunks/resolve` | chunk refs | — |

`managed_embed_texts` → `embed_texts` → `embed_passages`
([context.rs:1732](../../../crates/api/src/context.rs)). There is no role in the
request and no way to ask for one.

**This is the finding that matters.** Every vector Underdog holds — knowledge
point labels, evidence centroids, goal-scope subject probes, acquisition target
and candidate vectors — is embedded as a *passage*. Three of those are passages
and the arrangement is correct for them. The subject probe and the acquisition
target are queries, and they are not.

It costs nothing today: the pinned model has no prompts, so `embed_passages`
and `embed_query` are the same function. It is the reason a prefix model would
deliver a fraction of its value if pinned tomorrow.

**The consumer found this first.** Underdog's `docs/EMBEDDING_LEVERAGE.md` §13
(2026-08-19) records the same hard-coding, the same "inert until a prefix model"
caveat, and proposes an optional role field on the endpoint. What this
assessment adds is that most prefix-trained models do not ship their prefixes
in the file `aux_config` reads (§2), that the fastembed engine could not load
them anyway (§6.1), and that the role belongs on the search endpoint rather than
the embed one (§7). §13b there raises the half that lives on the consumer's
side and cannot be fixed here: a stored knowledge-point vector is read back both
as a passage and as a query probe, and one vector cannot be both under an
asymmetric model.

---

## 4. The managed/plain split is a trust boundary, not a copy

The obvious reading is that `/api/embed/text` and
`/api/integrations/underdog/embed/text` are the same endpoint twice. They are
not, and the difference is entirely in how the workspace is addressed.

| | plain | managed |
|---|---|---|
| addressed by | `workspace_id` ([lib.rs:1180](../../../crates/server/src/lib.rs)) | `corpus_id` + `expected_embedding_space_id` |
| on space mismatch | nothing to mismatch | `EMBEDDING_SPACE_MISMATCH`, refused |
| reply names the space | no | yes |
| shared implementation | `embed_texts` | the same `embed_texts` |

`managed_embed_texts` is 24 lines and computes nothing. What earns it is
`managed_context`: a consumer keeping a vector space of its own must never be
handed vectors from another model, and the plain route cannot promise that.
Underdog's client already respects this — its `legacy_embed_texts` is
`#[cfg(test)]` and unreachable in production.

The same reasoning explains the rest of the managed surface. The managed API
addresses chunks by **stable `ChunkRef`/snapshot/rendition**; the plain API by
**file path and rowid**, which do not survive a rebuild. Different identity
contracts need different projections, and that is why `accumulate_chunk_refs`
([db.rs:6293](../../../crates/core/src/embed/index/db.rs)) exists beside
`chunk_centroids` ([db.rs:6441](../../../crates/core/src/embed/index/db.rs)).

---

## 5. Two search implementations, and the measurement that spared them

`SemanticIndex` finds nearest chunks two ways:

* `query_corpus` ([db.rs:5551](../../../crates/core/src/embed/index/db.rs)) —
  one probe, `WHERE v.embedding MATCH ?1 AND v.k = ?2 ORDER BY v.distance`,
  through the vec extension.
* `managed_chunk_search` ([db.rs:6553](../../../crates/core/src/embed/index/db.rs))
  — N probes, `SELECT … FROM chunks JOIN files JOIN vec_chunks` **with no
  WHERE clause**: every row, decode every blob, normalize, dot in Rust against
  every probe, sort, truncate per probe.

The identity contract of §4 explains what each *returns*. It does not explain
the second scan, and the obvious prescription is to fold the managed search
onto the KNN path and keep the managed projection.

**The measurement says don't.** Live workspace, 5,484 chunks, `top_k` 24,
release build, median of seven:

| request | median |
|---|---:|
| `GET catalogue/status` (trivial, the floor) | 9 ms |
| `chunks/search`, 1 probe | **8 ms** |
| `chunks/search`, 8 probes | **8 ms** |
| `chunks/search`, 24 probes | **9 ms** |

The scan is *below the noise floor of an HTTP round trip*, and the marginal
cost of a probe is indistinguishable from zero — 1 probe to 24 costs 1 ms.
That is the scan's virtue: it reads each row once and scores every probe
against it while it is in hand. A per-probe KNN loop inverts that curve, so
folding managed search onto `query_corpus` would be a **regression** for the
batched callers the managed API exists to serve.

So: two implementations of a similar idea, each shaped for its caller, and
nothing is paying for it. It stays. The scan is linear and this corpus is
small; the trade reverses somewhere well above 5,484 chunks, and that is the
number to re-measure at, not a reason to reorder work now.

> The same measurement taken against the **debug** binary read 165 ms / 206 ms /
> 247 ms, and prompted a confident recommendation to remove the scan. Debug Rust
> does the decode-and-dot loop roughly twenty times slower while sqlite-vec's C
> is optimised either way, so a debug measurement systematically indicts hot
> Rust and exonerates the extension beside it. Timing this workspace in debug is
> not a weak measurement; it is a misleading one.

---

## 6. Defects found

1. **The fastembed engine can never load prefixes.** `fetch_aux_configs` is
   passed the fastembed *enum name* — `"AllMiniLML6V2"`, `"BGEBaseENV15"` —
   where an HF repo id is required
   ([fastembed.rs:133](../../../crates/core/src/embed/engines/fastembed.rs));
   `info.model_code` holds the real repo two lines above and is unused.
   `load_prefixes` then reads the cache under the same non-repo key
   ([fastembed.rs:432](../../../crates/core/src/embed/engines/fastembed.rs)).
   Candle does it correctly
   ([candle.rs:769](../../../crates/core/src/embed/engines/candle.rs)).

   Latent: the pinned model has no prompts, so nothing observable changes
   today. It bites the first person to pin arctic-embed or mxbai through
   fastembed, and it bites silently.

2. **`managed_chunk_search` never normalizes the probe.** It normalizes each
   stored vector and dots
   ([db.rs:6553](../../../crates/core/src/embed/index/db.rs)), so the value it
   calls `similarity` is cosine only if the caller sent a unit vector. Both
   engines normalize on output, so it holds — but it is an unchecked invariant
   under a `min_similarity` floor, and two of Underdog's thresholds ride on it
   (`COVERS_THRESHOLD`, `SHELF_PROBE_MIN_SIMILARITY`).

3. **No reranker.** fastembed 5.13 ships `BGERerankerV2M3`, `BGERerankerBase`
   and two Jina rerankers; nothing in Wilkes wires them. Noted because §7's
   failure is a bi-encoder failure, and a cross-encoder is its standard remedy.

---

## 7. The one thing that is structurally wrong

`chunks/search` and `chunks/similarity` take vectors and never text. So
"search the corpus for this text" is necessarily two calls — embed, then
search — and the embed call cannot know what the text is *for*. That is why
§3's role problem has nowhere to live.

The tempting fix is a `role` field on the embed request. It is the wrong one:
it asks the caller to declare something the endpoint should already know, and
a flag that can be set correctly can be set wrongly.

**`chunks/search` should accept `{text}` as an alternative to `{vector}`, and
embed it with `embed_query` itself.** Then the role is implied by which
endpoint was called, a search is one round trip, and a future prefix model
works without the consumer knowing it exists.

`embed/text` still has to exist. Underdog needs raw vectors for its own store
and for scoring catalogue records that live in neither system — and those are
all passages, so `embed_passages` is right there and stays.

---

## 8. If the model is rotated

Not a recommendation to rotate — that is Underdog's call and
`EMBEDDING_MODEL_CHANGE.md` records that online rotation is not an operational
path yet. What the assessment says about the choice:

* **Candle, not fastembed.** Custom models, Metal, and the prefix path that
  actually works.
* **`Snowflake/snowflake-arctic-embed-m` (768)** is the best fit for the
  measured failure: retrieval-trained with hard negatives, already in Candle's
  catalogue, and one of the few models that ships the query prompt so §2's
  machinery fires. `mxbai-embed-large-v1` (1024) is the step up, and ships both
  prompts.
* **Not `all-mpnet-base-v2`.** Same symmetric-similarity family as the current
  pin, just larger: more capacity for the wrong objective.
* **§7 first, or the query prefix never reaches a query.**

---

## 9. What would show this wrong

* **§4's boundary.** If a consumer is ever handed managed vectors without a
  space check and nothing goes wrong for a year, the pin is ceremony.
* **§5's measurement.** At a corpus an order of magnitude larger, re-time it.
  If the 1-probe search leaves the noise floor, the scan needs the KNN path
  after all — and the batched callers need measuring before it moves.
* **§8's model claim.** Untested. Re-run Underdog's §12e probe set against
  arctic-embed-m before anyone commits: `"causal inference"` should clear the
  adjacent band on the record `"econometrics"` already reaches at 0.578, and
  the cognitive-science course should fall below it. If neither moves, the
  failure is not the bi-encoder's objective and §6.3's reranker is the lead.
