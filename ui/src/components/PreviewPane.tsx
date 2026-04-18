import { useEffect, useRef, useState } from "react";
import { X, ArrowLeft, ArrowRight, ExternalLink, Copy } from "react-feather";
import CodeViewer from "./preview/CodeViewer";
import PdfViewer from "./preview/PdfViewer";
import { useSearchStore } from "../stores/useSearchStore";
import { api } from "../services";
import type { DocumentMetadata, ViewerMetadataStatus } from "../lib/types";
import { buildExternalLinks } from "../lib/externalLinks";

interface Props {
  canGoBack?: boolean;
  canGoForward?: boolean;
  onGoBack?: () => void;
  onGoForward?: () => void;
}

function fileName(path: string) {
  return path.split(/[/\\]/).pop() ?? path;
}

function headerTitle(path: string, metadata: DocumentMetadata | null) {
  const title = metadata?.title?.trim();
  return title && title.length > 0 ? title : fileName(path);
}

function formatCreatedAt(createdAt: string | null | undefined) {
  if (!createdAt) return null;

  const match = /^(\d{4})-(\d{2})$/.exec(createdAt);
  if (!match) return null;

  const [, year, month] = match;
  const monthIndex = Number(month) - 1;
  const date = new Date(Date.UTC(Number(year), monthIndex, 1));
  if (Number.isNaN(date.getTime())) return null;

  return new Intl.DateTimeFormat("en", {
    month: "short",
    year: "numeric",
    timeZone: "UTC",
  }).format(date);
}

function actionButtonClassName(compact = false) {
  return [
    "inline-flex items-center transition-colors border border-[var(--border-main)]",
    "bg-[var(--bg-active)] hover:text-[var(--text-main)] hover:border-[var(--border-strong)]",
    compact ? "gap-1 px-1.5 py-0.5 rounded" : "gap-1 px-2 py-0.5 rounded",
  ].join(" ");
}

function groupedActionClassName() {
  return [
    "inline-flex items-stretch overflow-hidden rounded border border-[var(--border-main)]",
    "bg-[var(--bg-active)]",
  ].join(" ");
}

function groupedActionSegmentClassName() {
  return [
    "inline-flex items-center gap-1 px-2 py-0.5 transition-colors",
    "hover:text-[var(--text-main)] hover:bg-[var(--bg-header)]",
  ].join(" ");
}

function metadataBadgeClassName() {
  return [
    "inline-flex items-center px-1.5 py-0.5 rounded border border-[var(--border-main)]",
    "bg-[var(--bg-active)] text-[var(--text-main)]",
  ].join(" ");
}

export default function PreviewPane({ canGoBack = false, canGoForward = false, onGoBack, onGoForward }: Props) {
  const selectedMatch = useSearchStore((s) => s.selectedMatch);
  const previewData = useSearchStore((s) => s.previewData);
  const previewLoading = useSearchStore((s) => s.previewLoading);
  const viewerMetadata = useSearchStore((s) => s.viewerMetadata);
  const viewerMetadataStatus = useSearchStore((s) => s.viewerMetadataStatus);
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
  const author = viewerMetadata?.author?.trim() || null;
  const createdAt = formatCreatedAt(viewerMetadata?.created_at);
  const links = buildExternalLinks(viewerMetadata?.doi);
  const doi = links?.doi ?? null;

  const handleOpenDoi = () => {
    if (!links) return;
    api.openPath(links.doiUrl).catch((e) => console.error("Open DOI failed:", e));
  };

  const handleOpenScholar = () => {
    if (!links) return;
    api.openPath(links.googleScholarUrl).catch((e) => console.error("Open Google Scholar failed:", e));
  };

  const handleCopyDoi = () => {
    if (!doi) return;
    Promise.resolve(navigator.clipboard?.writeText(doi)).catch((e) =>
      console.error("Copy DOI failed:", e),
    );
  };

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
            {headerTitle(selectedMatch.path, viewerMetadata)}
          </span>
          <div className="flex items-center gap-1 min-w-0 text-[10px] text-[var(--text-dim)] leading-tight">
            {createdAt && <span className={metadataBadgeClassName()}>{createdAt}</span>}
            {author && <span className="truncate">{author}</span>}
            {!createdAt && !author && viewerMetadataStatus === "loading" && <span>Loading metadata…</span>}
            {(createdAt || author || viewerMetadataStatus === "loading") && <span aria-hidden="true">·</span>}
            {doi && (
              <>
                <div className={groupedActionClassName()}>
                  <button
                    onClick={handleOpenDoi}
                    className={groupedActionSegmentClassName()}
                    title={`Open DOI ${doi}`}
                  >
                    <span className="truncate max-w-[140px]">DOI: {doi}</span>
                    <ExternalLink size={10} />
                  </button>
                  <button
                    onClick={handleCopyDoi}
                    className={`${groupedActionSegmentClassName()} border-l border-[var(--border-main)]`}
                    title={`Copy DOI ${doi}`}
                  >
                    <Copy size={10} />
                  </button>
                </div>
                <button
                  onClick={handleOpenScholar}
                  className={actionButtonClassName()}
                  title={`Open Google Scholar for DOI ${doi}`}
                >
                  <span>Scholar</span>
                  <ExternalLink size={10} />
                </button>
                <span aria-hidden="true">·</span>
              </>
            )}
            <span className="truncate min-w-0 flex-1">{selectedMatch.path}</span>
          </div>
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
