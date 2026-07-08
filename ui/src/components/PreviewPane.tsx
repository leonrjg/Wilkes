import { useEffect, useRef, useState } from "react";
import { X, ArrowLeft, ArrowRight, ExternalLink, Copy, Hash, Percent, Sidebar } from "react-feather";
import CodeViewer from "./preview/CodeViewer";
import PdfViewer from "./preview/PdfViewer";
import type { PdfSelection } from "./preview/PdfViewer";
import { useSearchStore } from "../stores/useSearchStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useChatStore } from "../stores/useChatStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { api, isTauri } from "../services";
import type { BoundingBox, DocumentMetadata, RelatedDocument } from "../lib/types";
import { buildExternalLinks } from "../lib/externalLinks";
import { formatDocumentMonthYear } from "../lib/dateFormatting";
import { useToasts } from "./Toast";
import { Tooltip } from "./Tooltip";
import { DocumentEntryRow, fileName, type DocumentDetail } from "./DocumentEntryRow";

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

type RelatedStatus = "idle" | "loading" | "ready" | "empty" | "error" | "unavailable";

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
  const directory = useSettingsStore((s) => s.directory);
  const relatedIndexReady = useSemanticStore((s) => s.readyForCurrentRoot);
  const indexStatus = useSemanticStore((s) => s.indexStatus);
  const { addToast } = useToasts();
  const relatedCacheRef = useRef<Map<string, RelatedDocument[]>>(new Map());
  const [relatedStatus, setRelatedStatus] = useState<RelatedStatus>("idle");
  const [relatedDocuments, setRelatedDocuments] = useState<RelatedDocument[]>([]);
  const [relatedPanelOpen, setRelatedPanelOpen] = useState(true);

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

  useEffect(() => {
    if (!relatedPanelOpen) return;
    if (!selectedMatch || !directory) {
      setRelatedStatus("idle");
      setRelatedDocuments([]);
      return;
    }
    if (!relatedIndexReady || !indexStatus) {
      setRelatedStatus("unavailable");
      setRelatedDocuments([]);
      return;
    }

    const indexKey = `${indexStatus.model_id}:${indexStatus.built_at ?? "unknown"}`;
    const cacheKey = `${directory}\n${selectedMatch.path}\n${indexKey}`;
    const cached = relatedCacheRef.current.get(cacheKey);
    if (cached) {
      setRelatedDocuments(cached);
      setRelatedStatus(cached.length > 0 ? "ready" : "empty");
      return;
    }

    let cancelled = false;
    setRelatedStatus("loading");
    setRelatedDocuments([]);
    api
      .relatedDocuments({ root: directory, path: selectedMatch.path, limit: 8 })
      .then((docs) => {
        if (cancelled) return;
        relatedCacheRef.current.set(cacheKey, docs);
        setRelatedDocuments(docs);
        setRelatedStatus(docs.length > 0 ? "ready" : "empty");
      })
      .catch((e) => {
        if (cancelled) return;
        console.debug("Related documents unavailable:", e);
        setRelatedDocuments([]);
        setRelatedStatus("error");
      });

    return () => {
      cancelled = true;
    };
  }, [
    selectedMatch?.path,
    directory,
    relatedIndexReady,
    indexStatus?.model_id,
    indexStatus?.built_at,
    relatedPanelOpen,
  ]);

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
  const bookmarkHighlights = bookmarks.flatMap((bookmark) => {
    if (bookmark.path !== selectedMatch.path || !("PdfPage" in bookmark.origin)) {
      return [];
    }
    const { page } = bookmark.origin.PdfPage;
    return bookmark.rects.length > 0
      ? [{ id: bookmark.id, page, rects: bookmark.rects }]
      : [];
  });
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

  const handleCopyDoi = () => {
    if (!doi) return;
    api.writeClipboard(doi).catch((e) => console.error("Copy DOI failed:", e));
  };

  const handleCopyTitle = () => {
    const title = headerTitle(selectedMatch.path, viewerMetadata);
    api.writeClipboard(title).catch((e) => console.error("Copy title failed:", e));
  };

  const handleAddBookmark = ({
    page,
    bbox,
    rects,
    quote,
  }: {
    page: number;
    bbox: BoundingBox;
    rects: BoundingBox[];
    quote: string;
  }) => {
    if (!selectedMatch) return;
    addBookmark({
      path: selectedMatch.path,
      origin: { PdfPage: { page, bbox } },
      quote,
      rects,
    })
      .then(() => addToast("Bookmark added", { type: "success" }))
      .catch((e) => console.error("Add bookmark failed:", e));
  };

  const chatSelectionActionsAvailable = isTauri && chatBackendsLoaded && hasAvailableChatBackend;

  const handleExplainSelection = (selection: PdfSelection) => {
    openChatPaneAndSend(`Explain: ${selection.quote}`).catch((e) =>
      console.error("Explain selection failed:", e),
    );
  };

  const handleAskSelection = (selection: PdfSelection, question: string) => {
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
              <button
                onClick={handleCopyTitle}
                className="p-0.5 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] hover:text-[var(--text-main)] flex-shrink-0"
              >
                <Copy size={10} />
              </button>
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
                    title={`Open DOI ${doi}`}
                    className={groupedActionSegmentClassName()}
                  >
                    <span className="truncate max-w-[140px]">DOI: {doi}</span>
                    <ExternalLink size={10} />
                  </button>
                </Tooltip>
                <Tooltip content={`Copy DOI ${doi}`}>
                  <button
                    onClick={handleCopyDoi}
                    aria-label={`Copy DOI ${doi}`}
                    title={`Copy DOI ${doi}`}
                    className={`${groupedActionSegmentClassName()} border-l border-[var(--border-main)]`}
                  >
                    <Copy size={10} />
                  </button>
                </Tooltip>
              </div>
            )}
            {links && (
              <>
                <Tooltip content="Open Google Scholar">
                  <button
                    onClick={handleOpenScholar}
                    aria-label="Open Google Scholar"
                    title="Open Google Scholar"
                    className={actionButtonClassName()}
                  >
                    <span>Scholar</span>
                    <ExternalLink size={10} />
                  </button>
                </Tooltip>
                <span aria-hidden="true">·</span>
              </>
            )}
            <span className="truncate min-w-0 flex-1">{selectedMatch.path}</span>
          </div>
        </div>

        <Tooltip content={relatedPanelOpen ? "Hide related documents" : "Show related documents"}>
          <button
            onClick={() => setRelatedPanelOpen((open) => !open)}
            aria-label={relatedPanelOpen ? "Hide related documents" : "Show related documents"}
            title={relatedPanelOpen ? "Hide related documents" : "Show related documents"}
            className={`hidden p-1 rounded text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] md:inline-flex ${
              relatedPanelOpen ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""
            }`}
          >
            <Sidebar size={16} />
          </button>
        </Tooltip>

        <Tooltip content="Close preview">
          <button
            onClick={clearPreview}
            aria-label="Close preview"
            title="Close preview"
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
            ) : displayData && "Text" in displayData ? (
              <CodeViewer
                content={displayData.Text.content}
                language={displayData.Text.language}
                highlightLine={displayData.Text.highlight_line}
                highlightRange={displayData.Text.highlight_range}
              />
            ) : null}
          </div>
          {relatedPanelOpen && (
            <RelatedDocumentsPanel
              status={relatedStatus}
              documents={relatedDocuments}
              onOpen={(path) => onFileOpen?.(path)}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function scoreLabel(score: number) {
  return `${Math.max(0, Math.min(100, Math.round(score * 100)))}%`;
}

function relatedDocumentDetails(doc: RelatedDocument): DocumentDetail[] {
  return [
    {
      key: "score",
      label: "Score",
      value: scoreLabel(doc.score),
      valueTitle: `${doc.score.toFixed(3)} cosine similarity`,
      icon: Percent,
      monospace: true,
    },
    {
      key: "indexed-chunks",
      label: "Indexed chunks",
      value: doc.indexed_chunks.toLocaleString(),
      icon: Hash,
      monospace: true,
    },
  ];
}

function RelatedDocumentsPanel({
  status,
  documents,
  onOpen,
}: {
  status: RelatedStatus;
  documents: RelatedDocument[];
  onOpen: (path: string) => void;
}) {
  return (
    <aside className="hidden w-64 flex-shrink-0 border-l border-[var(--border-main)] bg-[var(--bg-sidebar)] md:flex md:flex-col">
      <div className="border-b border-[var(--border-main)] px-3 py-2 text-xs font-medium text-[var(--text-main)]">
        Related
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {status === "loading" && (
          <div className="px-3 py-3 text-xs text-[var(--text-muted)]">Loading…</div>
        )}
        {status === "unavailable" && (
          <div className="px-3 py-3 text-xs text-[var(--text-dim)]">Semantic index unavailable</div>
        )}
        {status === "error" && (
          <div className="px-3 py-3 text-xs text-red-500">Related documents unavailable</div>
        )}
        {status === "empty" && (
          <div className="px-3 py-3 text-xs text-[var(--text-dim)]">No related documents</div>
        )}
        {documents.map((doc) => (
          <DocumentEntryRow
            key={doc.path}
            entry={doc}
            details={relatedDocumentDetails(doc)}
            onClick={() => onOpen(doc.path)}
          />
        ))}
      </div>
    </aside>
  );
}
