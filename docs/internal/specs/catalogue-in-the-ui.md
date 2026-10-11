# Catalogue in the UI — Design

Status: implemented, except §4 (coverage re-ranking) and §5.3, both deferred.
§9 (a catalogue is a literature provider) supersedes the two-halves design of
§5.2 and the separate settings tab of §6; those sections now describe what §9
left standing.
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
- **Action:** one per row, *Add*. Never automatic. A download is a network fetch
  of up to 100 MB into the user's files; it is a decision, and the decision is
  theirs. A course is fetched by the same button under the same label — the
  grain chip already says the row is a course, and what fetching one does is the
  button's tooltip.
- **Only the title shares the buttons' line.** The buttons used to stand beside
  the whole text column, so every line under the title was laid out in the width
  left over beside them — 186px of a 316px pane, and the blurb wrapped at 59% of
  the space it had. Below the title, the row is the pane's full width.
- **Sources:** every catalogue the mirror holds, and nothing else. The strip
  runs unprompted, so it asks only providers answered from this machine — the
  ones the registry reports as `mirrored` — and never an index the user
  enabled, which is on but has not been asked *this* question. The rows are the
  best five across the catalogues by recall score: the catalogues share one
  FTS index, so the scores are one scale.

### 5.2 The catalogue pane (browse and add)

A dockable pane in the Bookmarks / Topics / Chat idiom, opened from the top bar,
holding a search field over the mirror, a filter beside each half's own heading,
and the same candidate rows. Ten records per probe: the pane is a shortlist of
things to consider acquiring, not a second page of search results.

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
- **Catalogues and indexes are one list of providers.** One query goes to every
  selected provider that is on (`literature_search` on desktop, `POST
  /api/literature/search` on the server; both over
  `commands::integrations::literature::search_all`, resolved through
  `IntegrationRegistry` exactly as the MCP tool resolves one provider). Each
  answers under its own name and none is ranked against another. A provider
  that fails is reported in its own entry beside those that answered, and "no
  source is on" is stated as a setting rather than as an empty result. See §9
  for why there is no longer a catalogue half and a literature half.
- **One row for every result.** A result carries the facts its source knows and
  the row shows the ones it carries: a paper has a year, a venue and a citation
  count; a textbook has a kind, a subject, a licence and a blurb. The kind
  badge appears only where the source names a kind — a bibliographic index
  names none, and "Paper" over its monographs and theses would be wrong for
  some of its rows. Whatever the result, it is added through the same
  uploads-then-import path; a course is the one result that is many files.
- **One filter, of providers.** A chip per provider that is on, above the
  results. The grain chips are gone: a catalogue serves exactly one grain, so
  choosing grains was choosing catalogues by another name, and the provider
  chips already do it. What a catalogue holds is said beside its name over its
  results ("DevDocs · Reference").
- **Nothing selected means every one of them, including any added later.** A
  selection is `null` by default rather than an enumeration, so a provider the
  user enables tomorrow, or a catalogue they fetch, joins it. Deselecting every
  provider is the other end — nothing is searched, and the pane says so.
- **The filter persists** to `localStorage` under `wilkes.catalogue.filters`.
  The key keeps its original name so upgrading does not reset a filter, and
  what an older build stored there — a grain filter beside a provider filter —
  is turned into the one selection it amounted to when the provider list
  arrives: the providers that were selected, and the catalogues whose grain
  was. A stored provider id that no longer exists is pruned against the
  registry's list when the pane opens; the backend refuses an unknown id
  rather than quietly asking nobody.
- **The provider filter is passed down, not applied to the answer.** A provider
  left out of it is not asked — these are rate-limited services, and narrowing
  after the fact would have spent the request anyway. Widening asks only the
  providers the held answer does not already cover, and merges.

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
- **A row reports from the click to the file being in the library.** Adding is
  three calls — a provider may have to be asked where the file is, the file is
  fetched, the fetched file is moved into the root — and only the middle one
  reports bytes. A row that showed the byte stream alone said nothing during
  the resolve, nothing between the request going out and the first byte, and
  nothing during the move; that last silence flipped the button back to an
  enabled *Add* while the move was still running. `AddStage` covers all three,
  and `useCatalogueAdd` is its only writer because it is the only thing that
  knows the whole act. It is also the one reporter of failure, so a resolver
  that refused and a move that failed reach the user the way a failed download
  does — both used to reject into nothing at all.
- **Staged is not added.** `acquired` is written when the file lands in
  uploads, which is before it is in the library, so *Added* means the stage is
  clear and not merely that `acquired` has a key.
- **Where there is no denominator, the bar is indeterminate rather than
  absent.** It carries no `aria-valuenow` — which is what that means — and
  reuses the existing `animate-shimmer` utility rather than sitting at zero or
  vanishing for exactly the stages that take longest.
