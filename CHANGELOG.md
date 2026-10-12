# Changelog

## Unreleased

### Added

- Literature requests are decoded once against the selected providers' merged
  controls, then translated and checked in each provider's own terms. Manifests
  can tag shared filters and sort orders with `equivalent_to` and translate
  option values with `equivalent_values`; OpenAlex declares the shared tags.
  Unsupported parts are reported per provider; a decode failure fails the
  whole request.

- A manifest can list where a file may be, in order of preference:
  `url = ["{pdf_url}", "{download_url}"]` under `resolve_download`. The first
  template whose values are all known is the URL, and the steps run only while
  none can be written — so a result that already carries a link, such as an
  open-access PDF, is downloaded from it without a resolver request or a
  credential being spent, and every other result is resolved as before. A list
  holds two to four templates. A single template is unchanged: all its steps
  run. Which template is used is decided by what is known, never by whether a
  download worked, and a `filename` that names a step's value needs that step
  to have run.
- A download step can say that a missing value means "no file". A
  `resolve_download` step field written with `missing = "unavailable"` —
  `md5 = { path = "a[href*='md5=']", attribute = "href", missing = "unavailable" }`
  — makes a selector that finds nothing the answer that the service holds no
  file for that result, reported as the provider not providing one, instead of
  `field 'md5' matched nothing`. Unsaid, a field that matches nothing is still
  an error, and a value that is found and does not fit (a capture that does not
  match) is an error either way. A probe whose example result has no file says
  the resolver was not checked rather than that it failed.
- Custom integrations can extend a built-in or custom provider's capabilities and declare only what they add. Use the "Extend" button on a provider's tab or row to get started.
- With a generation model ready, describe a literature search in a sentence and press the lightning button to set filters, sort order and keywords. Anything the selected providers cannot apply is reported.
- LibreTexts, OpenStax, MIT OCW and DevDocs can be synced locally, searched in the literature pane and added to the workspace. Manage them in Settings › Integrations › Catalogues; records without a full download offer their landing page. Library searches also suggest catalogue matches. **Breaking for desktop IPC:** `catalogue_status` and `catalogue_search` are removed; HTTP routes are unchanged.
- The sidebar remembers closed folders per workspace across root switches and restarts. New folders open by default.
- Learning catalogues can be removed in Settings › Integrations › Catalogues and restored by syncing again. Refresh updates only installed catalogues.
- Literature providers can declare more filter types and combine values with "Any of", "All of" or "None of". OpenAlex now offers 48 filters, including author and institution suggestions, citation relationships and exclusions. An "Add a filter" menu keeps large filter sets manageable. **Breaking for manifests:** `browse` filters using `kind` must be updated; ranges use two filters sharing a `group`.
- Keyword searches in the literature pane have a "Load more" button per provider. Custom providers can enable pagination with `{page}` or `{offset}` in `search.path`.
- The literature pane supports browsing by filters and sort order, with or without keywords. Each selected provider has its own controls and removable filter chips. Manifests can also declare DOI lookup and reference retrieval.
- Readers remember page, scroll position, zoom and source/rendered mode across restarts, for up to 500 documents per workspace. Moved or renamed documents start at the beginning; standalone windows do not save reading positions.
- EPUB, MOBI, AZW3 and FB2 books, plus CBZ/CBT comic archives, can be read, searched and indexed alongside PDFs, with bookmarks, highlights and tables of contents where supported. Comic text requires a configured recognizer. AZW3 layout is approximate and internal Kindle links are unavailable; incomplete text extraction is reported. Existing PDFs do not need reindexing.
- Workspaces keep separate library roots, indexes, research data and saved sessions. Create, rename and switch workspaces from the workspace picker.
- Libraries can include multiple roots, with whole-library and global search across them.
- Save document selections as bookmarks with notes, revisit highlighted passages and copy quotes as Markdown. Related bookmarks can be grouped by meaning.
- Organize documents with tags and saved smart collections, use collections to scope searches, and revisit search history.
- Zotero integration looks up documents in the local Zotero library, copies formatted citations and adds documents to Zotero.
- OpenAlex and Semantic Scholar provide bibliographic enrichment and literature search, available from the literature pane and chat tools.
- In-app chat gives an agent access to the active document, attached files and Wilkes document and literature tools.
- An external MCP server exposes Wilkes tools to other clients, with configurable network binding and optional bearer authentication. HTTP API and MCP controls are grouped under Settings › Servers; the desktop can also expose the HTTP API.
- MCP tools can read bookmarks, tags, smart collections and search history, and edit research data, rename files or refresh metadata. Edits preserve research links and refresh the UI. MCP is also available at `/mcp` on the shared HTTP API.
- Local generation with Candle or Ollama provides document summaries, cited search summaries, bookmark and topic labels, and explanations of related documents.
- Text documents can be edited with inline completions grounded in the library, pinned sources, source provenance and an inspectable context budget.
- Semantic search supports optional HyDE query expansion and automatic relevance feedback from top results.
- Topic clouds let you explore related passages across a library or within one document. Related-document suggestions and a citation graph offer more ways to explore connections.
- Open multiple documents in viewer tabs and restore the session after restarting.
- Markdown has rendered and source views. Text readers support find, zoom, selections and bookmarks; PDF readers support tables of contents, internal links and destination previews on hover.
- The sidebar includes a file tree, file sorting and configurable metadata columns. Files can be imported by pasting.
- Custom providers can be added from manifests, validated and tested before enabling. Copyable authoring instructions help an LLM write a manifest; download resolvers can contact explicitly declared origins.
- Indexing can recognize scanned pages, images, formulas and tables, with page layout detection and specialized readers. Formula readers include Texify, PP-FormulaNet and UniMERNet.
- Applications can export document outlines and figure inventories, find figures associated with passages and retrieve their images through `/api/export/outline`, `/api/export/figures`, `/api/chunks/figures` and `/api/figure`.
- Applications can import documents into managed corpora with retained source copies and stable passage identities. Managed corpora support backup and restore through `/api/corpora/backup` and `/api/corpora/restore`.

