import { installReadableStreamAsyncIterator } from "./readableStreamAsyncIterator";
import type { PDFDocumentProxy } from "pdfjs-dist";

export interface PositionedPdfText {
  text: string;
  hasEOL: boolean;
  direction: string;
  x: number;
  top: number;
  width: number;
  height: number;
  /** True when advancing through the string primarily moves horizontally. */
  horizontal: boolean;
}

const pageTextCache = new WeakMap<
  PDFDocumentProxy,
  Map<number, Promise<PositionedPdfText[]>>
>();

/**
 * Convert one PDF.js text item to the same top-left page coordinate space used
 * by the viewer's highlight overlays. The axis-aligned envelope remains valid
 * for cropped and rotated pages; substring trimming is limited to horizontal
 * items by the caller.
 */
export function positionPdfTextItem(
  viewportTransform: number[],
  item: {
    str: string;
    hasEOL?: boolean;
    dir?: string;
    transform: number[];
    width?: number;
    height?: number;
  },
): PositionedPdfText {
  const [v0, v1, v2, v3, v4, v5] = viewportTransform;
  const [t0, t1, t2, t3, t4, t5] = item.transform;
  const [a, b, c, d, e, f] = [
    v0 * t0 + v2 * t1,
    v1 * t0 + v3 * t1,
    v0 * t2 + v2 * t3,
    v1 * t2 + v3 * t3,
    v0 * t4 + v2 * t5 + v4,
    v1 * t4 + v3 * t5 + v5,
  ];
  const advanceScale = Math.hypot(a, b);
  const advanceX = advanceScale > 0 ? a / advanceScale : 1;
  const advanceY = advanceScale > 0 ? b / advanceScale : 0;
  const width = Math.abs(item.width ?? 0);
  const fallbackHeight = Math.abs(item.height ?? Math.hypot(c, d));
  const verticalX = Number.isFinite(c) ? c : 0;
  const verticalY = Number.isFinite(d) ? d : -fallbackHeight;
  const corners = [
    [e, f],
    [e + advanceX * width, f + advanceY * width],
    [e + verticalX, f + verticalY],
    [e + advanceX * width + verticalX, f + advanceY * width + verticalY],
  ];
  const xs = corners.map(([x]) => x);
  const ys = corners.map(([, y]) => y);
  const left = Math.min(...xs);
  const right = Math.max(...xs);
  const top = Math.min(...ys);
  const bottom = Math.max(...ys);

  return {
    text: item.str,
    hasEOL: item.hasEOL === true,
    direction: item.dir ?? "ltr",
    x: left,
    top,
    width: Math.max(right - left, 0.5),
    height: Math.max(bottom - top, fallbackHeight, 0.5),
    horizontal: Math.abs(advanceX) >= Math.abs(advanceY),
  };
}

/** Load and position a page's PDF.js text items once per open document. */
export async function loadPdfPageText(
  pdf: PDFDocumentProxy,
  pageNumber: number,
): Promise<PositionedPdfText[]> {
  let documentPages = pageTextCache.get(pdf);
  if (!documentPages) {
    documentPages = new Map();
    pageTextCache.set(pdf, documentPages);
  }
  const cached = documentPages.get(pageNumber);
  if (cached) return cached;

  const promise = (async () => {
    installReadableStreamAsyncIterator();
    const page = await pdf.getPage(pageNumber);
    const viewport = page.getViewport({ scale: 1 });
    const content = await page.getTextContent({ disableNormalization: true });
    return content.items.flatMap((item) =>
      "str" in item
        ? [
            positionPdfTextItem(viewport.transform, {
              str: item.str,
              hasEOL: item.hasEOL,
              dir: item.dir,
              transform: item.transform,
              width: item.width,
              height: item.height,
            }),
          ]
        : [],
    );
  })();
  documentPages.set(pageNumber, promise);
  promise.catch(() => documentPages?.delete(pageNumber));
  return promise;
}
