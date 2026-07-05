/** Session-scoped memory of where the reader was left in each PDF, so closing a
 *  document and reopening it during the same app run lands at the same place.
 *
 *  Deliberately in-memory only (a module-level map, not persisted): the position
 *  is meant to survive the reader unmounting/remounting — closing the preview,
 *  switching files and coming back — but reset on app restart. Keyed by the
 *  resolved PDF URL, the same identity used to key the `PdfViewer` instance.
 *
 *  This module owns storage only; capturing/restoring the geometry is the
 *  reader's responsibility (it alone knows the DOM layout). */
/** The vertical anchor of the viewport, independent of zoom/width. */
export interface PdfScrollAnchor {
  /** 1-based page at the top of the viewport. */
  page: number;
  /** Fraction (0..1) of that page's height scrolled past its top. Stored as a
   *  ratio rather than pixels so it survives the reader's zoom/width changes. */
  offsetRatio: number;
}

export interface PdfScrollPosition extends PdfScrollAnchor {
  /** The reader's zoom when last here. Restored together with the anchor so
   *  reopening a document does not re-run auto-zoom (which, applied after the
   *  anchor is restored, would grow page heights and shift the position up). */
  zoom: number;
}

const positions = new Map<string, PdfScrollPosition>();

export function savePdfScrollPosition(url: string, position: PdfScrollPosition): void {
  positions.set(url, position);
}

export function readPdfScrollPosition(url: string): PdfScrollPosition | null {
  return positions.get(url) ?? null;
}
