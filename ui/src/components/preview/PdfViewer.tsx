import { useCallback, useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Search as SearchIcon, ChevronUp, ChevronDown, X } from "react-feather";
import { Document, Page, pdfjs } from "react-pdf";
import type { BoundingBox } from "../../lib/types";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { usePdfInnerSearch } from "./usePdfInnerSearch";
import { getScaledPageHeight, usePdfPageMetrics } from "./usePdfPageMetrics";
import PdfTextLayer from "./PdfTextLayer";

pdfjs.GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

export interface PdfViewerProps {
  url: string;
  page: number;
  highlight_bbox: BoundingBox | null;
  bookmarkHighlights?: Array<{ id: string; page: number; bbox: BoundingBox }>;
  onRenderSuccess?: () => void;
  onAddBookmark?: (bookmark: { page: number; bbox: BoundingBox; quote: string }) => void;
}

const PAGE_GAP_PX = 12;

export default function PdfViewer({
  url,
  page,
  highlight_bbox,
  bookmarkHighlights = [],
  onRenderSuccess,
  onAddBookmark,
}: PdfViewerProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(600);
  const [numPages, setNumPages] = useState<number | null>(null);
  const [currentPage, setCurrentPage] = useState(page);
  const prevNavigationTargetRef = useRef<{ page: number; bbox: BoundingBox | null } | null>(null);
  const [zoom, setZoom] = useState(1.0);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));
  const [selectionBookmark, setSelectionBookmark] = useState<{
    page: number;
    bbox: BoundingBox;
    quote: string;
    buttonLeft: number;
    buttonTop: number;
  } | null>(null);

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  const renderedWidth = containerWidth * zoom;
  const { pageMetrics, hasPageMetrics } = usePdfPageMetrics(pdf, url);

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

    setSelectionBookmark({
      page: pageNumber,
      bbox: {
        x: (selectionRect.left - pageRect.left) / pageScale,
        y: (selectionRect.top - pageRect.top) / pageScale,
        width: selectionRect.width / pageScale,
        height: selectionRect.height / pageScale,
      },
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

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0].contentRect.width;
      if (w > 0) {
        setContainerWidth(w);
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const prevTarget = prevNavigationTargetRef.current;
    const navigationChanged =
      !prevTarget ||
      prevTarget.page !== page ||
      prevTarget.bbox !== highlight_bbox;

    if (hasPageMetrics && !isSearchOpen && navigationChanged) {
      virtualizer.scrollToIndex(page - 1, { align: "start" });
      setCurrentPage(page);
    }

    if (hasPageMetrics) {
      prevNavigationTargetRef.current = { page, bbox: highlight_bbox };
    }
  }, [page, hasPageMetrics, highlight_bbox, isSearchOpen, virtualizer]);

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

  return (
    <div ref={rootRef} className="h-full relative flex flex-col">
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
          {!isSearchOpen && (
            <button
              onClick={() => {
                setIsSearchOpen(true);
                setTimeout(() => searchInputRef.current?.focus(), 50);
              }}
              className="p-1 hover:text-[var(--accent-blue)] transition-colors mr-1 border-r border-[var(--border-main)] pr-2"
              title="Find in document (Cmd+F)"
            >
              <SearchIcon size={12} />
            </button>
          )}
          {numPages && <span className="w-16 text-center font-mono">{currentPage}/{numPages}</span>}
          {numPages && <span className="text-[var(--text-dim)]">|</span>}
          <button
            onClick={() => setZoom((z) => Math.max(0.25, +(z - 0.25).toFixed(2)))}
            className="px-1 hover:text-[var(--accent-blue)]"
          >
            −
          </button>
          <span className="w-10 text-center font-mono">{Math.round(zoom * 100)}%</span>
          <button
            onClick={() => setZoom((z) => Math.min(3.0, +(z + 0.25).toFixed(2)))}
            className="px-1 hover:text-[var(--accent-blue)]"
          >
            +
          </button>
        </div>
      </div>

      <div
        ref={containerRef}
        className={`flex-1 overflow-auto bg-[var(--bg-sidebar)] pr-1 ${isDark ? "pdf-dark-mode" : ""}`}
        onMouseUp={handleMouseUp}
        onScroll={() => {
          requestAnimationFrame(syncCurrentPageFromScroll);
        }}
        style={{
          WebkitUserSelect: "text",
          userSelect: "text",
          transition: "filter 0.3s ease",
        }}
      >
        <Document
          file={url}
          onLoadSuccess={(doc) => {
            setPdf(doc);
            setNumPages(doc.numPages);
          }}
        >
          <div style={{ paddingTop, paddingBottom, minWidth: "fit-content" }}>
            {virtualItems.map((vItem) => {
              const pageNum = vItem.index + 1;
              const pageMetric = pageMetrics[vItem.index];
              if (!pageMetric) return null;
              const pageScale = renderedWidth / pageMetric.width;
              const pageHeight = getScaledPageHeight(pageMetric, renderedWidth);

              const isTargetPage = pageNum === page;
              const targetBbox = isTargetPage ? highlight_bbox : null;

              const innerMatch = innerMatches[currentMatchIdx];
              const innerBbox = innerMatch && innerMatch.page === pageNum ? innerMatch.bbox : null;

              const activeBbox = isSearchOpen ? innerBbox : targetBbox;
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
                      pageNumber={pageNum}
                      width={renderedWidth}
                      renderAnnotationLayer={false}
                      renderTextLayer={false}
                      canvasBackground="white"
                      onRenderSuccess={() => {
                        if (pageNum === page || (!page && pageNum === 1)) {
                          onRenderSuccess?.();
                        }
                      }}
                    />
                    {pdf && (
                      <PdfTextLayer pdf={pdf} pageNumber={pageNum} scale={pageScale} />
                    )}
                    {pageBookmarkHighlights.map((highlight) => {
                      const { x, y, width, height } = highlight.bbox;
                      return (
                        <div
                          key={highlight.id}
                          data-testid="bookmark-highlight"
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
                    })}
                    {overlayStyle && <div style={overlayStyle} />}
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
        </Document>
      </div>
      {selectionBookmark && onAddBookmark && (
        <button
          type="button"
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            onAddBookmark(selectionBookmark);
            setSelectionBookmark(null);
            window.getSelection()?.removeAllRanges();
          }}
          className="absolute z-40 px-2 py-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-xs text-[var(--text-main)] shadow-lg hover:border-[var(--border-strong)]"
          style={{ left: selectionBookmark.buttonLeft, top: selectionBookmark.buttonTop }}
        >
          + Bookmark
        </button>
      )}
    </div>
  );
}
