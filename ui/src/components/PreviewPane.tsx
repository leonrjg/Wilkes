import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, ArrowRight, ExternalLink, Check, Copy, Link2, Code, Eye, FileText, Cloud, Share2, Edit3 } from "react-feather";
import CodeViewer from "./preview/CodeViewer";
import DocumentEditor from "./DocumentEditor";
import MarkdownViewer from "./preview/MarkdownViewer";
import PdfViewer from "./preview/PdfViewer";
import type { DocumentSelection } from "./preview/SelectionActions";
import { utf8ByteRangeToUtf16Range } from "./preview/textOffsets";
import { readMarkdownViewMode, saveMarkdownViewMode } from "./preview/textScrollMemory";
import { activeViewerTab, useViewerStore } from "../stores/useViewerStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useChatStore } from "../stores/useChatStore";
import { api, isTauri } from "../services";
import type { BoundingBox, DocumentMetadata, Match, MatchRef } from "../lib/types";
import { buildExternalLinks } from "../lib/externalLinks";
import { formatDocumentMonthYear } from "../lib/dateFormatting";
import { useToasts } from "./Toast";
import { Tooltip } from "./Tooltip";
import { CopyButton } from "./CopyButton";
import RelatedDocumentsPane from "./RelatedDocumentsPane";
import BookmarkDetails from "./preview/BookmarkDetails";
import type { Decoration, ElementAnchor } from "./preview/decorations";
import SelectionActions from "./preview/SelectionActions";
import { ReaderHostProvider, type ReaderHostServices } from "./preview/ReaderHost";
import type { SelectionActionsSlot } from "./preview/slots";
import ViewerTabs from "./ViewerTabs";
import DocumentSummaryPane from "./DocumentSummaryPane";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import DocumentTopicCloudPane from "./DocumentTopicCloudPane";
import CitationGraphPane from "./CitationGraphPane";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSearchStore } from "../stores/useSearchStore";

type ViewerSidePanel = "related" | "citations" | "summary" | "topics" | null;

function headerTitle(metadata: DocumentMetadata | null) {
  const title = metadata?.title?.trim();
  return title && title.length > 0 ? title : null;
}

