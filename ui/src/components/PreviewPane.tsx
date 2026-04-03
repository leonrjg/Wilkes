import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { EditorState } from "@codemirror/state";
import { EditorView, Decoration, DecorationSet } from "@codemirror/view";
import { Search as SearchIcon, ChevronUp, ChevronDown, X, ArrowLeft, ArrowRight } from "react-feather";
import { basicSetup } from "codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { xml } from "@codemirror/lang-xml";
import { sql } from "@codemirror/lang-sql";
import { cpp } from "@codemirror/lang-cpp";
import { java } from "@codemirror/lang-java";
import { go } from "@codemirror/lang-go";
import { yaml } from "@codemirror/lang-yaml";
import { RangeSetBuilder } from "@codemirror/state";
import { StateField, StateEffect } from "@codemirror/state";
import { Document, Page, pdfjs } from "react-pdf";
import "react-pdf/dist/Page/TextLayer.css";
import type { MatchRef, PreviewData, BoundingBox } from "../lib/types";
import type { SearchApi } from "../services/api";
import type { PDFDocumentProxy } from "pdfjs-dist";

pdfjs.GlobalWorkerOptions.workerSrc = new URL(
  "pdfjs-dist/build/pdf.worker.min.mjs",
  import.meta.url,
).toString();

// ── Highlight effect / field ──────────────────────────────────────────────────

const setHighlight = StateEffect.define<{ from: number; to: number } | null>();

const highlightField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) {
      if (e.is(setHighlight)) {
        if (e.value === null) return Decoration.none;
        const { from, to } = e.value;
        const builder = new RangeSetBuilder<Decoration>();
        builder.add(
          from,
          to,
          Decoration.mark({ class: "cm-highlight-match" }),
        );
        return builder.finish();
      }
    }
    return deco.map(tr.changes);
  },
  provide: (f) => EditorView.decorations.from(f),
});

const highlightTheme = EditorView.baseTheme({
  ".cm-highlight-match": {
    backgroundColor: "rgba(250, 204, 21, 0.25)",
    borderBottom: "2px solid rgba(250, 204, 21, 0.7)",
  },
});

// ── Language detection ────────────────────────────────────────────────────────

function getLanguageExtension(lang: string | null) {
  switch (lang) {
    case "javascript":
    case "typescript":
      return javascript({ typescript: lang === "typescript" });
    case "python":
      return python();
    case "rust":
      return rust();
    case "json":
      return json();
    case "markdown":
      return markdown();
    case "html":
      return html();
    case "css":
      return css();
    case "xml":
      return xml();
    case "sql":
      return sql();
    case "cpp":
    case "c":
      return cpp();
    case "java":
      return java();
    case "go":
      return go();
    case "yaml":
      return yaml();
    default:
      return null;
  }
}

// ── CodeMirror viewer ─────────────────────────────────────────────────────────

interface CodeViewerProps {
  content: string;
  language: string | null;
  highlightLine: number;
  highlightRange: { start: number; end: number };
}

function CodeViewer({ content, language, highlightLine, highlightRange }: CodeViewerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));

  // Observe theme changes on the html element
  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!containerRef.current) return;

    const langExt = getLanguageExtension(language);
    const extensions = [
      basicSetup,
      EditorState.readOnly.of(true),
      highlightField,
      highlightTheme,
      EditorView.lineWrapping,
    ];
    if (isDark) extensions.push(oneDark);
    if (langExt) extensions.push(langExt);

    const state = EditorState.create({ doc: content, extensions });
    const view = new EditorView({ state, parent: containerRef.current });
    viewRef.current = view;

    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, [content, language, isDark]);

  // Apply highlight whenever highlight params change
  useEffect(() => {
    const view = viewRef.current;
    if (!view || !content) return;

    // Clamp to document length
    const docLen = view.state.doc.length;
    const from = Math.min(highlightRange.start, docLen);
    const to = Math.min(highlightRange.end, docLen);

    view.dispatch({
      effects: setHighlight.of({ from, to }),
    });

    // Scroll to the highlighted line
    if (highlightLine > 0 && highlightLine <= view.state.doc.lines) {
      const lineInfo = view.state.doc.line(highlightLine);
      view.dispatch({
        effects: EditorView.scrollIntoView(lineInfo.from, { y: "center" }),
      });
    }
  }, [content, highlightLine, highlightRange]);

  return (
    <div ref={containerRef} className="h-full w-full overflow-auto text-sm" />
  );
}

