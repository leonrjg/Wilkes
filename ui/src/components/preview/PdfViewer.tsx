import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Search as SearchIcon, ChevronUp, ChevronDown, X, List } from "react-feather";
import { Page, pdfjs } from "react-pdf";
import type { BoundingBox } from "../../lib/types";
import { usePdfInnerSearch } from "./usePdfInnerSearch";
import { getScaledPageHeight, usePdfPageMetrics } from "./usePdfPageMetrics";
import PdfTextLayer from "./PdfTextLayer";
import PdfLinkLayer from "./PdfLinkLayer";
import PdfOutline from "./PdfOutline";
import { usePdfOutline } from "./usePdfOutline";
import { resolveDestination, type PdfDestination } from "./pdfDestinations";
import {
  readPdfScrollPosition,
  savePdfScrollPosition,
  type PdfScrollAnchor,
  type PdfScrollPosition,
} from "./pdfScrollMemory";
import { usePdfDocument } from "./pdfDocumentCache";
import { api } from "../../services";
import { Tooltip } from "../Tooltip";

pdfjs.GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

export interface PdfViewerProps {
  url: string;
  page: number;
  highlight_bbox: BoundingBox | null;
  /** Precise per-line rects for the navigation target (bookmarks). When set,
   *  the emphasis is drawn per line instead of over `highlight_bbox`'s union. */
  highlight_rects?: BoundingBox[] | null;
  bookmarkHighlights?: Array<{ id: string; page: number; rects: BoundingBox[] }>;
  onRenderSuccess?: () => void;
  /** Fires (debounced) whenever the page nearest the viewport center changes
   *  -- covers scroll, page-jump, and link/outline navigation alike, since
   *  all of them funnel through `currentPage`. Used to keep the chat pane's
   *  "open document" page badge live as the user reads, not just on the
   *  initial landing page. */
  onPageChange?: (page: number) => void;
  onAddBookmark?: (selection: PdfSelection) => void;
  showChatSelectionActions?: boolean;
  onExplainSelection?: (selection: PdfSelection) => void;
  onAskSelection?: (selection: PdfSelection, question: string) => void;
}

export interface PdfSelection {
  page: number;
  bbox: BoundingBox;
  rects: BoundingBox[];
  quote: string;
}

const PAGE_GAP_PX = 12;
const ZOOM_STEP = 0.1;

// Auto-zoom: bring the dominant body text of a freshly opened document up to a
// comfortable on-screen size. TARGET is the desired CSS-pixel height of body
// text; we only ever enlarge (floor 1.0, so already-comfortable documents are
// left untouched) and cap the enlargement so pathological cases stay sane.
const AUTO_ZOOM_TARGET_PX = 16.5;
const AUTO_ZOOM_MAX = 1.6;
// Deadband: only auto-zoom when it enlarges by at least this factor. Applying a
// near-1.0x zoom still re-renders every page and recentres, which reads as a
// flicker on documents that are already comfortable, for no visible gain.
const AUTO_ZOOM_MIN_INCREASE = 1.05;
const AUTO_ZOOM_SAMPLE_PAGES = 5;
// Reference fit-to-width viewport used to judge body-text size. Using a fixed
// width (rather than the live pane) makes "does this document read small?" a
// deterministic property of the document itself, so the same file auto-zooms
// the same amount regardless of the current window size.
const AUTO_ZOOM_REFERENCE_WIDTH_PX = 900;

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Merge client rects that belong to the same visual text line into one
 *  rectangle. `Range.getClientRects()` can emit several fragments per line
 *  (one per text node); rendering them as separate translucent highlights
 *  would stack into uneven darker bands, so collapse each line first. */
function mergeRectsByLine(rects: BoundingBox[]): BoundingBox[] {
  const sorted = [...rects].sort((a, b) => a.y - b.y || a.x - b.x);
  const lines: BoundingBox[] = [];
  for (const rect of sorted) {
    const last = lines[lines.length - 1];
    const sameLine =
      last && rect.y < last.y + last.height && rect.y + rect.height > last.y;
    if (sameLine) {
      const x1 = Math.min(last.x, rect.x);
      const y1 = Math.min(last.y, rect.y);
      const x2 = Math.max(last.x + last.width, rect.x + rect.width);
      const y2 = Math.max(last.y + last.height, rect.y + rect.height);
      last.x = x1;
      last.y = y1;
      last.width = x2 - x1;
      last.height = y2 - y1;
    } else {
      lines.push({ ...rect });
    }
  }
  return lines;
}

/** Capture the reader's current position as a page + intra-page ratio, reading
 *  live DOM geometry. Returns null when nothing is measurable yet (no rendered
 *  page spans the viewport top), so callers never persist a garbage position. */
