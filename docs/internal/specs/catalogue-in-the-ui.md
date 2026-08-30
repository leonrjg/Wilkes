# Catalogue in the UI — Design

Status: implemented, except §4 (coverage re-ranking) and §5.3, both deferred
Depends on: catalogue mirror (schema v2, `shared_data_dir/catalogue`), semantic index, `acquire::download_to_root`
Premise: the catalogue is Wilkes' feature, not a sidecar's — the same premise that
took one consumer's name out of the corpus routes in `consumer-api.md`.

## 1. Purpose

Wilkes answers *what do I have that says X*. The catalogue answers *what should I
have*. Today it answers it to nobody: four HTTP routes, no UI, no desktop command.

This is the design for the second question becoming part of the application that
already asks the first.

## 2. Invariant

**A library that cannot say what it is missing is only half a library.**

Concretely: the moment Wilkes learns that the library cannot answer a question is
the moment it holds everything the catalogue needs — the question, in the user's
own words, in the shape (prose, blurb-like) the mirror's BM25 recall was built
for. Every surface below is an application of that one moment. Nothing here
introduces a second way to acquire a document, a second downloader, or a second
place that decides what a grain is.

## 3. Why it is a ghost, mechanically

Not an oversight in the UI. Three things are missing, in order of depth:

1. **The logic is in the wrong crate.** `catalogue_sync_handler` in
   `crates/server/src/lib.rs` holds the fetch-and-store loop itself: the registry
   walk, the per-provider failure handling, the outcome accounting. `wilkes-api`
   contains no catalogue code at all. The desktop shell delegates every business
   operation to `AppContext`, so there is nothing for it to delegate to.
2. **No IPC.** No `#[tauri::command]`, therefore nothing in `tauri.ts`.
3. **No contract.** `SearchApi` — the interface both shells implement, whose
   comment says "Shared across desktop and web. All methods are identical" — never
   names the catalogue.

So step one is the same move `consumer-api.md` made for corpora: the operations
belong in `AppContext`, and both the axum handler and the Tauri command become
thin wrappers over them. Nothing about the store or the providers changes.

## 4. The re-rank Wilkes can do and Underdog could not — deferred

**Deferred by decision, not by discovery.** Nothing below was built. The
surfaces ship showing recall in recall order, and every one of them says so in
as many words, which is the posture §4 demanded of them anyway when no index
exists. Building the coverage column later changes what the rows are sorted by
and adds one column; it does not change the shape of anything shipped.


`catalogue/mod.rs` draws a hard line: `search` returns recall, not a ranking,
because deciding which record is best for a reader "requires knowing what that
reader already knows, which is not a fact about documents and is not something
Wilkes holds."

That was true of a headless mirror serving a sidecar. It is not true of Wilkes.
The library *is* a statement of what the reader already has, and the semantic
index is a queryable form of it. Wilkes cannot know what the reader knows, but it
can answer the tractable proxy:

> **Coverage.** For each candidate, run its title and subject as a query against
> the local index; the best similarity is how well the library already covers it.

High coverage means *you already own something on this*. Low coverage means the
gap is real. That is a fact about two document sets, which is exactly the kind of
fact Wilkes is entitled to state.

Two rules keep this honest:

- **Coverage is displayed, not folded in.** It appears as its own column — "your
  library: 3 documents already close to this" — and never as an invisible
  reordering. A hidden second ranking is precisely what the module docs refuse,
  and hiding one in the UI would be the same mistake one layer up.
- **No index, no coverage.** When the root has no semantic index, candidates are
  shown in recall order, labelled as unranked. Not hidden, not silently reordered
  by something weaker.

## 5. Surfaces

### 5.1 The gap strip, under an empty result (primary)

`ResultList.tsx:872` renders `No results` today. That string is the feature's
natural home: Wilkes has just proved the library cannot answer, and the probe
already exists — it is what the user typed.

- **Trigger:** a *completed* search returning zero rows. One request per completed
  search, never per keystroke. A thin-but-nonempty result gets a line
  ("Nothing here teaches this? Search the open catalogues") that queries
  nothing until it is clicked — grep matches carry no relevance score, so any
  threshold for "thin" would be invented, and deciding that a user's own
  results are inadequate is not Wilkes's to do.
- **Shape:** a strip of at most five candidates: title, provider, grain chip,
  licence (and, once §4 lands, coverage). Grain is shown because it is the
  honest label of what the thing is — a documentation set and a textbook answer different questions, and
  the chip is cheaper than a paragraph explaining that.
- **Action:** one per row, *Add to library*. Never automatic. A download is a
  network fetch of up to 100 MB into the user's files; it is a decision, and the
  decision is theirs.
- **Grains:** all of them by default. The store's own comment argues for the set
  over the single preferred kind, and the UI has less information than the store
  about which one the user meant, not more.

### 5.2 The catalogue pane (browse and add)

