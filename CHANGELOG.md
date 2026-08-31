# Changelog

## Unreleased

### Added

- Folders can be dropped onto the window to become library roots. A dropped
  folder joins the root strip and becomes the active root, as if it had been
  chosen through Open — previously a dropped folder was refused as an import,
  and a drop that mixed folders with files imported nothing at all. A mixed drop
  now does both, with the files landing in the root that was active when the
  drop happened rather than in a folder the same drop is adding. A read-only
  workspace still refuses both, since roots and imports are equally the
  manifest's business.

- A viewer tab's context menu closes documents. Close dismisses the tab that was
  right-clicked — not whichever one happens to be active — and Close All empties
  the tab strip; both sit below the file actions the menu already offered, which
  every surface still shares.

- HTML files are read rather than inspected. A `.html` or `.htm` document opens
  rendered, with the same source/rendered toggle Markdown has, the same
  remembered choice per document, the same find bar and zoom, and bookmarks,
  search highlights and selections in the file's own bytes — a selection in a
  rendered page produces the same bookmark the source view would have.

  It is a reader, not a browser. The file supplies structure and text and the
  reader supplies the typography: scripts, author stylesheets, frames, forms
  and plugins do not survive parsing, and nothing in a document can cause a
  request to leave the machine, so opening a file cannot tell anyone that it
  was opened. Pictures beside the document do load — a relative `src` is
  resolved against the document and served through the application, which is
  also the only place that reach into the filesystem is judged — while remote
  addresses are refused and `data:` images, being part of the file, are kept.
  Links are destinations rather than navigations: `#fragment` scrolls within
  the document, a link to a neighbouring file is opened by the application, and
  anything else goes to the browser.

- Catalogue fetches say where they have got to. A provider sync reports each
  page as it lands — "Fetching… page 12, 1,204 records" — and a document
  download reports its bytes against the total, or without one when the server
  declared no length. Both travel on their own event streams
  (`catalogue-sync-progress`, `catalogue-download-progress`), and both are
  logged: every download now records its start, its outcome and each refusal,
  and every provider fetch records what it offered, stored and dropped, with
  how long it took.

  Reading the body a chunk at a time is what makes the byte count possible, and
  it fixes something else on the way: a response that lied about its length, or
  declared none, is now refused at the moment it crosses the 100 MiB limit
  rather than after all of it has been buffered.

- The teaching catalogues are part of Wilkes rather than a route nobody called.
  Settings › Catalogues says what the mirror holds per provider, when each was
  last fetched, and syncs them one at a time so a five-minute refresh reports
  each catalogue as it lands instead of going quiet; a provider that fails is
  reported next to the three that did not. A catalogue pane searches the mirror
  and adds what it finds — fetched into the workspace's uploads directory and
  imported from there, so nothing is written into a library root behind the
  user's back — and a record the provider does not serve whole offers its
  landing page instead of an Add button that would fail. A search of your own
  library that returns nothing now offers what the catalogues hold on the same
  question, in your own words, fetching nothing until you ask; a search that
  returns something offers the same in one line.

  The operations moved to `wilkes_api::commands::catalogue`, which is why any
  of this is possible: the sync loop and the provider registry walk used to
  live inside an axum handler, so the desktop shell had nothing to call. The
  HTTP routes are now wrappers over the same four operations the Tauri commands
  call.

  Recall is presented as recall. The order is a text match and every surface
  says so — ranking these by which is the better place to start needs the
  library-coverage measure that is not built yet.

- The managed corpus lifecycle is addressed at `/api/corpora/*`, and
  `/api/integrations/underdog/` is gone. A route path that names one consumer
  claims that surface belongs to it; a corpus belongs to whichever application
  created it, and `owner` — which the ensure request now carries — is where
  that is said. Nothing about the corpus contract changes: the six routes,
  their shapes and their refusals are what they were, at paths that describe
  what they do rather than who asked.

- Passages are addressed at `/api/chunks/{resolve,accumulate,similarity,search}`
  with one vocabulary: a `scope` naming the index and stable `ChunkRef`s naming
  the passages. Every one of these existed twice — once taking a corpus id and
  a pinned embedding space, once taking a workspace id and SQLite rowids — and
  the second vocabulary was never safe for a caller that stored anything, since
  a rowid is reissued when its file is re-indexed. `/api/embed/centroid`,
  `/api/embed/similarity` and `/api/export/chunk-text` are deleted, and
  `/api/embed/text` and the `/api/export/*` routes take the same `scope`.

  Refs work on an ordinary workspace, not only a managed corpus: the indexing
  path already writes them. An index built before they existed refuses with
  `INDEX_IDENTITY_UNVERIFIED`, naming a rebuild, rather than answering with
  nulls that read like passages. `/api/export/chunks` therefore names its
  chunks by `chunk_ref` and carries `text_sha256`, in the same shape
  `chunks/resolve` returns plus the vector — one definition of what a chunk
  looks like on the wire.

  `accumulate` returns the unnormalized sum of individually L2-normalized
  members and their count, and the normalized mean `/api/embed/centroid`
  returned is gone: the mean is derivable from the sum and the reverse is not,
  so a caller partitioning a large group across requests adds the sums and
  normalizes exactly once. `resolve` caps at 512 refs, reconciling a 64 that
  was sized for displaying a passage against an 8,192 borrowed from an
  operation that returns two scalars.

