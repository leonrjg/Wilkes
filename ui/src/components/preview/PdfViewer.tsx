import { useCallback, useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Search as SearchIcon, ChevronUp, ChevronDown, X } from "react-feather";
import { Document, Page, pdfjs } from "react-pdf";
import "react-pdf/dist/Page/TextLayer.css";
import type { BoundingBox } from "../../lib/types";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { usePdfInnerSearch } from "./usePdfInnerSearch";

pdfjs.GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

export interface PdfViewerProps {
  url: string;
  page: number;
  highlight_bbox: BoundingBox | null;
  onRenderSuccess?: () => void;
}

export default function PdfViewer({ url, page, highlight_bbox, onRenderSuccess }: PdfViewerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(600);
  const [pageWidth, setPageWidth] = useState<number | null>(null);
  const [pageAspectRatio, setPageAspectRatio] = useState<number | null>(null);
  const [numPages, setNumPages] = useState<number | null>(null);
  const [currentPage, setCurrentPage] = useState(page);
  const prevNavigationTargetRef = useRef<{ page: number; bbox: BoundingBox | null } | null>(null);
  const [zoom, setZoom] = useState(1.0);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  const renderedWidth = containerWidth * zoom;

  const virtualizer = useVirtualizer({
    count: numPages ?? 0,
    getScrollElement: () => containerRef.current,
    estimateSize: () => (pageAspectRatio ? pageAspectRatio * renderedWidth : 900),
    overscan: 2,
  });

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
    virtualizer.measure();
  }, [pageAspectRatio, renderedWidth, virtualizer]);

  useEffect(() => {
    const prevTarget = prevNavigationTargetRef.current;
    const navigationChanged =
      !prevTarget ||
      prevTarget.page !== page ||
      prevTarget.bbox !== highlight_bbox;

    if (numPages && !isSearchOpen && navigationChanged) {
      virtualizer.scrollToIndex(page - 1, { align: "start" });
      setCurrentPage(page);
    }

    prevNavigationTargetRef.current = { page, bbox: highlight_bbox };
  }, [page, numPages, highlight_bbox, isSearchOpen, virtualizer]);

  useEffect(() => {
    if (numPages) {
      setCurrentPage((prev) => Math.min(Math.max(prev, 1), numPages));
    }
  }, [numPages]);

  useEffect(() => {
    if (numPages) {
      requestAnimationFrame(syncCurrentPageFromScroll);
    }
  }, [numPages, zoom, syncCurrentPageFromScroll]);

  const scale = pageWidth ? renderedWidth / pageWidth : 1;

  return (
    <div className="h-full relative flex flex-col">
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
          <div style={{ height: virtualizer.getTotalSize(), position: "relative", minWidth: "fit-content" }}>
            {virtualizer.getVirtualItems().map((vItem) => {
              const pageNum = vItem.index + 1;

              const isTargetPage = pageNum === page;
              const targetBbox = isTargetPage ? highlight_bbox : null;

              const innerMatch = innerMatches[currentMatchIdx];
              const innerBbox = innerMatch && innerMatch.page === pageNum ? innerMatch.bbox : null;

              const activeBbox = isSearchOpen ? innerBbox : targetBbox;

              let overlayStyle: React.CSSProperties | undefined;
              if (activeBbox) {
                const { x, y, width, height } = activeBbox;
                overlayStyle = {
                  position: "absolute",
                  left: `${x * scale}px`,
                  top: `${y * scale}px`,
                  width: `${Math.max(width * scale, 4)}px`,
                  height: `${Math.max(height * scale, 4)}px`,
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
                  style={{ position: "absolute", top: vItem.start, width: "100%" }}
                >
                  <div style={{ position: "relative", display: "inline-block" }}>
                    <Page
                      pageNumber={pageNum}
                      width={renderedWidth}
                      renderAnnotationLayer={false}
                      renderTextLayer={true}
                      canvasBackground="white"
                      onRenderSuccess={() => {
                        if (pageNum === page || (!page && pageNum === 1)) {
                          onRenderSuccess?.();
                        }
                      }}
                      onLoadSuccess={
                        pageNum === 1
                          ? (p) => {
                              const vp = p.getViewport({ scale: 1 });
                              setPageWidth(vp.width);
                              setPageAspectRatio(vp.height / vp.width);
                            }
                          : undefined
                      }
                    />
                    {overlayStyle && <div style={overlayStyle} />}
                    {!isSearchOpen &&
                      targetBbox &&
                      isTargetPage &&
                      (() => {
                        const { x, y, width, height } = targetBbox;
                        const cx = (x + width / 2) * scale;
                        const cy = (y + height / 2) * scale;
                        const r = Math.max(width, height) * scale;
                        return (
                          <div
                            key={`${x}-${y}-${width}-${height}`}
                            className="animate-ping pointer-events-none"
                            style={{
                              position: "absolute",
                              left: cx - r,
                              top: cy - r,
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
    </div>
  );
}
