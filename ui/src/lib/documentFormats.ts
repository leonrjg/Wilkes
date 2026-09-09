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
  switch (extensionOf(path)) {
    case "pdf":
      return "PDF";
    case "epub":
      return "EPUB";
    case "mobi":
    case "prc":
    case "pdb":
      return "MOBI";
    case "azw3":
    case "azw":
      return "AZW3";
    case "fb2":
      return "FB2";
    case "cbz":
    case "cbt":
      return "CBZ";
    default:
      return null;
  }
}