- One answer about what this build can embed with, at
  `GET /api/embed/capabilities`. It replaces `/api/embed/engines`,
  `/api/embed/models`, and the consumer's own model list: the UI's picker used
  to assemble a model from two replies and then merge the user's hand-added
  entries itself, which is how a picker and a backend come to disagree about
  which models exist. `EmbedderCapability` gains `is_default` and
  `is_recommended` so the picker has everything it displayed before, and its
  two load-bearing nulls are unchanged — `dimension` is null for a model whose
  width only a first load reveals, and `prefix_source` says whether anything
  has established the model's prefixes at all.

- The catalogue mirror is addressed at `/api/catalogue/*` rather than under a
  consumer's name. Reading the open teaching catalogues has nothing to do with
  which application asked, and a route path that names one consumer is a claim
  about ownership that was never true.

- A managed corpus can carry more than one embedding space.
  `PUT /api/corpora/spaces` adds a projection of an existing
  corpus under a second model: it reads that corpus's admitted renditions and
  computes only its own vectors, so the source is retained once, extracted
  once, and every space shares the same rendition ids and chunk refs. The
  projection is an internal workspace — it is not listed beside the user's own
  and is never a corpus id callers address. Corpus status now reports a
  `corpus_generation` and one entry per space; an import brings every space to
  the document it admitted, and a space that has not indexed the current
  generation is refused with `EMBEDDING_SPACE_STALE` rather than answering from
  membership it lacks. A model that is unavailable does not decide whether a
  document is in the corpus: its own projection is left behind and unservable,
  the other spaces still follow, and catching up is idempotent from the
  canonical renditions, so the work is owed rather than lost.

- A local mirror of the open teaching catalogues — LibreTexts, OpenStax, MIT
  OpenCourseWare and DevDocs — with BM25 search over it, at
  `POST /api/catalogue/{search,sync}` and `GET /api/catalogue/status`. These
  catalogues are small enough to hold whole, which is what makes searching them
  locally possible; papers are not, and literature search is unchanged. Search returns *recall*, not a ranking:
  deciding which of these teaches a particular reader best needs to know what
  that reader already knows, which is not a fact about documents. Sync reports
  what each provider offered against what was stored, because both LibreTexts
  and MIT OpenCourseWare repeat ids across a paged fetch, and LibreTexts keeps
  yielding new records well past the `numTotal` it reports.

- A catalogue search query names the *set* of grains it will accept (`grains`)
  rather than one. Which kinds of source could answer a question is a judgement
  about the question and usually admits more than one; filtering to the single
  preferred kind hid every provider publishing at another grain, and on this
  mirror only one provider publishes courses — so a query for a broad subject
  came back entirely from it, with no textbook on the subject anywhere in the
  answer.

- MCP tools accept a `workspace` id and read that workspace's library — its
  roots, access boundary and index — without switching the app to it.
  `list_context` reports every workspace and marks the active one; omitting
  `workspace` reads the active workspace as before. The external listener
  resolves its workspace per call, so switching workspaces in Wilkes no longer
  restarts it.
- Search matches cached author alongside filename and title. Author hits are
  reported as `kind='author'` over MCP and labelled `Author` in the result
  list.

### Fixed

- A document indexed by the live file watcher now carries the same stable
  passage identities a build writes. The watcher called `write_file` where the
  build calls `write_file_with_recipe`, so a file edited while Wilkes was
  running landed with no `chunk_ref` — passages that could not be named,
  indistinguishable from passages that do not exist to every route that
  addresses one by ref.

- Downloading the same content to the same name reports it as already present
  instead of refusing. The name was checked before the request, so a fetch that
  succeeded and failed downstream left a file that made every retry impossible
  — the second attempt would have written exactly the same bytes. Different
  content under the same name is still refused and the existing file is still
  never overwritten.

- A download whose URL ends without a file extension is named from the
  server's content type instead of being saved under a name nothing can type.
  LibreTexts serves whole books from `.../download/<id>/pdf`, which previously
  produced a file called `pdf` that the managed importer then refused. An
  unrecognised content type is reported rather than guessed at.

