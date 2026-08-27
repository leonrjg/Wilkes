import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Ref,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Search as SearchIcon, List } from "react-feather";
import { GlobalWorkerOptions } from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { installReadableStreamAsyncIterator } from "./readableStreamAsyncIterator";
import PdfPageCanvas from "./PdfPageCanvas";
import type { BoundingBox } from "./documentCoordinates";
import { usePdfInnerSearch, type InnerMatch } from "./usePdfInnerSearch";
import { usePdfSearchResult } from "./usePdfSearchResult";
import type { PdfSearchLocator } from "./pdfTextLocator";
import { useDocumentFind } from "./useDocumentFind";
import FindBar from "./FindBar";
import ZoomControls, { ZOOM_STEP } from "./ZoomControls";
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
import { Tooltip } from "./Tooltip";
import type { DocumentSelection } from "./selection";
import SelectionLayer from "./SelectionLayer";
import { useDomDocumentSelection } from "./useDomDocumentSelection";
import { useReaderHost } from "./ReaderHost";
import {
  elementAnchor,
  rectDecorationsForPage,
  unionBox,
  type Decoration,
} from "./decorations";
import type { PdfReaderSlots } from "./slots";
import type { FindableReaderHandle, ZoomableReaderHandle } from "./readerHandle";

GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

export interface PdfViewerProps {
  url: string;
  /** Incremented to retry a failed parse without changing the document URL. */
  loadAttempt?: number;
  page: number;
  /** Where to navigate: the scroll destination and what the ping points at.
   *  It locates the target, it does not draw it -- a host that wants a mark
   *  left on the page supplies a decoration, so nothing is drawn twice. */
  highlight_bbox: BoundingBox | null;
  /** Raw search evidence used to correct a chunk-level indexed origin against
   *  nearby PDF.js pages. It is transient viewer state, never index data. */
  search_locator?: PdfSearchLocator | null;
  /** Host-owned marks on the document. Only `rects`-anchored decorations are
   *  placeable here; `range` anchors belong to the text readers and are
   *  ignored, so one list can be handed to whichever reader is mounted. */
  decorations?: Decoration[];
  slots?: PdfReaderSlots;
  /** Imperative control — navigation, zoom and find as commands. */
  ref?: Ref<PdfReaderHandle>;
  onRenderSuccess?: () => void;
  onLoadError?: (error: unknown) => void;
  /** Fires (debounced) whenever the page nearest the viewport center changes
   *  -- covers scroll, page-jump, and link/outline navigation alike, since
   *  all of them funnel through `currentPage`. Used to keep the chat pane's
   *  "open document" page badge live as the user reads, not just on the
   *  initial landing page. */
  onPageChange?: (page: number) => void;
}

export type PdfSelection = DocumentSelection;

export interface PdfReaderHandle extends FindableReaderHandle, ZoomableReaderHandle {
  /** Scroll a page to the top of the viewport, optionally revealing a
   *  rectangle on it once the page is mounted. */
  goToPage: (page: number, opts?: { reveal?: BoundingBox }) => void;
  /** Follow an in-document destination (outline entry, cross-reference). */
  goToDestination: (destination: PdfDestination) => void;
  getCurrentPage: () => number;
  getPageCount: () => number | null;
  /** The parsed document, for hosts that need to read it directly. Null until
   *  it has loaded. */
  getDocument: () => PDFDocumentProxy | null;
}

const PAGE_GAP_PX = 12;
const PDF_MIN_ZOOM = 0.25;
const PDF_MAX_ZOOM = 3.0;
// Keep active find matches away from the very edge of the reader. Besides
// making the result easier to spot, the top inset prevents a match from sitting
// underneath the floating find bar. The inset is reduced automatically when a
// viewport is too small to accommodate it around the whole match.
const FIND_MATCH_VIEWPORT_INSET_PX = 48;

