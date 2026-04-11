import { useEffect, useRef, useState } from "react";
import { X, ArrowLeft, ArrowRight } from "react-feather";
import CodeViewer from "./preview/CodeViewer";
import PdfViewer from "./preview/PdfViewer";
import { useSearchStore } from "../stores/useSearchStore";
import { api } from "../services";

interface Props {
  canGoBack?: boolean;
  canGoForward?: boolean;
  onGoBack?: () => void;
  onGoForward?: () => void;
}

function fileName(path: string) {
  return path.split(/[/\\]/).pop() ?? path;
}

export default function PreviewPane({ canGoBack = false, canGoForward = false, onGoBack, onGoForward }: Props) {
  const selectedMatch = useSearchStore((s) => s.selectedMatch);
  const previewData = useSearchStore((s) => s.previewData);
  const previewLoading = useSearchStore((s) => s.previewLoading);
  const clearPreview = useSearchStore((s) => s.clearPreview);

  // Keep the last valid previewData so the content stays mounted while a new
  // match is loading. This prevents PdfViewer from unmounting/remounting on
  // every match click, which would force react-pdf to re-parse the PDF file.
  const lastPreviewRef = useRef(previewData);
  const [isPdfRendering, setIsPdfRendering] = useState(false);
  const prevPdfUrlRef = useRef<string | null>(null);

  if (previewData) lastPreviewRef.current = previewData;
  const displayData = previewData ?? lastPreviewRef.current;

  // Show the loading spinner only when a new PDF file is opened, not when
  // navigating to a different match within the same file.
  useEffect(() => {
    if (selectedMatch) {
      const isPdf = selectedMatch.path.toLowerCase().endsWith(".pdf");
      if (isPdf) {
        const newUrl = api.resolvePdfUrl(selectedMatch.path);
        const isNewFile = newUrl !== prevPdfUrlRef.current;
        prevPdfUrlRef.current = newUrl;
        if (isNewFile) setIsPdfRendering(true);
      } else {
        prevPdfUrlRef.current = null;
        setIsPdfRendering(false);
      }
    } else {
      prevPdfUrlRef.current = null;
      setIsPdfRendering(false);
    }
  }, [selectedMatch?.path, selectedMatch?.origin]);

  if (!selectedMatch) {
    return (
      <div className="flex flex-col items-center justify-center h-full bg-[var(--bg-app)] text-[var(--text-dim)]">
        <img src="/logo.transparent.png" alt="Wilkes" className="max-h-72 w-auto mb-8 opacity-20 transition-all hover:opacity-50 -translate-x-2" />
        <div className="flex flex-col items-center gap-1">
          <span className="text-sm font-medium">Select a file or perform a search</span>
          <span className="text-[11px] opacity-60">Search results and documents will appear here</span>
        </div>
      </div>
    );
  }

  const isPdfFile = "PdfPage" in selectedMatch.origin;
  const pdfPage = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : 1;
  const pdfBbox = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.bbox : null;

  if (!isPdfFile && !displayData) {
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
            onClick={onGoBack}
            disabled={!canGoBack}
            className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            title="Go back"
          >
            <ArrowLeft size={14} />
          </button>
          <button
            onClick={onGoForward}
            disabled={!canGoForward}
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
          onClick={clearPreview}
          className="p-1 hover:bg-red-500/10 hover:text-red-500 rounded text-[var(--text-dim)] transition-colors"
          title="Close preview"
        >
          <X size={16} />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-hidden relative bg-[var(--bg-app)]">
        {(previewLoading || isPdfRendering) && (
          <div className="absolute inset-0 flex items-center justify-center bg-[var(--bg-app)] z-30 pointer-events-none">
            <div className="flex flex-col items-center gap-3">
              <div className="w-6 h-6 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
              <span className="text-[var(--text-muted)] text-sm animate-pulse">Loading document…</span>
            </div>
          </div>
        )}
        {isPdfFile ? (
          <PdfViewer
            key={api.resolvePdfUrl(selectedMatch.path)}
            url={api.resolvePdfUrl(selectedMatch.path)}
            page={pdfPage}
            highlight_bbox={pdfBbox}
            onRenderSuccess={() => setIsPdfRendering(false)}
          />
        ) : displayData && "Text" in displayData ? (
          <CodeViewer
            content={displayData.Text.content}
            language={displayData.Text.language}
            highlightLine={displayData.Text.highlight_line}
            highlightRange={displayData.Text.highlight_range}
          />
        ) : null}
      </div>
    </div>
  );
}
