import { useEffect, useRef, useState } from "react";
import { X, ArrowLeft, ArrowRight, ExternalLink, Check, Copy, Link2, Code, Eye } from "react-feather";
import CodeViewer from "./preview/CodeViewer";
import MarkdownViewer from "./preview/MarkdownViewer";
import PdfViewer from "./preview/PdfViewer";
import type { DocumentSelection } from "./preview/SelectionActions";
import { utf8ByteRangeToUtf16Range } from "./preview/textOffsets";
import { readMarkdownViewMode, saveMarkdownViewMode } from "./preview/textScrollMemory";
import { useSearchStore } from "../stores/useSearchStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useChatStore } from "../stores/useChatStore";
import { api, isTauri } from "../services";
import type { BoundingBox, DocumentMetadata } from "../lib/types";
import { buildExternalLinks } from "../lib/externalLinks";
import { formatDocumentMonthYear } from "../lib/dateFormatting";
import { useToasts } from "./Toast";
import { Tooltip } from "./Tooltip";
import { CopyButton } from "./CopyButton";
import { fileName } from "./DocumentEntryRow";
import RelatedDocumentsPane from "./RelatedDocumentsPane";

interface Props {
  canGoBack?: boolean;
  canGoForward?: boolean;
  onGoBack?: () => void;
  onGoForward?: () => void;
  onFileOpen?: (path: string) => void;
}