### Changed

- A manifest's `browse.path` now says where everything the user may vary is
  written: `{query}`, `{sort}`, any filter by name, and `{filters}` for the
  rest, beside `{limit}` and `{page}` or `{offset}`. A query parameter with
  nothing set is left off, so a service with one parameter per filter can be
  described, including a range in one parameter (`year={year_from}-{year_to}`).
  `query_required = true` declares a listing that needs a keyword. This replaces
  `query_param`, `filter_param` and `sort.param`, which are no longer read: a
  manifest written against this development branch's earlier browse needs those
  three folded into its path.
- Semantic Scholar offers filters: year range, publication type, field of
  study, venue, minimum citations and public PDF, beside a keyword.
- The manifest authoring prompt asks for everything the service offers that the
  schema can express: every capability, paging, every result field (including a
  value held inside a longer string) and every filter and sort. `UNSUPPORTED:`
  is now returned only when search itself cannot be expressed; anything else
  that does not fit is named in `# NOT REPRESENTED:` comment lines at the top of
  the manifest instead of being dropped silently.
- OpenAlex and Semantic Scholar use the same manifest engine as custom integrations. Custom providers with DOI lookup can enrich document metadata. Existing settings and manifests migrate automatically to format version 2; stored manifest comments are lost, and conversion failures are reported.
- Document extraction names now cover paged formats beyond PDF. Serialized file types and saved collection filters remain compatible.
- Right-clicking empty space in the sidebar creates a folder at that location, with an inline name field.
- Files can be dragged from the sidebar onto another root in the top strip to move them there. The strip scrolls when the drag reaches either end.
- Files dragged in from the file manager are imported into the folder under the pointer, with highlighting, auto-scroll and automatic opening of closed folders.
- Advanced › Data can delete workspaces, including managed corpora and their projections, without deleting source documents. The last workspace cannot be deleted. Also available through `DELETE /api/workspaces/{id}`.
- The root strip shows whether each root is fully indexed or needs updating, with document counts in the tooltip. API clients can use `POST /api/embed/coverage`.
- Applications can retrieve retained copies of managed-corpus documents through `POST /api/corpora/documents/snapshot`. Missing snapshots report `DOCUMENT_INDEX_INCOMPLETE`.
- Documents opened through the operating system use a standalone reader. Other applications can also use `wilkes://open?path=...` links; add a workspace for the main window, and `page` or `line` for a position. Invalid destinations are reported.
- Settings › Activity is now Workers, showing the current document, indexing stage, saved work, extraction failures and worker diagnostics. Progress survives restarts, and unfinished runs are marked as interrupted.
- Interrupted index builds keep completed documents and offer to continue unfinished work. Failed documents have a separate retry action.
- Combined search finds both exact wording and related passages, and labels each result's evidence. It replaces the Semantic checkbox and is the default for MCP `search`. Regular expressions use exact search; if semantic search is unavailable, exact results are returned with a note.
- Dropping folders onto the window adds them as library roots. Mixed drops also import files into the root that was active when the drop began.
- Viewer tab context menus offer "Close" and "Close All".
- HTML files open rendered, with a source toggle, find, zoom, bookmarks and highlights. Local images and document links work; scripts, author styles and remote images are blocked.
- Catalogue syncs show page and record counts; downloads show byte progress. Downloads exceeding 100 MiB are stopped while being received.
- **API change:** managed-corpus routes move from `/api/integrations/underdog/` to `/api/corpora/*`. Ensure requests include `owner`.
- **API change:** passage operations use `/api/chunks/{resolve,accumulate,similarity,search}`, a shared `scope` and stable `ChunkRef`s. `/api/embed/centroid`, `/api/embed/similarity` and `/api/export/chunk-text` are removed; embedding and export routes use `scope`. Chunk exports include `chunk_ref` and `text_sha256`; older indexes require a rebuild. `accumulate` returns a sum and count, and `resolve` accepts 512 refs.
- **API change:** `GET /api/embed/capabilities` replaces `/api/embed/engines` and `/api/embed/models`, including default and recommended model flags.
- **API change:** catalogue routes move to `/api/catalogue/*`.
- Managed corpora can use multiple embedding models without duplicating source documents or extraction. Add a space with `PUT /api/corpora/spaces`; unavailable or outdated spaces must catch up before serving results.
- Catalogue searches accept multiple source grains through `grains`, allowing courses, textbooks and other kinds of material in one query.
- MCP tools can read a specific `workspace` without switching the app. `list_context` lists workspaces and marks the active one; switching no longer restarts the external listener.
- Search matches cached authors as well as filenames and titles, with author matches labelled in the results.
- General OCR can be disabled independently of formula and table recognition. At least one reader must remain enabled. Changing reader roles causes affected documents to be re-extracted.
- Switching roots shows indexing coverage instead of starting an index build. Builds are started explicitly from Settings › Semantic.
- `/api/catalogue/search` returns the searchable `terms`. The UI explains when a query contains none instead of implying that the catalogues are empty.
- Data settings show separate application-data and active-workspace paths, each with its own open button.
- The catalogue mirror is shared across workspaces. Existing workspace mirrors are not migrated; sync once to populate the shared mirror.
- Catalogue databases accept new source grains without a schema constraint update. Existing databases migrate automatically; unknown grains are reported.
- Pre-workspace installations migrate automatically into the Default workspace on startup. Migration can resume after interruption; conflicting library files are reported before anything moves. The manual migration script is removed.
- Application-managed workspaces are visible and searchable. Their contents remain read-only to users, and listings include `read_only` and `managed_by`.
- Catalogue acquisition and MCP `download` share the same download handling.
- PDF extraction joins words split across lines, removes repeated headers and page numbers, and places margin notes after the page text. Search, embeddings and document exports use the cleaned text; queries copied from PDF viewers still match across line-wrap hyphenation.
- PDF outlines include offsets into the extracted text and report how each destination was resolved.
- Extraction recipe `wilkes-extractors-v2` re-extracts and re-embeds managed documents. Legacy indexed files update through their normal reindexing path.
- Shipped and generated manifests use one line per entry. Existing custom manifests do not need to change their layout.
- Literature results show year, citations and open-access status before authors and venue, keeping key facts visible with long author lists.
- **API changes:** document metadata uses a `providers` map. Desktop provider commands are replaced by `literature_provider_status`, `literature_provider_lookup` and `literature_browse`; HTTP clients use `/api/literature/status`, `/api/literature/lookup` and `/api/literature/browse`.
- **Breaking MCP change:** literature downloads use `provider` and `result_id`, with file URLs resolved by Wilkes. Search results report `downloadable` instead of direct file URLs, and `literature_resolve_download` is removed. User-supplied URLs remain supported by `download`.
- Supported document formats are always enabled. Extension settings configure text formats separately.
- Window and browser-tab titles identify the active document, root and workspace.

