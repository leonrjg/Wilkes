import { useEffect, useRef, useState } from "react";
import { AnnotationMode } from "pdfjs-dist";
import type { PDFDocumentProxy, PDFPageProxy, RenderTask } from "pdfjs-dist";

/**
 * Renders one page's canvas.
 *
 * This is a deliberate transcription of what `react-pdf`'s `<Page>` did with
 * the props this reader passed it, not a fresh design: the reader's current
 * rendering *is* the specification, so every quirk below is load-bearing until
 * someone changes it on purpose.
 *
 * What is preserved, and why each matters:
 *
 *   * **The device-pixel-ratio is not capped.** `window.devicePixelRatio || 1`,
 *     exactly as before. Capping it (at 2, say) is defensible for memory, but
 *     it would soften every page on a 3x display -- a visible change, not a
 *     cleanup.
 *   * **`alpha: false` plus an explicit white `background`.** An opaque canvas
 *     keeps PDF composition stable; a transparent one lets blend modes
 *     composite against whatever is behind the canvas.
 *   * **`annotationMode: ENABLE`.** The reader turns off the *DOM* annotation
 *     layer and draws its own links, but annotation appearances are still
 *     baked into the canvas. Passing DISABLE here would silently drop stamps,
 *     highlights and form field appearances from rendered pages.
 *   * **Hidden until rendered.** The canvas is sized, hidden, drawn, then
 *     revealed, so a partially painted page is never shown. `canvasBackground`
 *     is the *render* background, not the wrapper's: the wrapper is transparent
 *     (see `.pdf-page` in the app stylesheet), so what shows through meanwhile
 *     is the reader's own background, as it always did.
 *   * **Remount on scale change.** The canvas element is keyed by scale, so a
 *     zoom discards the old element rather than redrawing into it.
 *   * **`page.cleanup()` before each render**, so a re-render starts from
 *     scratch rather than inheriting prior state.
 *
 * Dropped deliberately: react-pdf set `--scale-factor`, `--total-scale-factor`
 * and `--user-unit` on the page wrapper. Nothing reads them -- the text layer
 * sets its own on the builder's div, which is not a descendant of this element.
 */

interface PdfPageCanvasProps {
  pdf: PDFDocumentProxy;
  pageNumber: number;
  /** Target CSS width in px. The page is scaled to meet it. */
  width: number;
  canvasBackground?: string;
  onRenderSuccess?: () => void;
  onRenderError?: (error: unknown) => void;
}

function isCancellation(error: unknown): boolean {
  const name = (error as { name?: string } | null)?.name;
  return name === "AbortException" || name === "RenderingCancelledException";
}

export default function PdfPageCanvas({
  pdf,
  pageNumber,
  width,
  canvasBackground,
  onRenderSuccess,
  onRenderError,
}: PdfPageCanvasProps) {
  const [page, setPage] = useState<PDFPageProxy | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setPage(null);
    setLoadFailed(false);
    pdf.getPage(pageNumber).then(
      (nextPage) => {
        if (!cancelled) setPage(nextPage);
      },
      (error) => {
        if (cancelled) return;
        console.error(`PDF page ${pageNumber} failed to load:`, error);
        setLoadFailed(true);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [pdf, pageNumber]);

  const rotate = page?.rotate ?? 0;
  const scale = page ? width / page.getViewport({ scale: 1, rotation: rotate }).width : 1;

  return (
    <div
      // Deliberately not `data-page-number`: the reader's virtualized wrapper
      // carries that, and it is the element every geometry reader in PdfViewer
      // actually resolves to -- `closest()` from the text layer, which is a
      // sibling of this element rather than a descendant, and `querySelector`,
      // which takes the first in document order. This element carried a second
      // copy only because react-pdf emitted one, leaving `querySelectorAll`
      // with two candidates per page whose centres differ by half the page gap.
      // The class is the styling hook (`.pdf-page canvas`, and the dark-mode
      // inversion); identity belongs to the wrapper.
      className="pdf-page"
      style={{
        position: "relative",
        minWidth: "min-content",
        minHeight: "min-content",
      }}
    >
      {page ? (
        <PageCanvas
          key={`${pageNumber}@${scale}/${rotate}`}
          page={page}
          scale={scale}
          rotate={rotate}
          canvasBackground={canvasBackground}
          onRenderSuccess={onRenderSuccess}
          onRenderError={onRenderError}
        />
      ) : (
        <div className="pdf-page-message">
          {loadFailed ? "Failed to load the page." : "Loading page…"}
        </div>
      )}
    </div>
  );
}

function PageCanvas({
  page,
  scale,
  rotate,
  canvasBackground,
  onRenderSuccess,
  onRenderError,
}: {
  page: PDFPageProxy;
  scale: number;
  rotate: number;
  canvasBackground?: string;
  onRenderSuccess?: () => void;
  onRenderError?: (error: unknown) => void;
}) {
  const canvasElement = useRef<HTMLCanvasElement>(null);
  // Read from the render effect without making it re-run when a caller passes a
  // fresh closure; re-running would cancel and repaint the page for nothing.
  const callbacksRef = useRef({ onRenderSuccess, onRenderError });
  callbacksRef.current = { onRenderSuccess, onRenderError };

  // Release the backing surface when this canvas goes away. Zeroing the
  // dimensions makes browsers drop it immediately; without it, a document
  // scrolled end to end holds every canvas it ever painted, and at device
  // resolution those are megabytes each.
  //
  // The element is captured here at setup rather than read from the ref in the
  // cleanup, which is what react-pdf did: under React 19 the host ref is
  // already null by the time a passive cleanup runs, so reading it there finds
  // nothing and the release silently never happens.
  useEffect(() => {
    const canvas = canvasElement.current;
    return () => {
      if (!canvas) return;
      canvas.width = 0;
      canvas.height = 0;
    };
  }, []);

  useEffect(() => {
    const canvas = canvasElement.current;
    if (!canvas) return;

    // Start from scratch; otherwise prior page state (form data included)
    // survives into the new render.
    page.cleanup();

    const devicePixelRatio = window.devicePixelRatio || 1;
    const renderViewport = page.getViewport({
      scale: scale * devicePixelRatio,
      rotation: rotate,
    });
    const viewport = page.getViewport({ scale, rotation: rotate });

    canvas.width = renderViewport.width;
    canvas.height = renderViewport.height;
    canvas.style.width = `${Math.floor(viewport.width)}px`;
    canvas.style.height = `${Math.floor(viewport.height)}px`;
    canvas.style.visibility = "hidden";

    const canvasContext = canvas.getContext("2d", { alpha: false });
    if (!canvasContext) return;

    let task: RenderTask | null = page.render({
      annotationMode: AnnotationMode.ENABLE,
      canvas,
      canvasContext,
      viewport: renderViewport,
      ...(canvasBackground ? { background: canvasBackground } : {}),
    });

    task.promise
      .then(() => {
        canvas.style.visibility = "";
        callbacksRef.current.onRenderSuccess?.();
      })
      .catch((error: unknown) => {
        // A cancelled render rejects; that is the normal path when the reader
        // scrolls a page out of view mid-paint, not a failure to report.
        if (isCancellation(error)) return;
        console.error("PDF page render failed:", error);
        callbacksRef.current.onRenderError?.(error);
      });

    return () => {
      task?.cancel();
      task = null;
    };
  }, [page, scale, rotate, canvasBackground]);

  return (
    <canvas
      ref={canvasElement}
      dir="ltr"
      style={{ display: "block", userSelect: "none" }}
    />
  );
}