- **Reports are lossy and never block.** `try_send` throughout: a consumer that
  stopped reading must not slow a fetch down, and the caller learns the outcome
  from the return value regardless.
- Progress is keyed — by provider, by candidate, and by the URL as it was
  *requested* — so that two things happening at once cannot render each other's
  numbers. The add stage is keyed for the same reason: it was a single key, so
  the second add to start erased the first row's indicator.

## 6. Settings

A *Catalogues* tab inside Settings › Integrations, beside the tabs of the
providers asked over the network, listing every provider the registry reports
with a mirror: name, grain, record count, last synced. It is one tab for all of
them rather than one each, because a catalogue has nothing to fill in — there
is no key, no origin to move — and one thing to do, which they all share.

- **One `Sync now` button, per-provider results.** The sync already reports
  outcomes per provider; a single global "failed" would hide that three of four
  succeeded, which is the ordinary case when one provider changes its wire shape.
- **A catalogue can be removed, and the mirror is what remembers that.**
  `Remove` on a held catalogue drops its records and its sync time
  (`CatalogueStore::remove_provider`, `catalogue_remove` on desktop, `POST
  /api/catalogue/remove` on the server), so it is no longer searched and reads
  *Never synced* again. There is no separate list of catalogues switched on:
  held is the only state, and syncing is how a catalogue comes back. The top
  button therefore refreshes the catalogues the mirror holds — *Sync held* —
  and is *Sync all* only when that is every one of them or none; a refresh
  that refetched a removed catalogue would have undone the removal. Removing
  takes a second click on the same button, because refetching is minutes. A
  provider the mirror holds rows for but this build no longer registers is not
  a provider, so it is not listed here; `POST /api/catalogue/remove` still
  accepts its id, which is the only way those rows ever leave.
- **Manual only, in v1.** No sync-on-startup and no staleness timer. Four
  whole-catalogue fetches is a network call the user did not ask for, and the one
  honest trigger — the mirror being empty when someone first opens the pane — is
  better served by the pane saying "this mirror is empty; sync to fill it" with
  the button right there.
- **Fetching a catalogue is what switches it on.** There is no enable checkbox:
  a catalogue the mirror holds is searched, and one it does not hold cannot
  be. After a sync or a removal the panel tells the pane's store and the tab
  strip to re-read the provider list, since both show which providers are on.
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

## 9. A catalogue is a literature provider

**Invariant: a catalogue is a `LiteratureSource` whose `search` is answered
from the mirror.**

Until this, there were two of everything — `CatalogueSource` beside
`LiteratureSource`, a hardcoded `registry()` beside `IntegrationRegistry`,
`CatalogueHit` beside `LiteratureSearchResult`, a catalogue half of the pane
with a grain filter beside a literature half with a provider filter, a settings
tab beside the integrations panel — for one difference that is real: where the
ranking happens. The module docs argue that difference carefully and it
stands. Nothing else about a catalogue differs from an index, so nothing else
is separate now.

- **One trait.** `catalogue::MirroredSource` implements `LiteratureSource`;
  its `search` runs `CatalogueStore::search_provider` and maps the mirror's
  records to `LiteratureSearchResult`. What was `CatalogueSource` is
  `CatalogueFetch`: the half of a catalogue that knows its publisher's wire
  shape, held behind `MirroredSource` and searched by nobody.
- **One registry.** `IntegrationRegistry::from_settings(settings, mirror_dir)`
  registers the catalogues beside the manifest-described providers, and every
  caller — the pane, the gap strip, the MCP `literature_search` tool — looks a
  catalogue up by id like any other provider. `mirror_dir` is `None` for a
  caller with no mirror, which registers no catalogue rather than catalogues
  that cannot answer.
- **One meaning of "on".** For a provider described by a manifest, `enabled`
  is the user's setting. For a catalogue it is whether the mirror holds it:
  one that has been fetched is searched, and one that has not is registered
  and off — *switched off*, not *unknown*, the same distinction a disabled
  remote provider gets. There is deliberately no second flag: a setting beside
  the mirror would be two answers to whether a catalogue is in use.
- **One result type.** `LiteratureSearchResult` gained `summary`, `subject`,
  `kind` (the grain), `pages` and `recall_score`, and `LiteratureAcquisition`
  gained `Course`. `CatalogueRecord` remains as the mirror's row.
- **The query that could not be searched** (§8) is now that catalogue's error
  in its own entry — "nothing in “C” could be searched for" — beside the
  providers that answered, rather than a second kind of empty state.
- **Capabilities say what is particular.** `ProviderCapabilities::mirrored`
  is what the gap strip filters on and what the settings panel groups by;
  `LiteratureProviderInfo::mirror` carries the grain, the record count and
  the sync time. No interface holds a list of catalogue ids or names.