// Auto-zoom: bring the dominant body text of a freshly opened document up to
// the user-configured CSS-pixel height. We only ever enlarge (floor 1.0, so
// already-comfortable documents are left untouched) and cap the enlargement so
// pathological cases stay sane.
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

/** Diameter of the navigation-target ripple, as a fraction of the target's
 *  shorter side. Sizing off the shorter side rather than the longer one keeps
 *  the ripple the same modest size whether the target is one word or a
 *  paragraph-wide union box — length of the match says nothing about how big
 *  the "look here" pulse should be. The `animate-ping` keyframes scale it to
 *  2x over its lifetime, so the value is the starting size. */
const PING_SIZE_RATIO = 1.2;

/** Place a highlight overlay on a rendered page. Geometry only — the palette,
 *  radius and padding belong to `.pdf-highlight` in styles.css, shared with
 *  every other viewer. */
function highlightRectStyle(rect: BoundingBox, pageScale: number): React.CSSProperties {
  const { x, y, width, height } = rect;
  return {
    left: `${x * pageScale}px`,
    top: `${y * pageScale}px`,
    width: `${Math.max(width * pageScale, 4)}px`,
    height: `${Math.max(height * pageScale, 4)}px`,
  };
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

/** Reveal a PDF-space rectangle inside the scroll container. Page
 * virtualization is handled by the caller; once the page is mounted, its live
 * DOM rectangle gives us a reliable coordinate bridge from PDF units to the
 * current viewport at any zoom. An axis that is already comfortably visible is
 * deliberately left alone, avoiding the disorienting lateral/vertical jitter
 * that a blanket `scrollIntoView({ block: "center" })` would introduce. */
function revealPdfMatch(
  container: HTMLDivElement,
  pageElement: HTMLElement,
  bbox: BoundingBox,
  pageScale: number,
): void {
  const containerRect = container.getBoundingClientRect();
  const pageRect = pageElement.getBoundingClientRect();
  const targetWidth = Math.max(bbox.width * pageScale, 4);
  const targetHeight = Math.max(bbox.height * pageScale, 4);
  const targetLeft = pageRect.left + bbox.x * pageScale;
  const targetTop = pageRect.top + bbox.y * pageScale;
  const targetRight = targetLeft + targetWidth;
  const targetBottom = targetTop + targetHeight;

  // A large match or a small pane may not leave room for the full configured
  // inset. Shrink it per axis so the visibility test remains achievable.
  const horizontalInset = Math.max(
    0,
    Math.min(FIND_MATCH_VIEWPORT_INSET_PX, (containerRect.width - targetWidth) / 2),
  );
  const verticalInset = Math.max(
    0,
    Math.min(FIND_MATCH_VIEWPORT_INSET_PX, (containerRect.height - targetHeight) / 2),
  );
  const horizontallyVisible =
    targetLeft >= containerRect.left + horizontalInset &&
    targetRight <= containerRect.right - horizontalInset;
  const verticallyVisible =
    targetTop >= containerRect.top + verticalInset &&
    targetBottom <= containerRect.bottom - verticalInset;

  if (!horizontallyVisible) {
    const targetCenter = targetLeft + targetWidth / 2;
    const viewportCenter = containerRect.left + containerRect.width / 2;
    container.scrollLeft += targetCenter - viewportCenter;
  }
  if (!verticallyVisible) {
    const targetCenter = targetTop + targetHeight / 2;
    const viewportCenter = containerRect.top + containerRect.height / 2;
    container.scrollTop += targetCenter - viewportCenter;
  }
}

export default function PdfViewer({
  url,
  loadAttempt = 0,
  page,
  highlight_bbox,
  search_locator = null,
  decorations = [],
  slots,
  ref,
  onRenderSuccess,
  onLoadError,
  onPageChange,
}: PdfViewerProps) {
  const { openExternal, colorScheme, pdfAutoZoomTargetPx: autoZoomTargetPx } = useReaderHost();
  const isDark = colorScheme === "dark";
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
  const pdf = usePdfDocument(url, loadAttempt, onLoadError);
  const numPages = pdf?.numPages ?? null;
  const locatedSearchResult = usePdfSearchResult(pdf, page, search_locator);
  const targetPage = locatedSearchResult?.page ?? page;
  const targetBbox =
    locatedSearchResult?.bbox ?? (search_locator ? null : highlight_bbox);
  // The reader draws emphasis only for geometry it worked out itself: a search
  // hit relocated against the real pages, which the host cannot know. Anything
  // the host already has coordinates for it draws as a decoration instead.
  const drawnTarget = locatedSearchResult;
  const [isOutlineOpen, setIsOutlineOpen] = useState(false);

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
    if (!pdf || autoZoomTargetPx === undefined) return;
    if (autoZoomedUrlRef.current === url) return;

    let cancelled = false;
    (async () => {
      const fontSizes: number[] = [];
      const pageWidths: number[] = [];
      const total = pdf.numPages;
      // Skip the title page when the document is long enough to have one.
      const start = total > AUTO_ZOOM_SAMPLE_PAGES + 1 ? 2 : 1;
      const end = Math.min(start + AUTO_ZOOM_SAMPLE_PAGES - 1, total);
      installReadableStreamAsyncIterator();
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

      const rawZoom = autoZoomTargetPx / renderedPx;
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
  }, [autoZoomTargetPx, pdf, url]);

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

  const openExternalLink = openExternal;

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

  const mapPdfSelection = useCallback((range: Range, selection: Selection): DocumentSelection | null => {
    const container = containerRef.current;
    if (!container) return null;
    const startNode = range.startContainer;
    const startElement =
      startNode instanceof Element ? startNode : startNode.parentElement ?? null;
    const pageElement = startElement?.closest<HTMLElement>("[data-page-number]");
    if (!pageElement || !container.contains(pageElement)) {
      return null;
    }

    const pageNumber = Number(pageElement.dataset.pageNumber);
    const pageMetric = pageMetrics[pageNumber - 1];
    if (!pageNumber || !pageMetric) {
      return null;
    }

    const selectionRect = range.getBoundingClientRect();
    const pageRect = pageElement.getBoundingClientRect();
    const pageScale = renderedWidth / pageMetric.width;
    const quote = selection.toString().trim();
    if (!quote || selectionRect.width <= 0 || selectionRect.height <= 0) {
      return null;
    }

    const selectionClientRects = Array.from(range.getClientRects());
    // Highlight the exact selected text by capturing one rectangle per line
    // (getClientRects) instead of the selection's bounding box, which on a
    // multi-line selection would also cover the unselected head/tail of the
    // first and last lines. Keep only fragments centred on the start page so a
    // selection dragged across page boundaries doesn't pull in other pages.
    const rects = mergeRectsByLine(
      selectionClientRects
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
      return null;
    }

    return {
      origin: { PdfPage: { page: pageNumber, bbox: unionBox(rects) } },
      rects,
      quote,
    };
  }, [pageMetrics, renderedWidth]);
  const domSelection = useDomDocumentSelection({
    rootRef,
    mapSelection: mapPdfSelection,
    dismissOnCollapsedSelection: true,
  });

  // A mirror of the matcher's output breaks the declaration cycle: the shared
  // controller needs the match count, while the matcher needs the controller's
  // query. The controller is declared first against this state, then synced.
  const [innerMatches, setInnerMatches] = useState<InnerMatch[]>([]);
  const find = useDocumentFind(innerMatches.length);
  const { matches, isSearching } = usePdfInnerSearch(pdf, find.query, find.isOpen);
  const isSearchOpen = find.isOpen;
  const currentMatchIdx = find.currentIdx;

  useEffect(() => setInnerMatches(matches), [matches]);

  // The controller owns which match is active. Navigation is deliberately
  // two-stage: first make the virtualizer mount the page, then use live page
  // geometry on the next frame to reveal the match itself. Page-only alignment
  // leaves matches lower down a page outside the viewport.
  useEffect(() => {
    const active = innerMatches[currentMatchIdx];
    if (!active) return;

    scrollToPage(active.page);
    const frame = requestAnimationFrame(() => {
      const container = containerRef.current;
      const pageMetric = pageMetrics[active.page - 1];
      const pageElement = container?.querySelector<HTMLElement>(
        `[data-page-number="${active.page}"]`,
      );
      if (!container || !pageMetric || !pageElement) return;

      revealPdfMatch(container, pageElement, active.bbox, renderedWidth / pageMetric.width);
    });

    // Stepping quickly can replace the active match before the virtualized page
    // is committed. Do not let the older deferred reveal win that race.
    return () => cancelAnimationFrame(frame);
  }, [currentMatchIdx, innerMatches, scrollToPage, pageMetrics, renderedWidth]);

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
      prevTarget.page !== targetPage ||
      prevTarget.bbox !== targetBbox;

    if (hasPageMetrics && !isSearchOpen && navigationChanged) {
      // On the first navigation for this document, a plain open (page 1, no
      // highlight target) carries no explicit destination, so restore where the
      // reader was last left. An explicit target (a search hit or bookmark)
      // always wins over the remembered position.
      const isInitial = prevTarget === null;
      const isDefaultTarget = targetPage === 1 && targetBbox === null;
      const remembered = isInitial && isDefaultTarget ? readPdfScrollPosition(url) : null;
      if (remembered) {
        restoreScrollPosition(remembered);
      } else {
        virtualizer.scrollToIndex(targetPage - 1, { align: "start" });
        setCurrentPage(targetPage);
      }

      if (isInitial) {
        // Record the page we actually land on so the loading overlay is cleared
        // by *its* paint, not props.page's. When restoring, this is the
        // remembered page (clamped the same way restoreScrollPosition clamps).
        const landing = remembered
          ? Math.min(Math.max(remembered.page, 1), pageMetrics.length)
          : targetPage;
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
      prevNavigationTargetRef.current = { page: targetPage, bbox: targetBbox };
    }
  }, [
    targetPage,
    hasPageMetrics,
    targetBbox,
    isSearchOpen,
    virtualizer,
    url,
    restoreScrollPosition,
    pageMetrics,
    signalInitialRender,
  ]);

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

  // Reveal a rectangle on a page: mount the page first, then use its live DOM
  // geometry on the next frame. Same two-stage dance the find navigation does,
  // for the same reason -- a virtualized page has no measurable box until it
  // has been committed.
  const revealOnPage = useCallback(
    (targetPageNumber: number, bbox: BoundingBox) => {
      scrollToPage(targetPageNumber);
      requestAnimationFrame(() => {
        const container = containerRef.current;
        const pageMetric = pageMetrics[targetPageNumber - 1];
        const pageElement = container?.querySelector<HTMLElement>(
          `[data-page-number="${targetPageNumber}"]`,
        );
        if (!container || !pageMetric || !pageElement) return;
        revealPdfMatch(container, pageElement, bbox, renderedWidth / pageMetric.width);
      });
    },
    [scrollToPage, pageMetrics, renderedWidth],
  );

  useImperativeHandle(
    ref,
    (): PdfReaderHandle => ({
      goToPage: (targetPageNumber, opts) => {
        if (opts?.reveal) {
          revealOnPage(targetPageNumber, opts.reveal);
        } else {
          scrollToPage(targetPageNumber);
        }
        setCurrentPage(targetPageNumber);
      },
      goToDestination: navigateToDestination,
      scrollToDecoration: (id) => {
        const decoration = decorations.find((candidate) => candidate.id === id);
        if (!decoration || decoration.anchor.kind !== "rects") return;
        const { page: decorationPage, rects } = decoration.anchor;
        if (rects.length === 0) {
          scrollToPage(decorationPage);
        } else {
          revealOnPage(decorationPage, unionBox(rects));
        }
        setCurrentPage(decorationPage);
      },
      getCurrentPage: () => currentPage,
      getPageCount: () => numPages,
      getDocument: () => pdf ?? null,
      getZoom: () => zoom,
      setZoom: (next) =>
        setZoomKeepingHorizontalCenter(() =>
          Math.min(PDF_MAX_ZOOM, Math.max(PDF_MIN_ZOOM, +next.toFixed(2))),
        ),
      openFind: (query) => {
        if (query !== undefined) find.setQuery(query);
        find.open();
      },
      closeFind: find.close,
    }),
    [
      ref,
      revealOnPage,
      scrollToPage,
      navigateToDestination,
      decorations,
      currentPage,
      numPages,
      pdf,
      zoom,
      setZoomKeepingHorizontalCenter,
      find,
    ],
  );

  return (
    <div ref={rootRef} className="h-full min-h-0 relative flex flex-col overflow-hidden">
      {isSearchOpen && (
        <div className="absolute top-4 right-4 z-20">
          <FindBar find={find} matchCount={innerMatches.length} isSearching={isSearching} />
        </div>
      )}

      <div className="absolute bottom-4 right-4 z-20 flex flex-col gap-2 items-end">
        <div className="flex items-center gap-1.5 bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-lg px-2.5 py-1.5 text-sm text-[var(--text-main)]">
          {pdf && (
            <Tooltip content={outline ? "Table of contents" : "This document has no table of contents"}>
              <button
                onClick={() => setIsOutlineOpen((open) => !open)}
                disabled={!outline}
                className={`p-1.5 transition-colors mr-1 border-r border-[var(--border-main)] pr-2.5 ${
                  outline ? "hover:text-[var(--accent-blue)]" : "opacity-40 cursor-default"
                } ${isOutlineOpen ? "text-[var(--accent-blue)]" : ""}`}
              >
                <List size={14} />
              </button>
            </Tooltip>
          )}
          {!isSearchOpen && (
            <Tooltip content="Find in document (Cmd+F)">
              <button
                onClick={find.open}
                className="p-1.5 hover:text-[var(--accent-blue)] transition-colors mr-1 border-r border-[var(--border-main)] pr-2.5"
              >
                <SearchIcon size={14} />
              </button>
            </Tooltip>
          )}
          {slots?.toolbar}
          {numPages && <span className="w-20 text-center font-mono">{currentPage}/{numPages}</span>}
          {numPages && <span className="text-[var(--text-dim)]">|</span>}
          <ZoomControls
            zoom={zoom}
            onZoomIn={() => setZoomKeepingHorizontalCenter((z) => Math.min(PDF_MAX_ZOOM, +(z + ZOOM_STEP).toFixed(2)))}
            onZoomOut={() => setZoomKeepingHorizontalCenter((z) => Math.max(PDF_MIN_ZOOM, +(z - ZOOM_STEP).toFixed(2)))}
          />
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
          onMouseUp={domSelection.readSelection}
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
              the same commit as a zoom change, instead of trailing the
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

              const isTargetPage = pageNum === targetPage;
              // Per-line emphasis when the relocation produced lines; it
              // replaces the coarse single-box emphasis below.
              const pageTargetRects =
                isTargetPage && !isSearchOpen ? drawnTarget?.rects ?? null : null;

              const innerMatch = innerMatches[currentMatchIdx];
              const innerBbox = innerMatch && innerMatch.page === pageNum ? innerMatch.bbox : null;

              const activeBbox = isSearchOpen
                ? innerBbox
                : pageTargetRects && pageTargetRects.length > 0
                  ? null
                  : isTargetPage
                    ? drawnTarget?.bbox ?? null
                    : null;
              const pageDecorations = rectDecorationsForPage(decorations, pageNum);

              const overlayStyle = activeBbox
                ? highlightRectStyle(activeBbox, pageScale)
                : undefined;

              return (
                <div
                  key={vItem.key}
                  data-page-number={pageNum}
                  style={{ width: "100%", height: pageHeight + PAGE_GAP_PX }}
                >
                  <div style={{ position: "relative", display: "inline-block", height: pageHeight }}>
                    {pdf && (
                      <PdfPageCanvas
                        pdf={pdf}
                        pageNumber={pageNum}
                        width={renderedWidth}
                        canvasBackground="white"
                        onRenderSuccess={() => {
                          const landing = landingPageRef.current ?? (page || 1);
                          if (pageNum === landing) signalInitialRender();
                        }}
                      />
                    )}
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
                    {pageDecorations.map((decoration) => {
                      const { rects } = decoration.anchor;
                      if (rects.length === 0) return null;
                      const { onActivate } = decoration;
                      const activate = (element: Element) =>
                        onActivate?.(decoration.id, elementAnchor(element));
                      return (
                        <div key={decoration.id}>
                          {rects.map((rect, rectIndex) => (
                            <div
                              key={rectIndex}
                              data-testid="decoration"
                              data-decoration-id={decoration.id}
                              className={[
                                "pdf-highlight",
                                decoration.className,
                                onActivate ? "pdf-highlight--clickable" : "",
                              ]
                                .filter(Boolean)
                                .join(" ")}
                              style={highlightRectStyle(rect, pageScale)}
                              role={onActivate ? "button" : undefined}
                              tabIndex={onActivate ? 0 : undefined}
                              aria-label={decoration.ariaLabel}
                              onClick={(event) => activate(event.currentTarget)}
                              onKeyDown={(event) => {
                                if (event.key === "Enter" || event.key === " ") {
                                  event.preventDefault();
                                  activate(event.currentTarget);
                                }
                              }}
                            />
                          ))}
                          {decoration.render && (
                            <div
                              data-decoration-content={decoration.id}
                              style={{
                                position: "absolute",
                                ...highlightRectStyle(unionBox(rects), pageScale),
                              }}
                            >
                              {decoration.render({
                                scale: pageScale,
                                box: unionBox(rects),
                                page: pageNum,
                              })}
                            </div>
                          )}
                        </div>
                      );
                    })}
                    {overlayStyle && <div className="pdf-highlight" style={overlayStyle} />}
                    {pageTargetRects?.map((rect, rectIndex) => (
                      <div
                        key={`target-${rectIndex}`}
                        data-testid="target-highlight"
                        className="pdf-highlight"
                        style={highlightRectStyle(rect, pageScale)}
                      />
                    ))}
                    {!isSearchOpen &&
                      targetBbox &&
                      isTargetPage &&
                      (() => {
                        const { x, y, width, height } = targetBbox;
                        const cx = (x + width / 2) * pageScale;
                        const cy = (y + height / 2) * pageScale;
                        const r = Math.min(width, height) * pageScale * PING_SIZE_RATIO;
                        return (
                          <div
                            key={`${x}-${y}-${width}-${height}`}
                            className="pdf-highlight-ping animate-ping"
                            style={{
                              left: cx - r / 2,
                              top: cy - r / 2,
                              width: r,
                              height: r,
                            }}
                          />
                        );
                      })()}
                    {slots?.pageGutter && (
                      <div
                        data-page-gutter={pageNum}
                        style={{ position: "absolute", left: "100%", top: 0, height: pageHeight }}
                      >
                        {slots.pageGutter(pageNum, {
                          scale: pageScale,
                          width: renderedWidth,
                          height: pageHeight,
                        })}
                      </div>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
      <SelectionLayer
        positioned={domSelection.positioned}
        api={domSelection.slotApi}
        slot={slots?.selectionActions}
      />
    </div>
  );
}
