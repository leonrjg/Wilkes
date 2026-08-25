# Changelog

## Unreleased

### Added

- A local mirror of the open teaching catalogues — LibreTexts, OpenStax, MIT
  OpenCourseWare and DevDocs — with BM25 search over it, at
  `POST /api/integrations/underdog/catalogue/{search,sync}` and
  `GET .../catalogue/status`. These catalogues are small enough to hold whole,
  which is what makes searching them locally possible; papers are not, and
  literature search is unchanged. Search returns *recall*, not a ranking:
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

- A download whose URL ends without a file extension is named from the
  server's content type instead of being saved under a name nothing can type.
  LibreTexts serves whole books from `.../download/<id>/pdf`, which previously
  produced a file called `pdf` that the managed importer then refused. An
  unrecognised content type is reported rather than guessed at.

### Changed

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