What stayed, and why:

- **`/api/catalogue/search` and `/api/catalogue/status`.** A consumer probes
  the mirror in batches, by grain, with the terms each probe reduced to; that
  is a view over the same store and not a second way to search a provider.
  The desktop commands for them are gone, since nothing in the interface
  calls them.
- **`catalogue_sync`, `catalogue_remove`, `catalogue_acquire_course`.**
  Operations only a mirror has. They are not on the `LiteratureSource` trait,
  which is what a provider answers, not how its copy is kept.
- **Ranking across catalogues.** The pane lists each provider's answer under
  its own name, so the single BM25 order across all catalogues that the old
  catalogue half showed is gone from it. The scores are still one scale, which
  the gap strip uses.

Not done here: describing a catalogue by manifest (a `mirror` capability),
which would make LibreTexts, OpenStax and DevDocs TOML and let a user add
their own. The trait no longer stands in its way.

## 10. Filters from a request in words

**Invariant: the model emits filter names and values from the provider's own
declaration, never a request; what it emits becomes the same refinements a
person sets by hand.**

A provider like OpenAlex declares some fifty filters. Finding the six a
sentence describes is the kind of translation a small local model does well,
provided it cannot invent anything — and the manifest already says exactly
what may be asked.

- **The grammar is the declaration.** `generate::tasks::browse_request` builds
  a grammar from `BrowseControls`: `name: value` parts separated by `; `, in
  any order, where a name is `search` (where the listing takes a keyword),
  `sort` (over the declared orders) or a declared filter, and a value is one
  of the filter's declared options or a typed shape. An undeclared filter, an
  unlisted option and a URL are not strings in the language. Unbounded
  repetition rather than counted, so forty filters do not unroll into forty
  copies per part.
- **The form is the one models write unprompted.** It was first a line per
  part, `filter: type = review`, with an on/off filter as a bare name. Run
  against real models, every one not held to the grammar wrote
  `type: review; open_access: on` regardless, and the one that was held to it
  could not be relied on to write a line break — which, carried in a JSON
  string, is an escape sequence — and ran the filters into the keyword. So
  the form is one line, every part has a value, a line break is accepted as a
  separator too, and an order may be written without its name.
- **A model is held to the grammar whether or not its runner does it.** Ollama
  applies the grammar while decoding for GGUF models, as a JSON-schema
  `pattern`; its MLX runner accepts the constraint and ignores it, and answers
  in plain text. Both replies are checked against the same grammar before a
  task sees them, so an MLX model works wherever its answer is in the
  language and is refused, with what it said, where it is not. The pattern
  itself describes the string *as JSON writes it* — a newline is `\n`, a free
  class admits no bare quote or backslash — since that is the text the engine
  is constrained on.
- **Two things a grammar cannot pin are settled after it.** A closed list too
  long to show the model (every country) is written as words and matched
  against the list by value or label. A filter the provider suggests values
  for (an author, a journal) is written as a name and looked up through the
  provider's own `suggest`; the first suggestion is taken and its label kept,
  so the chip reads "Author: Jennifer A. Doudna" and is changed like any other.
- **Checked by the provider's rules before it is returned.**
  `LiteratureSource::check_browse` runs the validation `browse` runs, without
  sending. Each filter is added to what has stood so far and checked, so one
  refused value costs that filter and a note saying why, not the composition.
- **One composition per provider**, sequentially: each has its own filters and
  there is one model. A provider-neutral "intent" mapped onto every provider
  would need filters tagged with what they mean, which is the kinds the
  manifest format was just rid of.
- **Set as refinements, not shown as an answer.**
  `literature_compose` (`POST /api/literature/compose`) returns, per provider,
  `filters`, `labels` and `sort` in the shape the pane's `Refinement` holds.
  The pane sets them, puts the composed keyword in the search box, and runs
  the ordinary search: listings for the providers with filters, the keyword
  for the rest. The status line under the box says what was composed from and
  what was asked for and left out — a missing filter is otherwise silent.
- **Its own button.** Enter still searches for the words typed. The button is
  absent where the generation model is not ready or no provider declares
  filters, like every model-dependent control, and while a composition runs it
  is what stops it (`literature_compose_cancel`): the decode ends at its next
  token, and the pane drops an answer to a composition it stopped.
- **Nothing is searched for by default.** A request nothing could be composed
  from is reported; the sentence is not quietly run as a keyword, and stays in
  the box for the user to do that if they meant it.

Not done: combination modes ("none of" — exclusion — is never composed; every
filter uses its first mode), and filters set from a result row ("works citing
this one"), which a sentence has no row to supply.
