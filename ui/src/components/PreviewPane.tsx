import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { convertFileSrc } from "@tauri-apps/api/core";
import { EditorState } from "@codemirror/state";
import { EditorView, Decoration, DecorationSet } from "@codemirror/view";
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

  useEffect(() => {
    if (!containerRef.current) return;

    const langExt = getLanguageExtension(language);
    const extensions = [
      basicSetup,
      oneDark,
      EditorState.readOnly.of(true),
      highlightField,
      highlightTheme,
      EditorView.lineWrapping,
    ];
    if (langExt) extensions.push(langExt);

    const state = EditorState.create({ doc: content, extensions });
    const view = new EditorView({ state, parent: containerRef.current });
    viewRef.current = view;

    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, [content, language]); // re-create editor when content/language changes

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
}: {
  url: string;
  page: number;
  highlight_bbox: BoundingBox | null;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(600);
  const [pageWidth, setPageWidth] = useState<number | null>(null);
  const [pageAspectRatio, setPageAspectRatio] = useState<number | null>(null);
  const [numPages, setNumPages] = useState<number | null>(null);
  const [zoom, setZoom] = useState(1.0);

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
    virtualizer.measure();
  }, [zoom]);

  useEffect(() => {
    if (numPages) virtualizer.scrollToIndex(page - 1, { align: "start" });
  }, [page, numPages]);

  // Scale = rendered-width / original-pdf-page-width
  const scale = pageWidth ? renderedWidth / pageWidth : 1;

  return (
    <div className="h-full relative">
      {/* Zoom controls */}
      <div className="absolute bottom-3 right-3 z-20 flex items-center gap-1 bg-neutral-800 border border-neutral-700 rounded px-2 py-1 text-xs text-neutral-300 select-none">
        <button
          onClick={() => setZoom((z) => Math.max(0.25, +(z - 0.25).toFixed(2)))}
          className="px-1 hover:text-white"
        >
          −
        </button>
        <span className="w-10 text-center">{Math.round(zoom * 100)}%</span>
        <button
          onClick={() => setZoom((z) => Math.min(3.0, +(z + 0.25).toFixed(2)))}
          className="px-1 hover:text-white"
        >
          +
        </button>
      </div>
      <div
        ref={containerRef}
        className="h-full overflow-auto bg-neutral-900 pr-1"
        style={{ WebkitUserSelect: "text", userSelect: "text" }}
      >
      <Document
        file={url}
        onLoadSuccess={({ numPages }) => setNumPages(numPages)}
        loading={
          <div className="text-neutral-500 text-sm p-4 animate-pulse">
            Loading PDF…
          </div>
        }
        error={
          <div className="text-red-400 text-sm p-4">Failed to load PDF.</div>
        }
      >
        <div style={{ height: virtualizer.getTotalSize(), position: "relative", minWidth: "fit-content" }}>
          {virtualizer.getVirtualItems().map((vItem) => {
            const pageNum = vItem.index + 1;
            const isMatch = pageNum === page;
            const bbox = isMatch ? highlight_bbox : null;

            let overlayStyle: React.CSSProperties | undefined;
            if (bbox) {
              const { x, y, width, height } = bbox;
              overlayStyle = {
                position: "absolute",
                left: `${x * scale}px`,
                top: `${y * scale}px`,
                width: `${Math.max(width * scale, 4)}px`,
                height: `${Math.max(height * scale, 4)}px`,
                backgroundColor: "rgba(250, 204, 21, 0.25)",
                border: "1px solid rgba(250, 204, 21, 0.8)",
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
                    onLoadSuccess={pageNum === 1 ? (p) => {
                      const vp = p.getViewport({ scale: 1 });
                      setPageWidth(vp.width);
                      setPageAspectRatio(vp.height / vp.width);
                      virtualizer.measure();
                    } : undefined}
                  />
                  {overlayStyle && <div style={overlayStyle} />}
                  {bbox && (() => {
                    const { x, y, width, height } = bbox;
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
                          animationIterationCount: 3,
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
}

function fileName(path: string) {
  return path.split(/[/\\]/).pop() ?? path;
}

export default function PreviewPane({ previewData, loading, selectedMatch }: Props) {
  // Keep the last valid previewData so the content stays mounted while a new
  // match is loading. This prevents PdfViewer from unmounting/remounting on
  // every match click, which would force react-pdf to re-parse the PDF file.
  const lastPreviewRef = useRef<PreviewData | null>(null);
  if (previewData) lastPreviewRef.current = previewData;
  const displayData = previewData ?? lastPreviewRef.current;

  if (!selectedMatch) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-600 text-sm">
        Select a match to preview
      </div>
    );
  }

  if (!displayData) {
    // First-ever load: no cached data to show yet.
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm animate-pulse">
        Loading…
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="px-4 py-2 border-b border-neutral-800 flex items-center gap-2 flex-shrink-0">
        <span className="text-sm font-medium text-neutral-200 truncate">
          {fileName(selectedMatch.path)}
        </span>
        <span className="text-xs text-neutral-500 truncate flex-1">
          {selectedMatch.path}
        </span>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden relative">
        {loading && (
          <div className="absolute inset-0 flex items-center justify-center bg-neutral-950/60 z-10 pointer-events-none">
            <span className="text-neutral-400 text-sm animate-pulse">Loading…</span>
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
            url={convertFileSrc(selectedMatch.path)}
            page={displayData.Pdf.page}
            highlight_bbox={displayData.Pdf.highlight_bbox}
          />
        )}
      </div>
    </div>
  );
}