A dockable pane in the Bookmarks / Topics / Chat idiom, opened from the top bar,
holding a search field over the mirror, the grain filters as toggles, and the same
candidate rows.

- **Placement is deliberate:** this belongs beside `UploadZone` and
  `DirectoryPicker` — the places documents enter a library — not beside the search
  results. Browsing for material to acquire is an acquisition act.
- **Read-only workspaces:** gated exactly as `UploadZone.tsx:135` gates itself,
  on `useActiveWorkspaceReadOnly`. A managed workspace's files belong to the
  application that owns it.
- **Add resolves to the existing path:** acquire into the workspace's uploads
  directory via `acquire::download_to_root`, then the import that already exists
  (`import_files` on desktop). One downloader, one importer; the catalogue adds a
  source of URLs, not a second way in.
- **A candidate with no `pdf_url` cannot be added, and says so.** That is a real
  and common case — the field is `Option`, and its absence is why the type's own
  comment separates admission from discovery. Such a row offers its landing URL
  and no Add button, rather than a button that fails on click.

### 5.3 Prerequisites for the document in the viewer (deferred)

`RelatedDocumentsPane` answers *what else here is like this*. The catalogue
counterpart is *what teaches what this assumes*, probed from the document's own
metadata and topics.

Explicitly deferred, and named here so it is deferred rather than forgotten: it is
the one surface where the recall/rank distinction actually bites, because the user
never typed a query and cannot see what the probe was. It needs 5.1 and the
coverage column to have earned trust first, and the coverage column is itself
deferred — so this stays deferred behind it rather than being built on nothing.

## 5.4 Progress

Both long operations report, because both are long enough that silence reads as
a hang: a whole-catalogue fetch is minutes, and a textbook is tens of megabytes
over a link nobody here controls.

- **A provider fetch reports pages, not a percentage.** No catalogue says how
  much it holds before it has been walked, and the one that publishes a total —
  LibreTexts' `numTotal` — is wrong by about 1,700 books. A rising count is
  honest; a bar claiming to know the end would not be.
- **A download reports bytes, and a bar only when there is a denominator.** A
  chunked response declares no length, so those show what has arrived and no
  bar, rather than a bar that would sit at zero for exactly the slowest
  downloads.
- **Reports are lossy and never block.** `try_send` throughout: a consumer that
  stopped reading must not slow a fetch down, and the caller learns the outcome
  from the return value regardless.
- Progress is keyed — by provider, and by the URL as it was *requested* — so
  that two things happening at once cannot render each other's numbers.

## 6. Settings

A `catalogue` tab in `SettingsModal`'s union, in its own nav group, showing
`/api/catalogue/status`: provider, grain, record count, last synced.

- **One `Sync now` button, per-provider results.** The sync already reports
  outcomes per provider; a single global "failed" would hide that three of four
  succeeded, which is the ordinary case when one provider changes its wire shape.
- **Manual only, in v1.** No sync-on-startup and no staleness timer. Four
  whole-catalogue fetches is a network call the user did not ask for, and the one
  honest trigger — the mirror being empty when someone first opens the pane — is
  better served by the pane saying "this mirror is empty; sync to fill it" with
  the button right there.
- **The tab states that the mirror is installation-wide.** It now lives in
  `shared_data_dir/catalogue`, so its numbers do not change when the workspace
  does, and a settings page that looks workspace-scoped would imply otherwise.

## 7. Work, in dependency order — done

1. `AppContext` gains `catalogue_search`, `catalogue_sync`, `catalogue_status`,
   `catalogue_acquire`. The sync loop moves out of `crates/server`; the axum
   handlers become wrappers, as the corpus routes already are.
2. Four `#[tauri::command]`s over those, registered in the desktop invoke handler.
3. `SearchApi` gains the four methods — shared, not `?`-optional: both shells can
   serve them, and an optional method is how a feature becomes invisible on one
   platform.
4. `lib/types.ts` gains `CatalogueRecord`, `CatalogueHit`, `CatalogueProviderStatus`,
   mirroring the serde shapes (`CatalogueHit` flattens its record).
5. `useCatalogueStore`, in the `useTopicsStore` pane idiom.
6. `CataloguePane.tsx`, the gap strip inside `ResultList`, `CataloguePanel.tsx`
   for the settings tab.

Steps 1–3 are the whole of "stop being a ghost"; 4–6 are the surfaces.

## 8. One defect this design exposed — fixed

`CatalogueStore::search` returns an empty vec both for *nothing matched* and for
*no term in your query survived stopword and length filtering* — the second is a
real case, since terms under two characters are dropped and the reference grain
exists to answer questions about languages named `C` and `R`. The store knows the
difference and discards it at the return.

The UI cannot reconstruct that without duplicating `fts_expression`, so the fix
belonged in core. `CatalogueStore::search` now returns `CatalogueRecall` — the
hits and the terms the query was actually run with — and every surface reads
them: the browse pane says "nothing in that could be searched for", and the gap
strip renders nothing at all rather than claiming an absence it never tested
for.
