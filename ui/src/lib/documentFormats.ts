/**
 * Which documents the backend reads as laid-out pages, and which of those the
 * reader can actually draw.
 *
 * `FileType` answers neither question any more. It says only *how* Wilkes reads
 * a file, and since the MuPDF backend began reading EPUB, MOBI and FB2 as well
 * as PDF, its `"Pdf"` value covers all of them — it is a wire name kept for the
 * saved filters users have written against it, not a format.
 *
 * The distinction that matters here is narrower: `PdfViewer` renders through
 * pdf.js, straight from the file's own bytes, and pdf.js opens exactly one of
 * these formats. Everything else is laid out by MuPDF inside the host, and none
 * of that layout exists in the bytes the webview would fetch — so the backend
 * sends the reading it made instead, and the pane shows that.
 */

/** Extension in lower case, without the dot. Empty when there is none. */
function extensionOf(path: string): string {
  const name = path.toLowerCase().split(/[/\\]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

/** Every extension the backend reads as laid-out pages, mapped to the badge it
 *  is shown under.
 *
 *  The frontend's copy of the backend's `PagedFormat::for_path` and
 *  `PagedFormat::EXTENSIONS`, and the three must agree. A document opened from
 *  the file tree is given its origin from this: get it wrong and the pane asks
 *  for a page-shaped preview and receives a text-shaped one, or the reverse,
 *  and renders nothing at all. */
const PAGED_FORMATS: Readonly<Record<string, string>> = {
  pdf: "PDF",
  epub: "EPUB",
  mobi: "MOBI",
  prc: "MOBI",
  pdb: "MOBI",
  azw3: "AZW3",
  azw: "AZW3",
  fb2: "FB2",
  cbz: "CBZ",
  cbt: "CBZ",
};

/** The documents Wilkes always reads, in the order they are shown.
 *
 *  These are not a preference. `FileType::detect` admits them whatever the
 *  extension setting holds, so the settings panel shows them as always on
 *  rather than as entries a user can remove and be quietly disobeyed. */
export const ALWAYS_ENABLED_DOCUMENT_EXTENSIONS: readonly string[] =
  Object.keys(PAGED_FORMATS);

/** Whether this extension is one of the always-enabled document formats.
 *  Takes a bare extension, with or without its dot. */
export function isDocumentExtension(extension: string): boolean {
  const clean = extension.trim().toLowerCase().replace(/^\./, "");
  return Object.prototype.hasOwnProperty.call(PAGED_FORMATS, clean);
}

/** Whether the backend reads this document as laid-out pages rather than as
 *  text — PDF, EPUB, MOBI, AZW3, FB2 and comic archives. */
export function isPagedPath(path: string): boolean {
  return pagedFormatLabel(path) !== null;
}

/** Whether the reader can draw this document's pages from the file itself.
 *  Only a PDF can; this is the frontend's copy of the backend's
 *  `preview::renders_as_pdf`, and the two must agree. */
export function isPdfPath(path: string): boolean {
  return extensionOf(path) === "pdf";
}

/** The badge for a document read as laid-out pages, or `null` for one read as
 *  plain text. Derived from the extension because `file_type` would label an
 *  EPUB "PDF". */
export function pagedFormatLabel(path: string): string | null {
  const extension = extensionOf(path);
  // An own-property check rather than a bare lookup: an extension like
  // `constructor` would otherwise find something on the prototype and label a
  // text file a book.
  return Object.prototype.hasOwnProperty.call(PAGED_FORMATS, extension)
    ? PAGED_FORMATS[extension]
    : null;
}