### Changed

- `catalogue/search` returns the `terms` each query reduced to alongside its
  hits. An empty result had two causes a caller could not tell apart — nothing
  matched, or nothing in the query survived stopword and length filtering, as
  happens to any single-letter term — and only the store knew which. The UI now
  says "nothing in that could be searched for" where it used to imply the
  catalogues were empty.

- The Data settings page names two paths where it named one. What it labelled
  the application's data directory was the active workspace's directory inside
  it, so the installation root — which holds `workspaces/`, the model cache and
  the catalogue mirror — could not be seen from the page at all, and the
  workspace path was shown under a name that belonged to its parent.
  `DataPaths.app_data` is now that root and `DataPaths.workspace` is the
  workspace, each with its own button.

- The catalogue mirror is one per installation, in `shared_data_dir/catalogue`
  beside the model cache, rather than one per workspace. Its rows are what four
  public catalogues publish, which is the same answer whichever workspace asks:
  per-workspace, an installation paid for a sync once per workspace, and a
  consumer that synced under one workspace found an empty mirror under the
  next. Existing per-workspace `catalogue.db` files are not migrated or read —
  the mirror is refetchable by definition, so the first `/api/catalogue/sync`
  refills the shared one — and they can be deleted.

- `catalogue_records.grain` no longer carries a CHECK constraint listing the
  grains this build knows. SQLite cannot drop a CHECK in place, so the three
  variants in the schema were three variants no existing database could be
  talked out of: a fourth grain would have needed the table rebuilt anyway.
  Opening a v1 database rebuilds it once, carrying the rowids across because
  the FTS index is keyed by them. The column's domain is the Rust type's to
  state, and a `grain` no variant covers is now an error naming the value
  rather than a row served as a textbook.

- A pre-workspace ("alpha") installation is migrated into its Default workspace
  by the application, on the start that finds it. The library, its databases'
  companion files and the roots the settings file had open move into one
  workspace; the roots and semantic block that the manifest now owns are
  dropped from global settings, so nothing answers "what is open" twice. The
  migration is resumable: the workspace it commits to is recorded before the
  first file moves, and the registry — what makes the workspace real — is
  written only after the last one lands. It refuses, having moved nothing, when
  the same library file exists in both the data and the config directory, since
  only the user knows which one is theirs.

  The startup screen that used to ask for this, and `scripts/migrate_workspace.py`
  that it told the user to run, are gone. The migration is mechanical — the
  whole library becomes one workspace and there is nothing to decide — so
  asking cost every alpha user a manual step to reach a state the app could
  reach itself, and left an install able to answer the question twice: run the
  script, or start a build that would create a fresh empty registry beside the
  library. The startup gate itself stays, with no feature contributing a
  blocker today; an unexpected startup failure is still reported through it.

- An application-managed workspace — Underdog's semantic corpus — is now listed,
  activatable and searchable by the user instead of being hidden from the
  workspace listing. Hiding it protected the corpus by making it unreachable,
  which cost the reads as well as the writes: its documents sit on the user's
  own disk and there was no way to look at them. The listing reports
  `read_only` and `managed_by` (also on the MCP `list_context` workspaces), and
  the protection is stated on the writes instead — rename, import, save,
  directory create/move/trash, index build and index delete, `/api/upload` and
  the `download` MCP tool are all refused. The managed import API keeps its own
  path and remains the corpus's only writer.
- The downloader behind the `download` MCP tool moved to `wilkes-core` so the
  catalogue acquisition route shares it. One set of answers to may-this-be-
  fetched, where-may-it-land and is-this-already-here.
- PDF extraction produces one canonical reading of a document instead of a
  transcription of its layout: words the typesetter broke across a line are
  joined on the document's own vocabulary, repeating page numbers and running
  heads are removed, and margin boxes are moved after the page they annotate
  rather than left in the middle of the sentence they interrupt. Search,
  embeddings, `get_document_text`, grep context and the managed corpus export
  all read that text.
- PDF outline entries carry a byte offset into that reading, resolved from the
  bookmark's own destination coordinate where the document has one, and report
  which rung of the resolution ladder answered.
- `PdfSearchProjection` no longer repairs line-wrap hyphenation: the reading it
  matches against no longer contains any. A query pasted out of a PDF viewer
  still matches across the break the viewer showed.
- `EXTRACTOR_RECIPE_VERSION` is `wilkes-extractors-v2`. Every managed document
  re-extracts and re-embeds; legacy indexed files re-index on their normal path.

## 0.9.5 - 2026-04-20

### Added

- Metadata extraction for documents (DOI, author, date).
- External links (Google Scholar) in viewer metadata.
- Context menus for file and directory rows.
