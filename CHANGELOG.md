# Changelog

## Unreleased

### Changed

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