### Fixed

- Ollama models using its MLX runner work for generation tasks even when the runner ignores output constraints. Replies are still validated.
- Ollama output constraints handle multiline text and quotation marks correctly.
- Cancelling an index build completes within three seconds. Completed documents are saved in the background and remain available for continuation.
- Searching an unindexed root reports that it needs indexing instead of starting a build automatically.
- MCP `search` returns within sixty seconds, with partial matches and `complete: false` if an exact scan reaches its time limit.
- Combined search reports unavailable semantic search in `notes` instead of counting it as an error when exact search succeeds.
- Embedding batch size is configurable under Workers and defaults to 16 to reduce memory pressure. Lower it if indexing causes swapping, or raise it when more memory is available.
- Workspaces share a limit of one worker per kind, preventing duplicate model loads when several workspaces are read at once.
- The topic cloud displays cached names when generation is off. Unnamed topics show passage counts and remain clickable, with an explanation of missing names instead of endless loading placeholders.
- The topic cloud keeps build errors visible and explains when no folder is selected.
- Reading outlines, summaries, citation labels or document text no longer starts recognition. These reads use indexed text where available or the document's own text; text inside images requires indexing.
- Documents indexed by the file watcher receive the same stable passage identities as documents indexed by a build.
- Downloading identical content to an existing filename reports it as already present. Different content is still refused without overwriting the file.
- Downloads whose URLs lack an extension are named using the server's content type. Unrecognized types are reported.
- Missing citation counts no longer appear as "0 citations". Previously cached zeros remain until the document is refreshed.
- Confirmation dialogs work again when clearing logs, deleting an index or switching workspaces with unsaved edits.
- Literature results remain available across MCP requests, so a download following a search can find the selected result.
- Provider rate limits beyond the retry budget are reported promptly instead of leaving requests to time out.
- Provider requests work with TLS-1.3-only services on macOS.
- Provider-supplied download filenames are sanitized and validated before saving.
- Custom-provider validation, health-check and request errors are visible, with bounded retries for transient request failures.
- Exact search reuses indexed text where available, and filename, title and content matches appear together in search results.
- PDF text selection is smoother in WebKit, scanned PDF pages render correctly, and comic and other non-PDF documents open in the appropriate reader.
- The maximum file size setting can be edited normally and saves on blur or Enter.