// ── PDF viewer ────────────────────────────────────────────────────────────────

function PdfViewer({
  url,
  page,
  highlight_bbox,
  onRenderSuccess,
}: {
  url: string;
  page: number;
  highlight_bbox: BoundingBox | null;
  onRenderSuccess?: () => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [containerWidth, setContainerWidth] = useState(600);
  const [pageWidth, setPageWidth] = useState<number | null>(null);
  const [pageAspectRatio, setPageAspectRatio] = useState<number | null>(null);
  const [numPages, setNumPages] = useState<number | null>(null);
  const [zoom, setZoom] = useState(1.0);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const hasCalledRenderSuccess = useRef(false);

  // Find in document state
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [innerQuery, setInnerQuery] = useState("");
  const [innerMatches, setInnerMatches] = useState<Array<{ page: number; bbox: BoundingBox }>>([]);
  const [currentMatchIdx, setCurrentMatchIdx] = useState(-1);
  const [isSearching, setIsSearching] = useState(false);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));

  // Reset success flag when URL changes
  useEffect(() => {
    hasCalledRenderSuccess.current = false;
  }, [url]);

  // Observe theme changes
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

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0].contentRect.width;
      if (w > 0) {
        setContainerWidth(w);
        virtualizer.measure();
      }
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    if (zoom) {
      hasCalledRenderSuccess.current = false;
      virtualizer.measure();
    }
  }, [zoom]);

  useEffect(() => {
    if (numPages && !isSearchOpen) {
      virtualizer.scrollToIndex(page - 1, { align: "start" });
    }
  }, [page, numPages, isSearchOpen]);

  // Scale = rendered-width / original-pdf-page-width
  const scale = pageWidth ? renderedWidth / pageWidth : 1;

  // Handle inner search
  useEffect(() => {
    if (!isSearchOpen || !innerQuery.trim() || !pdf) {
      setInnerMatches([]);
      setCurrentMatchIdx(-1);
      return;
    }

    const abort = new AbortController();
    const search = async () => {
      setIsSearching(true);
      const matches: Array<{ page: number; bbox: BoundingBox }> = [];
      const query = innerQuery.toLowerCase();

      try {
        for (let i = 1; i <= pdf.numPages; i++) {
          if (abort.signal.aborted) return;
          const page = await pdf.getPage(i);
          const textContent = await page.getTextContent();
          
          for (const item of textContent.items) {
            if ("str" in item) {
              const text = item.str.toLowerCase();
              if (text.includes(query)) {
                // item.transform is [scaleX, skewY, skewX, scaleY, translateX, translateY]
                // PDF space origin is bottom-left, y increases up.
                // We need to convert to top-left relative for our renderer.
                const [scX, _skY, _skX, scY, tx, ty] = item.transform;
                const vp = page.getViewport({ scale: 1 });
                
                matches.push({
                  page: i,
                  bbox: {
                    x: tx,
                    y: vp.height - ty - scY, // Flip Y
                    width: item.width || (text.length * scX * 0.6), // Fallback if width missing
                    height: Math.abs(scY),
                  }
                });
              }
            }
          }
        }
        
        if (!abort.signal.aborted) {
          setInnerMatches(matches);
          setCurrentMatchIdx(matches.length > 0 ? 0 : -1);
          if (matches.length > 0) {
            virtualizer.scrollToIndex(matches[0].page - 1, { align: "start" });
          }
        }
      } catch (e) {
        console.error("PDF inner search failed:", e);
      } finally {
        if (!abort.signal.aborted) setIsSearching(false);
      }
    };

    const timeout = setTimeout(search, 300);
    return () => {
      abort.abort();
      clearTimeout(timeout);
    };
  }, [innerQuery, isSearchOpen, pdf]);

  useEffect(() => {
    if (currentMatchIdx >= 0 && innerMatches[currentMatchIdx]) {
      virtualizer.scrollToIndex(innerMatches[currentMatchIdx].page - 1, { align: "start" });
    }
  }, [currentMatchIdx, innerMatches]);

  const handleNextMatch = () => {
    if (innerMatches.length === 0) return;
    setCurrentMatchIdx((prev) => (prev + 1) % innerMatches.length);
  };

  const handlePrevMatch = () => {
    if (innerMatches.length === 0) return;
    setCurrentMatchIdx((prev) => (prev - 1 + innerMatches.length) % innerMatches.length);
  };

  // Listen for Cmd/Ctrl+F
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        setIsSearchOpen(true);
        setTimeout(() => searchInputRef.current?.focus(), 50);
      }
      if (e.key === "Escape" && isSearchOpen) {
        setIsSearchOpen(false);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isSearchOpen]);

  return (
    <div className="h-full relative flex flex-col">
      {/* Zoom and Search controls */}
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
                className="bg-transparent border-none outline-none px-2 py-1 text-xs text-[var(--text-main)] placeholder-[var(--text-dim)] w-48"
              />
            </div>
            {innerMatches.length > 0 && (
              <span className="text-[10px] text-[var(--text-muted)] font-mono px-1">
                {currentMatchIdx + 1}/{innerMatches.length}
              </span>
            )}
            {isSearching && <div className="w-3 h-3 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin mx-1" />}
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
        style={{ 
          WebkitUserSelect: "text", 
          userSelect: "text",
          transition: "filter 0.3s ease"
        }}
      >
      <Document
        file={url}
        onLoadSuccess={(pdf) => {
          setPdf(pdf);
          setNumPages(pdf.numPages);
        }}
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative", minWidth: "fit-content" }}>
          {virtualizer.getVirtualItems().map((vItem) => {
            const pageNum = vItem.index + 1;
            
            // Primary highlight from props (the search result that opened this PDF)
            const isTargetPage = pageNum === page;
            const targetBbox = isTargetPage ? highlight_bbox : null;

            // Secondary highlight from inner search
            const innerMatch = innerMatches[currentMatchIdx];
            const innerBbox = (innerMatch && innerMatch.page === pageNum) ? innerMatch.bbox : null;

            // Render both if they exist, but prioritize showing the inner match if search is open
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
                backgroundColor: isSearchOpen ? "rgba(59, 130, 246, 0.25)" : "rgba(250, 204, 21, 0.25)",
                border: isSearchOpen ? "1px solid rgba(59, 130, 246, 0.8)" : "1px solid rgba(250, 204, 21, 0.8)",
                borderRadius: "2px",
                pointerEvents: "none",
              };
            }

            return (
              <div
                key={vItem.key}
                style={{
                  position: "absolute",
                  top: vItem.start,
                  width: "100%",
                }}
              >
                <div style={{ position: "relative", display: "inline-block" }}>
                  <Page
                    pageNumber={pageNum}
                    width={renderedWidth}
                    renderAnnotationLayer={false}
                    renderTextLayer={true}
                    canvasBackground="transparent"
                    onRenderSuccess={() => {
                      if (pageNum === page || (!page && pageNum === 1)) {
                        if (!hasCalledRenderSuccess.current) {
                          hasCalledRenderSuccess.current = true;
                          onRenderSuccess?.();
                        }
                      }
                    }}
                    onLoadSuccess={pageNum === 1 ? (p) => {
                      const vp = p.getViewport({ scale: 1 });
                      setPageWidth(vp.width);
                      setPageAspectRatio(vp.height / vp.width);
                      virtualizer.measure();
                    } : undefined}
                  />
                  {overlayStyle && <div style={overlayStyle} />}
                  {!isSearchOpen && targetBbox && isTargetPage && (() => {
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
                          width: r * 2,
                          height: r * 2,
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


// ── PreviewPane ───────────────────────────────────────────────────────────────

interface Props {
  previewData: PreviewData | null;
  loading: boolean;
  selectedMatch: MatchRef | null;
  api: SearchApi;
  onClose: () => void;
}

function fileName(path: string) {
  return path.split(/[/\\]/).pop() ?? path;
}

export default function PreviewPane({ previewData, loading, selectedMatch, api, onClose }: Props) {
  // Keep the last valid previewData so the content stays mounted while a new
  // match is loading. This prevents PdfViewer from unmounting/remounting on
  // every match click, which would force react-pdf to re-parse the PDF file.
  const lastPreviewRef = useRef<PreviewData | null>(null);
  const [isPdfRendering, setIsPdfRendering] = useState(false);

  if (previewData) lastPreviewRef.current = previewData;
  const displayData = previewData ?? lastPreviewRef.current;

  // Whenever the selection changes, we're definitely loading a new view
  useEffect(() => {
    if (selectedMatch) {
      const isPdf = selectedMatch.path.toLowerCase().endsWith(".pdf");
      setIsPdfRendering(isPdf);
    } else {
      setIsPdfRendering(false);
    }
  }, [selectedMatch?.path, selectedMatch?.origin]);

  if (!selectedMatch) {
    return (
      <div className="flex flex-col items-center justify-center h-full bg-[var(--bg-app)] text-[var(--text-dim)]">
        <div className="w-80 h-80 mb-8 opacity-20 grayscale brightness-150 transition-all hover:opacity-40 hover:grayscale-0">
          <img src="/logo.transparent.png" alt="Wilkes" className="w-full h-full object-contain" />
        </div>
        <div className="flex flex-col items-center gap-1">
          <span className="text-sm font-medium">Select a file to preview</span>
          <span className="text-[11px] opacity-60">Search results and document contents will appear here</span>
        </div>
      </div>
    );
  }

  if (!displayData) {
    // First-ever load: no cached data to show yet.
    return (
      <div className="flex items-center justify-center h-full text-[var(--text-muted)] text-sm animate-pulse">
        Loading…
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="px-3 py-2 border-b border-[var(--border-main)] flex items-center gap-3 flex-shrink-0 bg-[var(--bg-header)]">
        <div className="flex items-center gap-1">
          <button
            disabled
            className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            title="Go back"
          >
            <ArrowLeft size={14} />
          </button>
          <button
            disabled
            className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            title="Go forward"
          >
            <ArrowRight size={14} />
          </button>
        </div>

        <div className="flex flex-col min-w-0 flex-1 selectable">
          <span className="text-xs font-medium text-[var(--text-main)] truncate leading-tight">
            {fileName(selectedMatch.path)}
          </span>
          <span className="text-[10px] text-[var(--text-dim)] truncate leading-tight">
            {selectedMatch.path}
          </span>
        </div>

        <button
          onClick={onClose}
          className="p-1 hover:bg-red-500/10 hover:text-red-500 rounded text-[var(--text-dim)] transition-colors"
          title="Close preview"
        >
          <X size={16} />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden relative bg-[var(--bg-app)]">
        {(loading || isPdfRendering) && (
          <div className="absolute inset-0 flex items-center justify-center bg-[var(--bg-app)] z-30 pointer-events-none">
            <div className="flex flex-col items-center gap-3">
              <div className="w-6 h-6 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
              <span className="text-[var(--text-muted)] text-sm animate-pulse">Loading document…</span>
            </div>
          </div>
        )}
        {"Text" in displayData ? (
          <CodeViewer
            content={displayData.Text.content}
            language={displayData.Text.language}
            highlightLine={displayData.Text.highlight_line}
            highlightRange={displayData.Text.highlight_range}
          />
        ) : (
          <PdfViewer
            url={api.resolvePdfUrl(selectedMatch.path)}
            page={displayData.Pdf.page}
            highlight_bbox={displayData.Pdf.highlight_bbox}
            onRenderSuccess={() => setIsPdfRendering(false)}
          />
        )}
      </div>
    </div>
  );
}
