/**
 * How a position in a document is expressed.
 *
 * These are the readers' vocabulary, not the application's: a reader cannot
 * place a decoration, report a selection or navigate anywhere without them, so
 * they belong to the package rather than to whatever host is using it. Wilkes
 * re-exports them from `lib/types` so its own code is unaffected.
 */

/** A rectangle in a PDF page's own coordinate units, origin top-left. */
export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A half-open range in a document's UTF-8 bytes. Byte offsets rather than
 *  UTF-16 ones because they are what the backend persists; each reader converts
 *  at its own boundary. */
export interface ByteRange {
  start: number;
  end: number;
}

/** Where in a document something sits, in whichever coordinate system that kind
 *  of document has. */
export type SourceOrigin =
  | { TextFile: { line: number; col: number } }
  | { PdfPage: { page: number; bbox: BoundingBox | null } };