function headerTitle(path: string, metadata: DocumentMetadata | null) {
  const title = metadata?.title?.trim();
  return title && title.length > 0 ? title : fileName(path);
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

export default function PreviewPane({ canGoBack = false, canGoForward = false, onGoBack, onGoForward, onFileOpen }: Props) {
  const selectedMatch = useSearchStore((s) => s.selectedMatch);
  const previewData = useSearchStore((s) => s.previewData);
  const previewLoading = useSearchStore((s) => s.previewLoading);
  const viewerMetadata = useSearchStore((s) => s.viewerMetadata);
  const viewerMetadataStatus = useSearchStore((s) => s.viewerMetadataStatus);
  const clearPreview = useSearchStore((s) => s.clearPreview);
  const addBookmark = useBookmarksStore((s) => s.add);
  const bookmarks = useBookmarksStore((s) => s.bookmarks);
  const setChatActiveDoc = useChatStore((s) => s.setActiveDoc);
  const chatBackendsLoaded = useChatStore((s) => s.backendsLoaded);
  const hasAvailableChatBackend = useChatStore((s) => s.hasAvailableBackend);
  const openChatPaneAndSend = useChatStore((s) => s.openPaneAndSend);
  const { addToast } = useToasts();
  const [relatedPanelOpen, setRelatedPanelOpen] = useState(false);
  const [markdownView, setMarkdownView] = useState<"source" | "rendered">("rendered");

  useEffect(() => {
    if (!selectedMatch || "PdfPage" in selectedMatch.origin) return;
    setMarkdownView(readMarkdownViewMode(selectedMatch.path));
  }, [selectedMatch?.path]);

  const setRememberedMarkdownView = (view: "source" | "rendered") => {
    if (!selectedMatch) return;
    saveMarkdownViewMode(selectedMatch.path, view);
    setMarkdownView(view);
  };

  // The chat pane's "open document" badge (spec §6.1, §7.4): PreviewPane is
  // the single owner of "what's currently being viewed" since it's the only
  // component that knows both the open file *and* -- via PdfViewer's live
  // scroll tracking below -- the page actually on screen, not just the page
  // the user landed on.
  useEffect(() => {
    if (!isTauri) return;
    if (!selectedMatch) {
      setChatActiveDoc(null);
      return;
    }
    const page = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : null;
    setChatActiveDoc(selectedMatch.path, page);
  }, [selectedMatch, setChatActiveDoc]);

  const handleChatPageChange = (page: number) => {
    if (isTauri && selectedMatch) setChatActiveDoc(selectedMatch.path, page);
  };

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
  const isMarkdownFile =
    !isPdfFile && displayData != null && "Text" in displayData && displayData.Text.language === "markdown";
  const shouldRestoreSourceScroll =
    !isPdfFile &&
    "TextFile" in selectedMatch.origin &&
    selectedMatch.origin.TextFile.line === 0 &&
    selectedMatch.text_range == null;
  const pdfPage = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : 1;
  const pdfBbox = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.bbox : null;
  const bookmarkHighlights = bookmarks.flatMap((bookmark) => {
    if (bookmark.path !== selectedMatch.path || !("PdfPage" in bookmark.origin)) {
      return [];
    }
    const { page } = bookmark.origin.PdfPage;
    return bookmark.rects.length > 0
      ? [{ id: bookmark.id, page, rects: bookmark.rects }]
      : [];
  });
  const textBookmarkHighlights = bookmarks.flatMap((bookmark) =>
    bookmark.path === selectedMatch.path &&
    "TextFile" in bookmark.origin &&
    bookmark.text_range
      ? [{ id: bookmark.id, range: bookmark.text_range }]
      : [],
  );
  // Text locations stay in persisted UTF-8 document coordinates. Each renderer
  // translates only at its boundary (CodeMirror uses UTF-16; Markdown does not).
  const renderedHighlightRange =
    !isPdfFile && displayData && "Text" in displayData
      ? selectedMatch.text_range ?? displayData.Text.highlight_range
      : { start: 0, end: 0 };
  // When the navigation target is one of this file's bookmarks, emphasise its
  // exact per-line rects instead of the union bbox the search path uses.
  const bboxesEqual = (a: BoundingBox | null, b: BoundingBox | null) =>
    a != null &&
    b != null &&
    a.x === b.x &&
    a.y === b.y &&
    a.width === b.width &&
    a.height === b.height;
  const targetBookmarkRects =
    bookmarks.find(
      (bookmark) =>
        bookmark.path === selectedMatch.path &&
        bookmark.rects.length > 0 &&
        "PdfPage" in bookmark.origin &&
        bookmark.origin.PdfPage.page === pdfPage &&
        bboxesEqual(bookmark.origin.PdfPage.bbox, pdfBbox),
    )?.rects ?? null;
  const author = viewerMetadata?.author?.trim() || null;
  const createdAt = formatDocumentMonthYear(viewerMetadata?.created_at);
  const links = buildExternalLinks(viewerMetadata?.doi, viewerMetadata?.title);
  const doi = links?.doi ?? null;

  const handleOpenDoi = () => {
    if (!links?.doiUrl) return;
    api.openPath(links.doiUrl).catch((e) => console.error("Open DOI failed:", e));
  };

  const handleOpenScholar = () => {
    if (!links) return;
    api.openPath(links.googleScholarUrl).catch((e) => console.error("Open Google Scholar failed:", e));
  };

  const handleAddBookmark = (selection: DocumentSelection) => {
    if (!selectedMatch) return;
    addBookmark({
      path: selectedMatch.path,
      ...selection,
    })
      .then(() => addToast("Bookmark added", { type: "success" }))
      .catch((e) => console.error("Add bookmark failed:", e));
  };

  const chatSelectionActionsAvailable = isTauri && chatBackendsLoaded && hasAvailableChatBackend;

  const handleExplainSelection = (selection: DocumentSelection) => {
    openChatPaneAndSend(`Explain: ${selection.quote}`).catch((e) =>
      console.error("Explain selection failed:", e),
    );
  };

  const handleAskSelection = (selection: DocumentSelection, question: string) => {
    openChatPaneAndSend(`Question: ${question}\n\nSelected text:\n${selection.quote}`).catch((e) =>
      console.error("Ask about selection failed:", e),
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
    <div className="flex flex-col h-full min-h-0 overflow-hidden">
      {/* Header */}
      <div className="px-3 py-2 border-b border-[var(--border-main)] flex items-center gap-3 flex-shrink-0 bg-[var(--bg-header)]">
        <div className="flex items-center gap-1">
          <Tooltip content="Go back">
            <button
              onClick={onGoBack}
              disabled={!canGoBack}
              className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            >
              <ArrowLeft size={14} />
            </button>
          </Tooltip>
          <Tooltip content="Go forward">
            <button
              onClick={onGoForward}
              disabled={!canGoForward}
              className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            >
              <ArrowRight size={14} />
            </button>
          </Tooltip>
        </div>

        <div className="flex flex-col min-w-0 flex-1 selectable">
          <div className="flex items-center gap-1 min-w-0">
            <span className="text-xs font-medium text-[var(--text-main)] truncate leading-tight">
              {headerTitle(selectedMatch.path, viewerMetadata)}
            </span>
            <Tooltip content="Copy title">
              <CopyButton
                copy={() => api.writeClipboard(headerTitle(selectedMatch.path, viewerMetadata))}
                copiedChildren={<Check size={10} />}
                className="p-0.5 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] hover:text-[var(--text-main)] flex-shrink-0"
              >
                <Copy size={10} />
              </CopyButton>
            </Tooltip>
          </div>
          <div className="flex items-center gap-1 min-w-0 text-[10px] text-[var(--text-dim)] leading-tight">
            {createdAt && <span className={metadataBadgeClassName()}>{createdAt}</span>}
            {author && <span className="truncate">{author}</span>}
            {!createdAt && !author && viewerMetadataStatus === "loading" && <span>Loading metadata…</span>}
            {(createdAt || author || viewerMetadataStatus === "loading") && <span aria-hidden="true">·</span>}
            {doi && (
              <div className={groupedActionClassName()}>
                <Tooltip content={`Open DOI ${doi}`}>
                  <button
                    onClick={handleOpenDoi}
                    aria-label={`Open DOI ${doi}`}
                    className={groupedActionSegmentClassName()}
                  >
                    <span className="truncate max-w-[140px]">DOI: {doi}</span>
                    <ExternalLink size={10} />
                  </button>
                </Tooltip>
                <Tooltip content={`Copy DOI ${doi}`}>
                  <CopyButton
                    copy={() => api.writeClipboard(doi)}
                    aria-label={`Copy DOI ${doi}`}
                    copiedChildren={<Check size={10} />}
                    className={`${groupedActionSegmentClassName()} border-l border-[var(--border-main)]`}
                  >
                    <Copy size={10} />
                  </CopyButton>
                </Tooltip>
              </div>
            )}
            {links && (
              <>
                <Tooltip content="Open Google Scholar">
                  <button
                    onClick={handleOpenScholar}
                    aria-label="Open Google Scholar"
                    className={actionButtonClassName()}
                  >
                    <span>Scholar</span>
                    <ExternalLink size={10} />
                  </button>
                </Tooltip>
                <span aria-hidden="true">·</span>
              </>
            )}
            <Tooltip content="Copy path">
              <CopyButton
                copy={() => api.writeClipboard(selectedMatch.path)}
                aria-label="Copy path"
                copiedChildren={<><Check size={10} /><span>Copied</span></>}
                className={actionButtonClassName(true)}
              >
                <Copy size={10} />
                <span>Copy path</span>
              </CopyButton>
            </Tooltip>
          </div>
        </div>

        <Tooltip content={relatedPanelOpen ? "Hide related documents" : "Show related documents"}>
          <button
            onClick={() => setRelatedPanelOpen((open) => !open)}
            aria-label={relatedPanelOpen ? "Hide related documents" : "Show related documents"}
            className={`hidden p-1 rounded text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] md:inline-flex ${
              relatedPanelOpen ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""
            }`}
          >
            <Link2 size={16} />
          </button>
        </Tooltip>

        {isMarkdownFile && (
          <Tooltip content={markdownView === "rendered" ? "View Markdown source" : "View rendered Markdown"}>
            <button
              type="button"
              onClick={() => setRememberedMarkdownView(markdownView === "rendered" ? "source" : "rendered")}
              aria-label={markdownView === "rendered" ? "View Markdown source" : "View rendered Markdown"}
              className="inline-flex p-1 rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-header)] hover:text-[var(--text-main)]"
            >
              {markdownView === "rendered" ? <Code size={16} /> : <Eye size={16} />}
            </button>
          </Tooltip>
        )}

        <Tooltip content="Close preview">
          <button
            onClick={clearPreview}
            aria-label="Close preview"
            className="p-1 hover:bg-red-500/10 hover:text-red-500 rounded text-[var(--text-dim)] transition-colors"
          >
            <X size={16} />
          </button>
        </Tooltip>
      </div>

      {/* Content */}
      <div className="flex-1 min-h-0 overflow-hidden bg-[var(--bg-app)]">
        <div className="flex h-full min-h-0">
          <div className="relative min-w-0 flex-1 overflow-hidden">
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
                highlight_rects={targetBookmarkRects}
                bookmarkHighlights={bookmarkHighlights}
                onRenderSuccess={() => setIsPdfRendering(false)}
                onAddBookmark={handleAddBookmark}
                showChatSelectionActions={chatSelectionActionsAvailable}
                onExplainSelection={handleExplainSelection}
                onAskSelection={handleAskSelection}
                onPageChange={handleChatPageChange}
              />
            ) : isMarkdownFile && markdownView === "rendered" ? (
              <MarkdownViewer
                content={displayData.Text.content}
                documentPath={selectedMatch.path}
                restoreScrollPosition={shouldRestoreSourceScroll}
                highlightRange={renderedHighlightRange}
                bookmarkHighlights={textBookmarkHighlights}
                onAddBookmark={handleAddBookmark}
                showChatSelectionActions={chatSelectionActionsAvailable}
                onExplainSelection={handleExplainSelection}
                onAskSelection={handleAskSelection}
              />
            ) : displayData && "Text" in displayData ? (
              <CodeViewer
                content={displayData.Text.content}
                language={displayData.Text.language}
                documentPath={selectedMatch.path}
                restoreScrollPosition={shouldRestoreSourceScroll}
                highlightLine={displayData.Text.highlight_line}
                highlightRange={utf8ByteRangeToUtf16Range(
                  displayData.Text.content,
                  displayData.Text.highlight_range,
                )}
                bookmarkHighlights={textBookmarkHighlights}
                onAddBookmark={handleAddBookmark}
                showChatSelectionActions={chatSelectionActionsAvailable}
                onExplainSelection={handleExplainSelection}
                onAskSelection={handleAskSelection}
              />
            ) : null}
          </div>
          {relatedPanelOpen && (
            <RelatedDocumentsPane
              currentPath={selectedMatch.path}
              onOpenDocument={(path) => onFileOpen?.(path)}
              onClose={() => setRelatedPanelOpen(false)}
            />
          )}
        </div>
      </div>
    </div>
  );
}