## 0.9.5 - 2026-04-20

### Added

- Document metadata extraction (title, DOI, author and date), with titles and PDF creation dates shown in the viewer.
- Open and copy a document’s DOI, or look it up on Google Scholar, from viewer metadata.
- Context menus for file and directory rows.

## 0.9.4 - 2026-04-15

### Changed

- Search consistently follows the supported extensions configured in Settings. The separate search extension filter is removed.

### Fixed

- Corrected the Docker container port and improved image publishing reliability.

## 0.9.3 - 2026-04-11

### Added

- Automated publishing of Docker images to GitHub Container Registry.

## 0.9.2 - 2026-04-11

### Fixed

- Workers shut down reliably, including on Windows, and no longer leave requests blocked during shutdown.

## 0.9.1 - 2026-04-11

### Added

- The file list shows omitted files and explains unsupported extensions or size limits.

### Changed

- Candle’s default embedding model is MiniLM-L6 instead of MiniLM-L12.
- Improved PDF page sizing and virtualization.

### Fixed

- Python environment setup no longer opens unwanted console windows on Windows.

## 0.9.0 - 2026-04-10

### Added

- Exact and local semantic search across PDFs and text files, with regex, case-sensitive matching and highlighted results.
- PDF and code viewers with match navigation, plus PDF page controls, zoom and in-document search.
- Fastembed, Sentence Transformers and Candle embedding engines, with custom models, model downloads, configurable chunking and CPU/device selection.
- Library settings, configurable text extensions and file-size limits, logs and worker controls.
- Desktop support for macOS, Windows and Linux, plus a web server and Docker packaging.