function headerAuthor(metadata: DocumentMetadata | null) {
  const author = metadata?.author?.trim();
  if (!author) return null;

  const characters = Array.from(author);
  return characters.length <= 30 ? author : `${characters.slice(0, 29).join("")}…`;
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

function isReferencedPdfMatch(candidate: Match, selected: MatchRef): boolean {
  if (!("PdfPage" in candidate.origin) || !("PdfPage" in selected.origin)) {
    return false;
  }
  const candidateRange = candidate.text_range;
  const selectedRange = selected.text_range;
  const sameRange =
    (candidateRange == null && selectedRange == null) ||
    (candidateRange != null &&
      selectedRange != null &&
      candidateRange.start === selectedRange.start &&
      candidateRange.end === selectedRange.end);
  if (!sameRange || candidate.origin.PdfPage.page !== selected.origin.PdfPage.page) {
    return false;
  }
  const candidateBox = candidate.origin.PdfPage.bbox;
  const selectedBox = selected.origin.PdfPage.bbox;
  return (
    candidateBox === selectedBox ||
    (candidateBox != null &&
      selectedBox != null &&
      candidateBox.x === selectedBox.x &&
      candidateBox.y === selectedBox.y &&
      candidateBox.width === selectedBox.width &&
      candidateBox.height === selectedBox.height)
  );
}

export default function PreviewPane() {
  const activeTab = useViewerStore(activeViewerTab);
  const goBack = useViewerStore((state) => state.goBack);
  const goForward = useViewerStore((state) => state.goForward);
  const openFile = useViewerStore((state) => state.openFile);
  const selectedMatch = activeTab?.match ?? null;
  const previewData = activeTab?.previewData ?? null;
  const previewLoading = activeTab?.previewLoading ?? false;
  const previewError = activeTab?.previewError ?? null;
  const pdfLoadAttempt = activeTab?.pdfLoadAttempt ?? 0;
  const viewerMetadata = activeTab?.metadata ?? null;
  const viewerMetadataStatus = activeTab?.metadataStatus ?? "idle";
  const canGoBack = activeTab != null && activeTab.historyIndex > 0;
  const canGoForward =
    activeTab != null && activeTab.historyIndex < activeTab.history.length - 1;
  const retryTab = useViewerStore((state) => state.retryTab);
  const reportTabLoadError = useViewerStore((state) => state.reportTabLoadError);
  const addBookmark = useBookmarksStore((s) => s.add);
  const removeBookmark = useBookmarksStore((s) => s.remove);
  const bookmarks = useBookmarksStore((s) => s.bookmarks);
  const setChatActiveDoc = useChatStore((s) => s.setActiveDoc);
  const chatBackendsLoaded = useChatStore((s) => s.backendsLoaded);
  const hasAvailableChatBackend = useChatStore((s) => s.hasAvailableBackend);
  const openChatPaneAndSend = useChatStore((s) => s.openPaneAndSend);
  const generationReady = useGenerationStore((state) => state.ready);
  const semanticReady = useSemanticStore((state) => state.readyForCurrentRoot);
  const searchResults = useSearchStore((state) => state.results);
  const listedDoi = useSettingsStore((state) =>
    selectedMatch
      ? state.fileList.find((entry) => entry.path === selectedMatch.path)?.doi ?? null
      : null,
  );
  const { addToast } = useToasts();
  const pdfAutoZoomTargetPx = useSettingsStore(
    (state) => state.settings?.pdf_auto_zoom_target_px,
  );
  // Everything the readers need from this application. They take it from here
  // rather than importing the Tauri bridge and the settings store directly, so
  // the same components can be mounted by a host that has neither.
  const readerHost = useMemo<ReaderHostServices>(
    () => ({
      openExternal: (url) =>
        api.openPath(url).catch((e) => console.error("Open link failed:", e)),
      pdfAutoZoomTargetPx,
    }),
    [pdfAutoZoomTargetPx],
  );
  const [sidePanel, setSidePanel] = useState<ViewerSidePanel>(null);
  const [markdownView, setMarkdownView] = useState<"source" | "rendered">("rendered");
  const [editing, setEditing] = useState(false);
  const [openBookmarkTarget, setOpenBookmarkTarget] = useState<{
    id: string;
    anchor: ElementAnchor;
  } | null>(null);
  const [deletingBookmark, setDeletingBookmark] = useState(false);
  const openBookmark = bookmarks.find(
    (bookmark) => bookmark.id === openBookmarkTarget?.id,
  ) ?? null;

  useEffect(() => {
    setOpenBookmarkTarget(null);
    setDeletingBookmark(false);
    setEditing(false);
  }, [selectedMatch?.path]);

  useEffect(() => {
    if (!generationReady && sidePanel === "summary") setSidePanel(null);
  }, [generationReady, sidePanel]);

  useEffect(() => {
    if (!semanticReady && sidePanel === "topics") setSidePanel(null);
  }, [semanticReady, sidePanel]);

  const currentDoi = viewerMetadata?.doi?.trim() || listedDoi?.trim() || "";

  useEffect(() => {
    if (!currentDoi && sidePanel === "citations") setSidePanel(null);
  }, [currentDoi, sidePanel]);

  useEffect(() => {
    if (openBookmarkTarget && !openBookmark) setOpenBookmarkTarget(null);
  }, [openBookmark, openBookmarkTarget]);

  const handleOpenBookmark = (id: string, anchor: ElementAnchor) => {
    setOpenBookmarkTarget({ id, anchor });
  };

  useEffect(() => {
    if (!selectedMatch || "PdfPage" in selectedMatch.origin) return;
    setMarkdownView(readMarkdownViewMode(selectedMatch.path));
  }, [selectedMatch?.path]);

  const setRememberedMarkdownView = (view: "source" | "rendered") => {
    if (!selectedMatch) return;
    saveMarkdownViewMode(selectedMatch.path, view);
    setMarkdownView(view);
  };

  // PreviewPane is the single owner of "what's currently being viewed" since
  // it's the only component that knows both the open file and -- via
  // PdfViewer's live scroll tracking below -- the page actually on screen.
  // Publish that application state directly for external MCP, then separately
  // mirror it into the private chat context when the desktop chat is present.
  useEffect(() => {
    if (!selectedMatch) {
      api.setActiveDocument?.(null).catch((error) =>
        console.error("mcp: failed to clear active document", error),
      );
      if (isTauri) setChatActiveDoc(null);
      return;
    }
    const page = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : null;
    api.setActiveDocument?.(selectedMatch.path, page).catch((error) =>
      console.error("mcp: failed to update active document", error),
    );
    if (isTauri) setChatActiveDoc(selectedMatch.path, page);
  }, [selectedMatch, setChatActiveDoc]);

  const handleChatPageChange = (page: number) => {
    if (selectedMatch) {
      api.setActiveDocument?.(selectedMatch.path, page).catch((error) =>
        console.error("mcp: failed to update active document page", error),
      );
    }
    if (isTauri && selectedMatch) setChatActiveDoc(selectedMatch.path, page);
  };

  const [isPdfRendering, setIsPdfRendering] = useState(false);
  const prevPdfUrlRef = useRef<string | null>(null);
  const displayData = previewData;

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

  if (!activeTab || !selectedMatch) {
    return (
      <div className="flex h-full flex-col bg-[var(--bg-app)] text-[var(--text-dim)]">
        <ViewerTabs />
        <div className="flex flex-1 flex-col items-center justify-center">
          <img
            src="/logo.transparent.png"
            alt="Wilkes"
            className="mb-8 h-auto w-[clamp(10rem,20vw,18rem)] max-w-[80vw] opacity-25 -translate-x-2"
          />
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
  const selectedSearchMatch = isPdfFile
    ? searchResults
        .find((file) => file.path === selectedMatch.path)
        ?.matches.find((match) => isReferencedPdfMatch(match, selectedMatch))
    : undefined;
  // Semantic results intentionally keep their indexed chunk highlight. Their
  // matched text is the whole chunk, whereas exact results carry the bounded
  // raw quote that can be localized reliably against nearby PDF.js pages.
  const pdfSearchLocator =
    selectedSearchMatch && selectedSearchMatch.score == null
      ? {
          matched_text: selectedSearchMatch.matched_text,
          context_before: selectedSearchMatch.context_before,
          context_after: selectedSearchMatch.context_after,
        }
      : null;
  // One list for every reader. A bookmark is anchored by whichever coordinate
  // system its origin already carries -- page rects for a PDF, a byte range for
  // a text file -- and each reader keeps the anchors it can place. `pdf-`/`cm-`/
  // `markdown-` prefixed classes are this application's palette, not the
  // reader's: the readers no longer have a notion of a bookmark to style.
  const bookmarkDecorations: Decoration[] = bookmarks.flatMap((bookmark): Decoration[] => {
    if (bookmark.path !== selectedMatch.path) return [];
    const shared = {
      id: bookmark.id,
      onActivate: handleOpenBookmark,
      ariaLabel: "Open bookmark",
    };
    if ("PdfPage" in bookmark.origin) {
      return bookmark.rects.length > 0
        ? [{
            ...shared,
            anchor: {
              kind: "rects" as const,
              page: bookmark.origin.PdfPage.page,
              rects: bookmark.rects,
            },
            className: "pdf-highlight--bookmark",
          }]
        : [];
    }
    if ("TextFile" in bookmark.origin && bookmark.text_range) {
      return [{
        ...shared,
        anchor: { kind: "range" as const, range: bookmark.text_range },
        className: isMarkdownFile && markdownView === "rendered"
          ? "markdown-bookmark-highlight"
          : "cm-bookmark-highlight",
      }];
    }
    return [];
  });

  // Wilkes' selection chrome, handed to whichever reader is mounted. The reader
  // decides where it appears; this decides what it offers.
  const selectionActionsSlot: SelectionActionsSlot = (selection, api) => (
    <SelectionActions
      selection={selection}
      api={api}
      onAddBookmark={handleAddBookmark}
      showChatActions={chatSelectionActionsAvailable}
      onExplain={handleExplainSelection}
      onAsk={handleAskSelection}
    />
  );
  // Text locations stay in persisted UTF-8 document coordinates. Each renderer
  // translates only at its boundary (CodeMirror uses UTF-16; Markdown does not).
  const renderedHighlightRange =
    !isPdfFile && displayData && "Text" in displayData
      ? selectedMatch.text_range ?? displayData.Text.highlight_range
      : { start: 0, end: 0 };
  // When the navigation target is one of this file's bookmarks, emphasise its
  // exact persisted per-line rects instead of its union bbox.
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
  const title = headerTitle(viewerMetadata);
  const author = headerAuthor(viewerMetadata);
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

  const handleDeleteBookmark = async () => {
    if (!openBookmark || deletingBookmark) return;
    setDeletingBookmark(true);
    try {
      await removeBookmark(openBookmark.id);
      setOpenBookmarkTarget(null);
      addToast("Bookmark deleted", { type: "success" });
    } catch (error) {
      console.error("Delete bookmark failed:", error);
      addToast("Failed to delete bookmark", { type: "error" });
    } finally {
      setDeletingBookmark(false);
    }
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

  return (
    <div className="flex flex-col h-full min-h-0 overflow-hidden">
      <ViewerTabs />
      {/* Header */}
      <div className="px-3 py-2 border-b border-[var(--border-main)] flex items-center gap-3 flex-shrink-0 bg-[var(--bg-header)]">
        <div className="flex items-center gap-1">
          <Tooltip content="Go back">
            <button
              onClick={goBack}
              disabled={!canGoBack}
              className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            >
              <ArrowLeft size={14} />
            </button>
          </Tooltip>
          <Tooltip content="Go forward">
            <button
              onClick={goForward}
              disabled={!canGoForward}
              className="p-1 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] disabled:opacity-30"
            >
              <ArrowRight size={14} />
            </button>
          </Tooltip>
        </div>

        <div className="flex flex-col min-w-0 flex-1 selectable">
          {title && (
            <div className="flex items-center gap-1 min-w-0">
              <span className="text-xs font-medium text-[var(--text-main)] truncate leading-tight">
                {title}
              </span>
              <Tooltip content="Copy title">
                <CopyButton
                  copy={() => api.writeClipboard(title)}
                  copiedChildren={<Check size={10} />}
                  className="p-0.5 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] hover:text-[var(--text-main)] flex-shrink-0"
                >
                  <Copy size={10} />
                </CopyButton>
              </Tooltip>
            </div>
          )}
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
                <span>Path</span>
              </CopyButton>
            </Tooltip>
          </div>
        </div>

        {generationReady && (
          <Tooltip content={sidePanel === "summary" ? "Hide summary" : "Summarize document"}>
            <button
              onClick={() =>
                setSidePanel((current) => (current === "summary" ? null : "summary"))
              }
              aria-label={sidePanel === "summary" ? "Hide summary" : "Summarize document"}
              className={`hidden p-1 rounded text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] md:inline-flex ${
                sidePanel === "summary"
                  ? "bg-[var(--bg-active)] text-[var(--text-main)]"
                  : ""
              }`}
            >
              <FileText size={16} />
            </button>
          </Tooltip>
        )}

        <Tooltip content={sidePanel === "related" ? "Hide related documents" : "Show related documents"}>
          <button
            onClick={() =>
              setSidePanel((current) => (current === "related" ? null : "related"))
            }
            aria-label={sidePanel === "related" ? "Hide related documents" : "Show related documents"}
            className={`hidden p-1 rounded text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] md:inline-flex ${
              sidePanel === "related" ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""
            }`}
          >
            <Link2 size={16} />
          </button>
        </Tooltip>

        {currentDoi && (
          <Tooltip content={sidePanel === "citations" ? "Hide citation graph" : "Show citation graph"}>
            <button
              type="button"
              onClick={() =>
                setSidePanel((current) => (current === "citations" ? null : "citations"))
              }
              aria-label={sidePanel === "citations" ? "Hide citation graph" : "Show citation graph"}
              className={`hidden rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] md:inline-flex ${
                sidePanel === "citations" ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""
              }`}
            >
              <Share2 size={16} />
            </button>
          </Tooltip>
        )}

        <Tooltip
          content={
            semanticReady
              ? sidePanel === "topics"
                ? "Hide document topics"
                : "Show document topics"
              : "Build the semantic index to view document topics"
          }
        >
          <button
            type="button"
            disabled={!semanticReady}
            onClick={() =>
              setSidePanel((current) =>
                current === "topics" ? null : "topics",
              )
            }
            aria-label={
              sidePanel === "topics"
                ? "Hide document topics"
                : "Show document topics"
            }
            className={`hidden rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-40 md:inline-flex ${
              sidePanel === "topics"
                ? "bg-[var(--bg-active)] text-[var(--text-main)]"
                : ""
            }`}
          >
            <Cloud size={16} />
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

        {!isPdfFile && displayData && "Text" in displayData && (
          <Tooltip content={editing ? "Return to document viewer" : "Edit document"}>
            <button
              type="button"
              onClick={() => {
                setEditing((current) => !current);
                if (!editing) setRememberedMarkdownView("source");
              }}
              aria-label={editing ? "Finish editing document" : "Edit document"}
              className={`inline-flex rounded border border-[var(--border-main)] p-1 text-[var(--text-dim)] ${editing ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""}`}
            >
              <Edit3 size={16} />
            </button>
          </Tooltip>
        )}

      </div>

      {/* Content */}
      <div
        id="viewer-tabpanel"
        role="tabpanel"
        aria-labelledby={`viewer-tab-${activeTab.id}`}
        className="flex-1 min-h-0 overflow-hidden bg-[var(--bg-app)]"
      >
        <div className="flex h-full min-h-0">
          <ReaderHostProvider value={readerHost}>
          <div className="relative min-w-0 flex-1 overflow-hidden">
            {openBookmark && openBookmarkTarget && (
              <BookmarkDetails
                bookmark={openBookmark}
                anchor={openBookmarkTarget.anchor}
                deleting={deletingBookmark}
                onClose={() => setOpenBookmarkTarget(null)}
                onDelete={() => void handleDeleteBookmark()}
              />
            )}
            {(previewLoading || isPdfRendering) && (
              <div className="absolute inset-0 flex items-center justify-center bg-[var(--bg-app)] z-30 pointer-events-none">
                <div className="flex flex-col items-center gap-3">
                  <div className="w-6 h-6 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
                  <span className="text-[var(--text-muted)] text-sm animate-pulse">Loading document…</span>
                </div>
              </div>
            )}
            {previewError && !previewLoading && (
              <div className="absolute inset-0 z-30 flex items-center justify-center bg-[var(--bg-app)] px-6">
                <div className="flex max-w-md flex-col items-center gap-3 text-center">
                  <FileText size={28} className="text-[var(--text-dim)]" />
                  <div>
                    <p className="text-sm font-medium text-[var(--text-main)]">
                      Could not load this document
                    </p>
                    <p className="mt-1 break-words text-xs text-[var(--text-muted)]">
                      {previewError}
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={() => retryTab(activeTab.id)}
                    className="rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-1.5 text-xs text-[var(--text-main)] hover:border-[var(--border-strong)]"
                  >
                    Retry
                  </button>
                </div>
              </div>
            )}
            {isPdfFile ? (
              <PdfViewer
                key={api.resolvePdfUrl(selectedMatch.path)}
                url={api.resolvePdfUrl(selectedMatch.path)}
                loadAttempt={pdfLoadAttempt}
                page={pdfPage}
                highlight_bbox={pdfBbox}
                highlight_rects={targetBookmarkRects}
                search_locator={pdfSearchLocator}
                decorations={bookmarkDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
                onRenderSuccess={() => setIsPdfRendering(false)}
                onLoadError={(error) => reportTabLoadError(activeTab.id, error)}
                onPageChange={handleChatPageChange}
              />
            ) : isMarkdownFile && markdownView === "rendered" ? (
              <MarkdownViewer
                content={displayData.Text.content}
                documentPath={selectedMatch.path}
                restoreScrollPosition={shouldRestoreSourceScroll}
                highlightRange={renderedHighlightRange}
                decorations={bookmarkDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
              />
            ) : displayData && "Text" in displayData && editing ? (
              <DocumentEditor
                content={displayData.Text.content}
                language={displayData.Text.language}
                documentPath={selectedMatch.path}
                semanticReady={semanticReady}
                generationReady={generationReady}
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
                decorations={bookmarkDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
              />
            ) : null}
          </div>
          </ReaderHostProvider>
          {sidePanel === "summary" && generationReady && (
            <DocumentSummaryPane
              path={selectedMatch.path}
              onClose={() => setSidePanel(null)}
            />
          )}
          {sidePanel === "related" && (
            <RelatedDocumentsPane
              currentPath={selectedMatch.path}
              onOpenDocument={openFile}
              onClose={() => setSidePanel(null)}
            />
          )}
          {sidePanel === "citations" && currentDoi && (
            <CitationGraphPane
              currentPath={selectedMatch.path}
              doi={currentDoi}
              onOpenDocument={openFile}
              onClose={() => setSidePanel(null)}
            />
          )}
          {sidePanel === "topics" && semanticReady && (
            <DocumentTopicCloudPane
              currentPath={selectedMatch.path}
              onClose={() => setSidePanel(null)}
            />
          )}
        </div>
      </div>
    </div>
  );
}