function captureScrollPosition(container: HTMLDivElement): PdfScrollAnchor | null {
  const viewportTop = container.getBoundingClientRect().top;
  const pageElements = container.querySelectorAll<HTMLElement>("[data-page-number]");
  for (const pageElement of pageElements) {
    const rect = pageElement.getBoundingClientRect();
    if (rect.height > 0 && rect.top <= viewportTop && rect.bottom > viewportTop) {
      const page = Number(pageElement.dataset.pageNumber);
      if (!page) return null;
      return { page, offsetRatio: (viewportTop - rect.top) / rect.height };
    }
  }
  return null;
}

/** Bounding envelope of a set of rectangles (used as the navigation anchor). */
function unionBox(rects: BoundingBox[]): BoundingBox {
  const x1 = Math.min(...rects.map((r) => r.x));
  const y1 = Math.min(...rects.map((r) => r.y));
  const x2 = Math.max(...rects.map((r) => r.x + r.width));
  const y2 = Math.max(...rects.map((r) => r.y + r.height));
  return { x: x1, y: y1, width: x2 - x1, height: y2 - y1 };
}

export default function PdfViewer({
  url,
  page,
  highlight_bbox,
  highlight_rects = null,
  bookmarkHighlights = [],
  onRenderSuccess,
  onAddBookmark,
  showChatSelectionActions = false,
  onExplainSelection,
  onAskSelection,
  onPageChange,
}: PdfViewerProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(600);
  const [currentPage, setCurrentPage] = useState(page);
  const prevNavigationTargetRef = useRef<{ page: number; bbox: BoundingBox | null } | null>(null);
  // The page this viewer actually lands on for the initial open (props.page, or
  // the remembered scroll position when one is restored). PreviewPane's loading
  // overlay is cleared when *this* page paints -- gating on props.page instead
  // would hang forever whenever the two diverge (e.g. a restored position deep
  // in the document, whose page never enters the render window).
  const landingPageRef = useRef<number | null>(null);
  const initialRenderSignaledRef = useRef(false);
  // True while a remembered position is being restored. Restoring scrolls the
  // container programmatically, which fires `onScroll`; without this guard those
  // events would save an intermediate (page-top) position back over the anchor
  // we are mid-way through restoring, corrupting it a little more each reopen.
  const isRestoringRef = useRef(false);
  const restoreSettleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Restore the reader's last zoom for this document synchronously, so
  // renderedWidth is already correct when the scroll position is restored and
  // auto-zoom (skipped below when a zoom is remembered) never shifts it.
  const [zoom, setZoom] = useState(() => readPdfScrollPosition(url)?.zoom ?? 1.0);
  // The parsed document comes from a shared LRU cache (kept alive across
  // unmounts), so navigating back to a recently opened file is instant.
  const pdf = usePdfDocument(url);
  const numPages = pdf?.numPages ?? null;
  const [isOutlineOpen, setIsOutlineOpen] = useState(false);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));
  const [selectionBookmark, setSelectionBookmark] = useState<{
    page: number;
    bbox: BoundingBox;
    rects: BoundingBox[];
    quote: string;
    buttonLeft: number;
    buttonTop: number;
  } | null>(null);
  const [askDraft, setAskDraft] = useState("");
  const [isAskOpen, setIsAskOpen] = useState(false);
  const askInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  // Dismiss the "+ Bookmark" action as soon as the selection collapses (the
  // user clicked elsewhere, pressed a key, etc.) rather than waiting for the
  // next mouseup inside the viewer.
  useEffect(() => {
    const onSelectionChange = () => {
      // While the ask form is open, focusing its input collapses the PDF text
      // selection, which would otherwise tear down the popup mid-typing. The
      // form captured what it needs in selectionBookmark and owns its own
      // dismissal (Escape/Cancel/submit), so ignore selection loss here.
      if (isAskOpen) return;
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) {
        setSelectionBookmark(null);
        setIsAskOpen(false);
        setAskDraft("");
      }
    };
    window.document.addEventListener("selectionchange", onSelectionChange);
    return () => window.document.removeEventListener("selectionchange", onSelectionChange);
  }, [isAskOpen]);

  useEffect(() => {
    if (isAskOpen) askInputRef.current?.focus();
  }, [isAskOpen]);

  const renderedWidth = containerWidth * zoom;
  const { pageMetrics, hasPageMetrics } = usePdfPageMetrics(pdf, url);
  const outline = usePdfOutline(pdf);

  // Preserve the horizontal focal point across a zoom change. Without this the
  // scroll container keeps scrollLeft = 0, so a zoomed-in page stays pinned to
  // the left edge and its centre drifts off-screen to the right. We capture the
  // point under the viewport's horizontal centre as a fraction of the current
  // content width, then re-apply it once the page has grown to its new width.
  const pendingZoomAnchorRef = useRef<number | null>(null);

  // URL of the document we have already auto-zoomed, so the measurement runs
  // exactly once per document (and never fights a subsequent manual zoom). A
  // remembered zoom means this document was already sized in an earlier mount;
  // pre-mark it so auto-zoom is skipped on reopen and the restored zoom stands.
  const autoZoomedUrlRef = useRef<string | null>(
    readPdfScrollPosition(url)?.zoom !== undefined ? url : null,
  );

  const setZoomKeepingHorizontalCenter = useCallback(
    (nextZoom: (zoom: number) => number) => {
      setZoom((zoom) => {
        const next = nextZoom(zoom);
        // No-op at the min/max limits: leave no pending anchor, otherwise it
        // would be applied later on an unrelated resize.
        if (next === zoom) return zoom;
        const container = containerRef.current;
        if (container && renderedWidth > 0) {
          const centerX = container.scrollLeft + container.clientWidth / 2;
          pendingZoomAnchorRef.current = centerX / renderedWidth;
        }
        return next;
      });
    },
    [renderedWidth],
  );

  useLayoutEffect(() => {
    const relativeCenter = pendingZoomAnchorRef.current;
    const container = containerRef.current;
    if (relativeCenter === null || !container) return;
    pendingZoomAnchorRef.current = null;
    // Synchronous: renderedWidth (and the page div's width) already updated in
    // this commit, so scrollWidth is grown before paint — no left-edge flash.
    container.scrollLeft = relativeCenter * renderedWidth - container.clientWidth / 2;
  }, [renderedWidth]);

  // Auto-zoom a freshly opened document so its body text renders at a
  // comfortable on-screen size. We sample a few pages, take the
  // character-weighted median font size (which locks onto body text and ignores
  // headings/footnotes), and combine it with the page width to predict the
  // pixel height of body text against a fixed reference viewport. The required
  // zoom is then TARGET / that height, floored at 1.0 (never shrink) and capped.
  // Runs once per document; a scanned/textless PDF yields no samples and is
  // left at 1.0.
  useEffect(() => {
    if (!pdf) return;
    if (autoZoomedUrlRef.current === url) return;

    let cancelled = false;
    (async () => {
      const fontSizes: number[] = [];
      const pageWidths: number[] = [];
      const total = pdf.numPages;
      // Skip the title page when the document is long enough to have one.
      const start = total > AUTO_ZOOM_SAMPLE_PAGES + 1 ? 2 : 1;
      const end = Math.min(start + AUTO_ZOOM_SAMPLE_PAGES - 1, total);
      for (let p = start; p <= end; p++) {
        const pdfPage = await pdf.getPage(p);
        if (cancelled) return;
        pageWidths.push(pdfPage.view[2] - pdfPage.view[0]);
        const content = await pdfPage.getTextContent();
        if (cancelled) return;
        for (const item of content.items) {
          if (!("str" in item)) continue;
          const length = item.str.trim().length;
          if (length === 0) continue;
          // Font size in PDF units = vertical scale of the text transform.
          const size = Math.hypot(item.transform[2], item.transform[3]);
          for (let i = 0; i < length; i++) fontSizes.push(size);
        }
      }
      if (cancelled) return;
      // Mark done only after a full measurement completes. Setting this up front
      // would break under React StrictMode, whose mount/unmount/remount cancels
      // the first pass — the remount would then see the flag and skip measuring.
      autoZoomedUrlRef.current = url;
      if (fontSizes.length === 0 || pageWidths.length === 0) return;

      const medianPageWidth = median(pageWidths);
      if (medianPageWidth <= 0) return;
      // Body-text height in CSS px when the reference viewport is fit to the page.
      const renderedPx =
        median(fontSizes) * (AUTO_ZOOM_REFERENCE_WIDTH_PX / medianPageWidth);
      if (renderedPx <= 0) return;

      const rawZoom = AUTO_ZOOM_TARGET_PX / renderedPx;
      // Below the deadband the text is already comfortable; leave zoom at 1.0
      // untouched rather than nudging it and flickering the page.
      if (rawZoom < AUTO_ZOOM_MIN_INCREASE) return;
      const autoZoom = Math.min(AUTO_ZOOM_MAX, rawZoom);
      // Reuse the recentre mechanism so the enlarged page opens horizontally
      // centred (equal margins trimmed) instead of pinned to the left edge.
      pendingZoomAnchorRef.current = 0.5;
      setZoom(autoZoom);
    })().catch((e) => console.error("PDF auto-zoom measurement failed:", e));

    return () => {
      cancelled = true;
    };
  }, [pdf, url]);

  const getVirtualPageSize = useCallback(
    (index: number) => {
      const metric = pageMetrics[index];
      if (!metric) return 900 + PAGE_GAP_PX;
      return getScaledPageHeight(metric, renderedWidth) + PAGE_GAP_PX;
    },
    [pageMetrics, renderedWidth],
  );

  const virtualizer = useVirtualizer({
    count: hasPageMetrics ? pageMetrics.length : 0,
    getScrollElement: () => containerRef.current,
    estimateSize: getVirtualPageSize,
    overscan: 2,
  });
  const virtualItems = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();
  const paddingTop = virtualItems[0]?.start ?? 0;
  const paddingBottom = (() => {
    const lastItem = virtualItems[virtualItems.length - 1];
    if (!lastItem) return 0;
    return Math.max(totalSize - lastItem.start - getVirtualPageSize(lastItem.index), 0);
  })();

  const scrollToPage = useCallback(
    (p: number) => virtualizer.scrollToIndex(p - 1, { align: "start" }),
    [virtualizer],
  );

  // Restore a remembered position as the exact inverse of captureScrollPosition.
  // scrollToIndex only brings the target page into the render window (and near
  // the top); the precise landing is then computed from live DOM geometry using
  // the *same* basis capture used -- the page element's own height, gap included
  // -- so `capture(restore(pos)) === pos`. Reading the element's real position
  // also absorbs any residual from scrollToIndex's estimate-based alignment,
  // replacing the old relative `+=` nudge that landed on an uncertain base.
  const restoreScrollPosition = useCallback(
    (pos: PdfScrollPosition) => {
      const pageIndex = Math.min(Math.max(pos.page - 1, 0), pageMetrics.length - 1);
      isRestoringRef.current = true;
      if (restoreSettleTimerRef.current) clearTimeout(restoreSettleTimerRef.current);

      virtualizer.scrollToIndex(pageIndex, { align: "start" });
      setCurrentPage(pageIndex + 1);

      requestAnimationFrame(() => {
        const container = containerRef.current;
        const pageElement = container?.querySelector<HTMLElement>(
          `[data-page-number="${pageIndex + 1}"]`,
        );
        if (container && pageElement) {
          const viewportTop = container.getBoundingClientRect().top;
          const rect = pageElement.getBoundingClientRect();
          // Bring the page's top to the viewport top, then descend by the stored
          // fraction of the same height capture divided by.
          container.scrollTop += rect.top - viewportTop + pos.offsetRatio * rect.height;
        }
        // Re-enable saving only after this restore's scroll events have drained
        // (each onScroll saves a frame later), so an intermediate state can't be
        // written back over the anchor. The anchor already sits in memory
        // unchanged, so nothing is lost by not saving during the restore.
        restoreSettleTimerRef.current = setTimeout(() => {
          isRestoringRef.current = false;
        }, 250);
      });
    },
    [virtualizer, pageMetrics],
  );

  // Follow an in-document GoTo link (table-of-contents entry, cross-reference):
  // resolve its destination to a page and scroll there, nudging to the exact
  // vertical anchor when the destination pins one.
  const navigateToDestination = useCallback(
    (dest: PdfDestination) => {
      if (!pdf) return;
      resolveDestination(pdf, dest)
        .then((resolved) => {
          if (!resolved) return;
          const { pageIndex, offsetY } = resolved;
          virtualizer.scrollToIndex(pageIndex, { align: "start" });
          setCurrentPage(pageIndex + 1);
          if (offsetY !== null) {
            const metric = pageMetrics[pageIndex];
            const container = containerRef.current;
            if (metric && container) {
              const pageScale = renderedWidth / metric.width;
              // scrollToIndex sets scrollTop synchronously; apply the in-page
              // offset on the next frame so it lands after the page is measured.
              requestAnimationFrame(() => {
                container.scrollTop += offsetY * pageScale;
              });
            }
          }
        })
        .catch((e) => console.error("PDF link navigation failed:", e));
    },
    [pdf, virtualizer, pageMetrics, renderedWidth],
  );

  const openExternalLink = useCallback((url: string) => {
    api.openPath(url).catch((e) => console.error("Open PDF link failed:", e));
  }, []);

  const syncCurrentPageFromScroll = useCallback(() => {
    const container = containerRef.current;
    if (!container) return;

    const containerRect = container.getBoundingClientRect();
    const viewportCenter = containerRect.top + containerRect.height / 2;
    const pageElements = Array.from(container.querySelectorAll<HTMLElement>("[data-page-number]"));

    if (pageElements.length === 0) return;

    let closestPage: number | null = null;
    let closestDistance = Number.POSITIVE_INFINITY;

    for (const pageElement of pageElements) {
      const pageRect = pageElement.getBoundingClientRect();
      const pageCenter = pageRect.top + pageRect.height / 2;
      const distance = Math.abs(pageCenter - viewportCenter);

      if (distance < closestDistance) {
        closestDistance = distance;
        closestPage = Number(pageElement.dataset.pageNumber);
      }
    }

    if (closestPage !== null) {
      setCurrentPage(closestPage);
    }
  }, []);

  const handleMouseUp = useCallback(() => {
    const root = rootRef.current;
    const container = containerRef.current;
    const selection = window.getSelection();
    if (!root || !container || !selection || selection.isCollapsed || selection.rangeCount === 0) {
      setSelectionBookmark(null);
      return;
    }

    const range = selection.getRangeAt(0);
    const startNode = range.startContainer;
    const startElement =
      startNode instanceof Element ? startNode : startNode.parentElement ?? null;
    const pageElement = startElement?.closest<HTMLElement>("[data-page-number]");
    if (!pageElement || !container.contains(pageElement)) {
      setSelectionBookmark(null);
      return;
    }

    const pageNumber = Number(pageElement.dataset.pageNumber);
    const pageMetric = pageMetrics[pageNumber - 1];
    if (!pageNumber || !pageMetric) {
      setSelectionBookmark(null);
      return;
    }

    const selectionRect = range.getBoundingClientRect();
    const pageRect = pageElement.getBoundingClientRect();
    const rootRect = root.getBoundingClientRect();
    const pageScale = renderedWidth / pageMetric.width;
    const quote = selection.toString().trim();
    if (!quote || selectionRect.width <= 0 || selectionRect.height <= 0) {
      setSelectionBookmark(null);
      return;
    }

    // Highlight the exact selected text by capturing one rectangle per line
    // (getClientRects) instead of the selection's bounding box, which on a
    // multi-line selection would also cover the unselected head/tail of the
    // first and last lines. Keep only fragments centred on the start page so a
    // selection dragged across page boundaries doesn't pull in other pages.
    const rects = mergeRectsByLine(
      Array.from(range.getClientRects())
        .filter((rect) => {
          if (rect.width <= 0 || rect.height <= 0) return false;
          const centerY = rect.top + rect.height / 2;
          return centerY >= pageRect.top && centerY <= pageRect.bottom;
        })
        .map((rect) => ({
          x: (rect.left - pageRect.left) / pageScale,
          y: (rect.top - pageRect.top) / pageScale,
          width: rect.width / pageScale,
          height: rect.height / pageScale,
        })),
    );
    if (rects.length === 0) {
      setSelectionBookmark(null);
      return;
    }

    setSelectionBookmark({
      page: pageNumber,
      bbox: unionBox(rects),
      rects,
      quote,
      buttonLeft: Math.min(
        Math.max(selectionRect.left - rootRect.left, 8),
        Math.max(rootRect.width - 128, 8),
      ),
      buttonTop: Math.min(
        Math.max(selectionRect.bottom - rootRect.top + 3, 8),
        Math.max(rootRect.height - 40, 8),
      ),
    });
    setIsAskOpen(false);
    setAskDraft("");
  }, [pageMetrics, renderedWidth]);

  const {
    searchInputRef,
    isSearchOpen,
    setIsSearchOpen,
    innerQuery,
    setInnerQuery,
    innerMatches,
    currentMatchIdx,
    isSearching,
    handleNextMatch,
    handlePrevMatch,
    handleSearchInputKeyDown,
  } = usePdfInnerSearch(pdf, scrollToPage);

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    // Adopt the real width synchronously, before paint: the page heights that
    // the position restore reads are derived from it, and starting at the 600px
    // placeholder would let the async ResizeObserver correct it only *after*
    // restore had landed, reflowing the document and shifting the restored
    // position off by a constant amount.
    const initialWidth = el.clientWidth;
    if (initialWidth > 0) setContainerWidth(initialWidth);
    const ro = new ResizeObserver((entries) => {
      const w = entries[0].contentRect.width;
      if (w > 0) {
        setContainerWidth(w);
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Clear PreviewPane's "loading document" overlay exactly once, when the page
  // we actually landed on has painted.
  const signalInitialRender = useCallback(() => {
    if (initialRenderSignaledRef.current) return;
    initialRenderSignaledRef.current = true;
    onRenderSuccess?.();
  }, [onRenderSuccess]);

  useEffect(() => {
    const prevTarget = prevNavigationTargetRef.current;
    const navigationChanged =
      !prevTarget ||
      prevTarget.page !== page ||
      prevTarget.bbox !== highlight_bbox;

    if (hasPageMetrics && !isSearchOpen && navigationChanged) {
      // On the first navigation for this document, a plain open (page 1, no
      // highlight target) carries no explicit destination, so restore where the
      // reader was last left. An explicit target (a search hit or bookmark)
      // always wins over the remembered position.
      const isInitial = prevTarget === null;
      const isDefaultTarget = page === 1 && highlight_bbox === null;
      const remembered = isInitial && isDefaultTarget ? readPdfScrollPosition(url) : null;
      if (remembered) {
        restoreScrollPosition(remembered);
      } else {
        virtualizer.scrollToIndex(page - 1, { align: "start" });
        setCurrentPage(page);
      }

      if (isInitial) {
        // Record the page we actually land on so the loading overlay is cleared
        // by *its* paint, not props.page's. When restoring, this is the
        // remembered page (clamped the same way restoreScrollPosition clamps).
        const landing = remembered
          ? Math.min(Math.max(remembered.page, 1), pageMetrics.length)
          : page;
        landingPageRef.current = landing;
        // A top-of-document open can paint the landing page before this effect
        // runs; that page's onRenderSuccess has already fired and won't fire
        // again, so signal now instead of waiting for an event that never comes.
        if (containerRef.current?.querySelector(`[data-page-number="${landing}"] canvas`)) {
          signalInitialRender();
        }
      }
    }

    if (hasPageMetrics) {
      prevNavigationTargetRef.current = { page, bbox: highlight_bbox };
    }
  }, [page, hasPageMetrics, highlight_bbox, isSearchOpen, virtualizer, url, restoreScrollPosition, pageMetrics, signalInitialRender]);

  // Remember where the reader is as it scrolls, so reopening this document later
  // in the same session lands back here. Captured live (not on unmount): by the
  // time an unmounting component's effect cleanup runs, React has already
  // detached the ref and removed the DOM, leaving nothing to measure.
  const rememberScrollPosition = useCallback(() => {
    // Ignore the container's own restore-driven scrolls; only genuine user
    // scrolling should update the remembered position.
    if (isRestoringRef.current) return;
    const container = containerRef.current;
    if (!container) return;
    const pos = captureScrollPosition(container);
    if (pos) savePdfScrollPosition(url, { ...pos, zoom });
  }, [url, zoom]);

  useEffect(
    () => () => {
      if (restoreSettleTimerRef.current) clearTimeout(restoreSettleTimerRef.current);
    },
    [],
  );

  useEffect(() => {
    if (numPages) {
      setCurrentPage((prev) => Math.min(Math.max(prev, 1), numPages));
    }
  }, [numPages]);

  useEffect(() => {
    if (hasPageMetrics) {
      requestAnimationFrame(syncCurrentPageFromScroll);
    }
  }, [hasPageMetrics, zoom, syncCurrentPageFromScroll]);

  // Debounced so a fast scroll-past doesn't report every page it flies
  // through -- only where the reader actually settles.
  useEffect(() => {
    if (!onPageChange) return;
    const id = setTimeout(() => onPageChange(currentPage), 400);
    return () => clearTimeout(id);
  }, [currentPage, onPageChange]);

  return (
    <div ref={rootRef} className="h-full min-h-0 relative flex flex-col overflow-hidden">
      <div className="absolute bottom-4 right-4 z-20 flex flex-col gap-2 items-end">
        {isSearchOpen && (
          <div className="bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-xl flex items-center p-1 gap-1 animate-in fade-in slide-in-from-bottom-2 duration-200">
            <div className="relative flex items-center pl-2 text-[var(--text-dim)]">
              <SearchIcon size={12} />
              <input
                ref={searchInputRef}
                type="text"
                placeholder="Find in document..."
                value={innerQuery}
                onChange={(e) => setInnerQuery(e.target.value)}
                onKeyDown={handleSearchInputKeyDown}
                className="bg-transparent border-none outline-none px-2 py-1 text-xs text-[var(--text-main)] placeholder-[var(--text-dim)] w-48"
              />
            </div>
            {innerMatches.length > 0 && (
              <span className="text-[10px] text-[var(--text-muted)] font-mono px-1">
                {currentMatchIdx + 1}/{innerMatches.length}
              </span>
            )}
            {isSearching && (
              <div className="w-3 h-3 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin mx-1" />
            )}
            <div className="flex border-l border-[var(--border-main)] ml-1 pl-1">
              <button
                onClick={handlePrevMatch}
                disabled={innerMatches.length === 0}
                className="p-1 hover:bg-[var(--bg-active)] rounded disabled:opacity-30"
              >
                <ChevronUp size={14} />
              </button>
              <button
                onClick={handleNextMatch}
                disabled={innerMatches.length === 0}
                className="p-1 hover:bg-[var(--bg-active)] rounded disabled:opacity-30"
              >
                <ChevronDown size={14} />
              </button>
              <button
                onClick={() => setIsSearchOpen(false)}
                className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] hover:text-[var(--accent-red)]"
              >
                <X size={14} />
              </button>
            </div>
          </div>
        )}

        <div className="flex items-center gap-1 bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-lg px-2 py-1 text-xs text-[var(--text-main)]">
          {pdf && (
            <Tooltip content={outline ? "Table of contents" : "This document has no table of contents"}>
              <button
                onClick={() => setIsOutlineOpen((open) => !open)}
                disabled={!outline}
                className={`p-1 transition-colors mr-1 border-r border-[var(--border-main)] pr-2 ${
                  outline ? "hover:text-[var(--accent-blue)]" : "opacity-40 cursor-default"
                } ${isOutlineOpen ? "text-[var(--accent-blue)]" : ""}`}
              >
                <List size={12} />
              </button>
            </Tooltip>
          )}
          {!isSearchOpen && (
            <Tooltip content="Find in document (Cmd+F)">
              <button
                onClick={() => {
                  setIsSearchOpen(true);
                  setTimeout(() => searchInputRef.current?.focus(), 50);
                }}
                className="p-1 hover:text-[var(--accent-blue)] transition-colors mr-1 border-r border-[var(--border-main)] pr-2"
              >
                <SearchIcon size={12} />
              </button>
            </Tooltip>
          )}
          {numPages && <span className="w-16 text-center font-mono">{currentPage}/{numPages}</span>}
          {numPages && <span className="text-[var(--text-dim)]">|</span>}
          <button
            onClick={() =>
              setZoomKeepingHorizontalCenter((z) => Math.max(0.25, +(z - ZOOM_STEP).toFixed(2)))
            }
            className="px-1 hover:text-[var(--accent-blue)]"
          >
            −
          </button>
          <span className="w-10 text-center font-mono">{Math.round(zoom * 100)}%</span>
          <button
            onClick={() =>
              setZoomKeepingHorizontalCenter((z) => Math.min(3.0, +(z + ZOOM_STEP).toFixed(2)))
            }
            className="px-1 hover:text-[var(--accent-blue)]"
          >
            +
          </button>
        </div>
      </div>

      <div className="flex-1 flex min-h-0">
        {isOutlineOpen && outline && (
          <PdfOutline
            outline={outline}
            onNavigateToDestination={navigateToDestination}
            onOpenExternal={openExternalLink}
            onClose={() => setIsOutlineOpen(false)}
          />
        )}
        <div
          ref={containerRef}
          className={`flex-1 min-w-0 overflow-auto bg-[var(--bg-sidebar)] pr-1 ${isDark ? "pdf-dark-mode" : ""}`}
          onMouseUp={handleMouseUp}
          onScroll={() => {
            requestAnimationFrame(() => {
              syncCurrentPageFromScroll();
              rememberScrollPosition();
            });
          }}
          style={{
            WebkitUserSelect: "text",
            userSelect: "text",
            transition: "filter 0.3s ease",
          }}
        >
          {/* Explicit width (not fit-content) so the scrollable extent grows in
              the same commit as a zoom change, instead of trailing react-pdf's
              async canvas render. This lets the zoom-recentre effect set
              scrollLeft synchronously without the browser clamping it to a
              stale, not-yet-widened maximum. */}
          <div style={{ paddingTop, paddingBottom, width: `${renderedWidth}px` }}>
            {virtualItems.map((vItem) => {
              const pageNum = vItem.index + 1;
              const pageMetric = pageMetrics[vItem.index];
              if (!pageMetric) return null;
              const pageScale = renderedWidth / pageMetric.width;
              const pageHeight = getScaledPageHeight(pageMetric, renderedWidth);

              const isTargetPage = pageNum === page;
              const targetBbox = isTargetPage ? highlight_bbox : null;
              // Precise emphasis for a bookmark target; when present it replaces
              // the coarse single-box emphasis below.
              const targetRects =
                isTargetPage && !isSearchOpen ? highlight_rects : null;

              const innerMatch = innerMatches[currentMatchIdx];
              const innerBbox = innerMatch && innerMatch.page === pageNum ? innerMatch.bbox : null;

              const activeBbox = isSearchOpen
                ? innerBbox
                : targetRects && targetRects.length > 0
                  ? null
                  : targetBbox;
              const pageBookmarkHighlights = bookmarkHighlights.filter(
                (highlight) => highlight.page === pageNum,
              );

              let overlayStyle: React.CSSProperties | undefined;
              if (activeBbox) {
                const { x, y, width, height } = activeBbox;
                overlayStyle = {
                  position: "absolute",
                  left: `${x * pageScale}px`,
                  top: `${y * pageScale}px`,
                  width: `${Math.max(width * pageScale, 4)}px`,
                  height: `${Math.max(height * pageScale, 4)}px`,
                  backgroundColor: isSearchOpen
                    ? "rgba(59, 130, 246, 0.25)"
                    : "rgba(250, 204, 21, 0.25)",
                  border: isSearchOpen
                    ? "1px solid rgba(59, 130, 246, 0.8)"
                    : "1px solid rgba(250, 204, 21, 0.8)",
                  borderRadius: "2px",
                  pointerEvents: "none",
                };
              }

              return (
                <div
                  key={vItem.key}
                  data-page-number={pageNum}
                  style={{ width: "100%", height: pageHeight + PAGE_GAP_PX }}
                >
                  <div style={{ position: "relative", display: "inline-block", height: pageHeight }}>
                    <Page
                      pdf={pdf ?? false}
                      pageNumber={pageNum}
                      width={renderedWidth}
                      renderAnnotationLayer={false}
                      renderTextLayer={false}
                      canvasBackground="white"
                      onRenderSuccess={() => {
                        const landing = landingPageRef.current ?? (page || 1);
                        if (pageNum === landing) signalInitialRender();
                      }}
                    />
                    {pdf && (
                      <PdfTextLayer pdf={pdf} pageNumber={pageNum} scale={pageScale} />
                    )}
                    {pdf && (
                      <PdfLinkLayer
                        pdf={pdf}
                        pageNumber={pageNum}
                        scale={pageScale}
                        onNavigateToDestination={navigateToDestination}
                        onOpenExternal={openExternalLink}
                      />
                    )}
                    {pageBookmarkHighlights.flatMap((highlight) =>
                      highlight.rects.map((rect, rectIndex) => {
                        const { x, y, width, height } = rect;
                        return (
                          <div
                            key={`${highlight.id}-${rectIndex}`}
                            data-testid="bookmark-highlight"
                            data-bookmark-id={highlight.id}
                            style={{
                              position: "absolute",
                              left: `${x * pageScale}px`,
                              top: `${y * pageScale}px`,
                              width: `${Math.max(width * pageScale, 4)}px`,
                              height: `${Math.max(height * pageScale, 4)}px`,
                              backgroundColor: "rgba(250, 204, 21, 0.16)",
                              borderBottom: "2px solid rgba(202, 138, 4, 0.75)",
                              borderRadius: "2px",
                              pointerEvents: "none",
                            }}
                          />
                        );
                      }),
                    )}
                    {overlayStyle && <div style={overlayStyle} />}
                    {targetRects?.map((rect, rectIndex) => {
                      const { x, y, width, height } = rect;
                      return (
                        <div
                          key={`target-${rectIndex}`}
                          data-testid="target-highlight"
                          style={{
                            position: "absolute",
                            left: `${x * pageScale}px`,
                            top: `${y * pageScale}px`,
                            width: `${Math.max(width * pageScale, 4)}px`,
                            height: `${Math.max(height * pageScale, 4)}px`,
                            backgroundColor: "rgba(250, 204, 21, 0.25)",
                            border: "1px solid rgba(250, 204, 21, 0.8)",
                            borderRadius: "2px",
                            pointerEvents: "none",
                          }}
                        />
                      );
                    })}
                    {!isSearchOpen &&
                      targetBbox &&
                      isTargetPage &&
                      (() => {
                        const { x, y, width, height } = targetBbox;
                        const cx = (x + width / 2) * pageScale;
                        const cy = (y + height / 2) * pageScale;
                        const r = Math.max(width, height) * pageScale;
                        return (
                          <div
                            key={`${x}-${y}-${width}-${height}`}
                            className="animate-ping pointer-events-none"
                            style={{
                              position: "absolute",
                              left: cx - r / 2,
                              top: cy - r / 2,
                              width: r,
                              height: r,
                              borderRadius: "50%",
                              backgroundColor: "rgba(202, 138, 4, 0.45)",
                              animationIterationCount: 2,
                              animationFillMode: "forwards",
                            }}
                          />
                        );
                      })()}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
      {selectionBookmark &&
        (onAddBookmark ||
          (showChatSelectionActions && (onExplainSelection || onAskSelection))) && (
          <div
            onMouseDown={(event) => event.preventDefault()}
            className="absolute z-40 rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-xs text-[var(--text-main)] shadow-lg"
            style={{ left: selectionBookmark.buttonLeft, top: selectionBookmark.buttonTop }}
          >
            {isAskOpen ? (
              <form
                className="flex items-center gap-1 p-1"
                onSubmit={(event) => {
                  event.preventDefault();
                  const question = askDraft.trim();
                  if (!question || !onAskSelection) return;
                  onAskSelection(selectionBookmark, question);
                  setSelectionBookmark(null);
                  setIsAskOpen(false);
                  setAskDraft("");
                  window.getSelection()?.removeAllRanges();
                }}
              >
                <input
                  ref={askInputRef}
                  value={askDraft}
                  onChange={(event) => setAskDraft(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") {
                      event.preventDefault();
                      setIsAskOpen(false);
                      setAskDraft("");
                    }
                  }}
                  placeholder="Ask about this…"
                  className="w-48 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-1.5 py-0.5 text-xs outline-none focus:border-[var(--accent-blue)]"
                />
                <button
                  type="submit"
                  disabled={!askDraft.trim()}
                  className="px-1.5 py-0.5 rounded bg-[var(--accent-blue)] text-white disabled:opacity-40"
                >
                  Send
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setIsAskOpen(false);
                    setAskDraft("");
                  }}
                  className="px-1.5 py-0.5 rounded hover:bg-[var(--bg-active)]"
                >
                  Cancel
                </button>
              </form>
            ) : (
              <div className="flex items-center">
                {onAddBookmark && (
                  <button
                    type="button"
                    onClick={() => {
                      onAddBookmark(selectionBookmark);
                      setSelectionBookmark(null);
                      window.getSelection()?.removeAllRanges();
                    }}
                    className="px-2 py-1 hover:bg-[var(--bg-active)]"
                  >
                    Bookmark
                  </button>
                )}
                {showChatSelectionActions && onExplainSelection && (
                  <button
                    type="button"
                    onClick={() => {
                      onExplainSelection(selectionBookmark);
                      setSelectionBookmark(null);
                      window.getSelection()?.removeAllRanges();
                    }}
                    className="px-2 py-1 border-l border-[var(--border-main)] hover:bg-[var(--bg-active)]"
                  >
                    Explain
                  </button>
                )}
                {showChatSelectionActions && onAskSelection && (
                  <button
                    type="button"
                    onClick={() => setIsAskOpen(true)}
                    className="px-2 py-1 border-l border-[var(--border-main)] hover:bg-[var(--bg-active)]"
                  >
                    Ask about this
                  </button>
                )}
              </div>
            )}
          </div>
      )}
    </div>
  );
}
