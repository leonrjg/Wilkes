import type { PDFDocumentProxy } from "pdfjs-dist";

/**
 * A PDF GoTo destination as exposed by pdf.js: either a named destination
 * (string, resolved via `getDestination`) or an explicit destination array
 * `[pageRef, {name}, ...params]`.
 */
export type PdfDestination = string | unknown[];

export interface ResolvedDestination {
  /** 0-based index of the target page. */
  pageIndex: number;
  /**
   * Vertical offset of the target within the page, in unscaled (scale-1)
   * top-left PDF-unit coordinates, or `null` when the destination does not
   * pin a specific position (e.g. a plain "Fit" destination). Callers scale
   * this by the page's render scale before adjusting scroll.
   */
  offsetY: number | null;
}

/**
 * Resolve a PDF GoTo destination to a concrete page index and vertical offset.
 *
 * Handles both named destinations (looked up via `getDestination`) and explicit
 * destination arrays. The first array element is a page reference resolved with
 * `getPageIndex`; the destination "mode" (XYZ / FitH / FitBH) carries the `y`
 * used to scroll to the exact position, matching how OS readers land on an
 * anchor rather than the page top.
 */
export async function resolveDestination(
  pdf: PDFDocumentProxy,
  dest: PdfDestination,
): Promise<ResolvedDestination | null> {
  const explicit = typeof dest === "string" ? await pdf.getDestination(dest) : dest;
  if (!Array.isArray(explicit) || explicit.length === 0) return null;

  const pageRef = explicit[0];
  const pageIndex = await pdf.getPageIndex(pageRef as never);

  // Extract the destination `y` for the position-bearing modes. XYZ packs
  // [ref, {name:"XYZ"}, x, y, zoom]; FitH/FitBH pack [ref, {name}, y].
  const mode = (explicit[1] as { name?: string } | undefined)?.name;
  let destY: number | null = null;
  if (mode === "XYZ") {
    destY = typeof explicit[3] === "number" ? (explicit[3] as number) : null;
  } else if (mode === "FitH" || mode === "FitBH") {
    destY = typeof explicit[2] === "number" ? (explicit[2] as number) : null;
  }

  let offsetY: number | null = null;
  if (destY !== null) {
    const page = await pdf.getPage(pageIndex + 1);
    const viewport = page.getViewport({ scale: 1 });
    // convertToViewportPoint maps PDF user space (bottom-left origin) to the
    // top-left viewport space our overlays and scroll math use.
    const [, vy] = viewport.convertToViewportPoint(0, destY);
    offsetY = vy;
  }

  return { pageIndex, offsetY };
}
