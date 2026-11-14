import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { isPagedPath, isPdfPath, servesPagesOnDemand } from "../lib/documentFormats";
import { readerLinks, readerOutline } from "../lib/surrogate";
import { ArrowLeft, ArrowRight, ExternalLink, Check, Copy, Link2, Code, Eye, FileText, Cloud, Share2, Edit3, Save, RotateCcw } from "react-feather";
import DocumentEditor from "./DocumentEditor";
// Everything this pane takes from the readers comes through their public
// surface -- the same entry point Underdog will import. Reaching past it into
// a reader's internals is how a shared package stops being shareable.
import {
  CodeViewer,
  HtmlViewer,
  MarkdownViewer,
  PdfViewer,
  ImagePageViewer,
  type ImagePageSource,
  ReaderHostProvider,
  utf8ByteRangeToUtf16Range,
  type Decoration,
  type DocumentSelection,
  type ElementAnchor,
  type PdfFontRequest,
  type PdfReaderHandle,
  type ReaderHostServices,
  type SelectionActionsSlot,
} from "@leonrjg/wilkes-reader";
import { readTextViewMode, readerPdfEditStore, readerPositionStore, saveTextViewMode } from "../stores/readerPositions";
import { confirmDialog } from "../lib/utils/dialog";
import { paneActiveTab, useViewerStore } from "../stores/useViewerStore";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useChatStore } from "../stores/useChatStore";
import { api, isTauri } from "../services";
import type { BoundingBox, DocumentMetadata, Match, MatchRef } from "../lib/types";
import { buildExternalLinks } from "../lib/externalLinks";
import { formatDocumentMonthYear } from "../lib/dateFormatting";
import { useToasts } from "./Toast";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { CopyButton } from "./CopyButton";
import RelatedDocumentsPane from "./RelatedDocumentsPane";
import BookmarkDetails from "./BookmarkDetails";
import SelectionActions from "./SelectionActions";
import ViewerTabs from "./ViewerTabs";
import DocumentSummaryPane from "./DocumentSummaryPane";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import DocumentTopicCloudPane from "./DocumentTopicCloudPane";
import CitationGraphPane from "./CitationGraphPane";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSearchStore } from "../stores/useSearchStore";
import { openLink } from "../lib/openLink";
import { fileNameOf } from "../lib/editedCopy";

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

interface PreviewPaneProps {
  standalone?: boolean;
}

/** Narrowest a pane may be dragged, in pixels. */
const MIN_PANE_WIDTH = 160;
/** A keyboard step of the resize handle, as a share of the two panes it sits between. */
const KEY_RESIZE_STEP = 0.02;

/** The document viewer: its panes of tabs side by side, as many as the tabs
 *  have been split into. Each pane is keyed by its id, so a pane keeps the
 *  document it has mounted while other panes open and close beside it. */
export default function PreviewPane({ standalone = false }: PreviewPaneProps) {
  const panes = useViewerStore((state) => state.panes);
  const focusPane = useViewerStore((state) => state.focusPane);
  // Each pane's share of the width, relative to the others. Only a handle
  // between two panes changes them, and only theirs.
  const [weights, setWeights] = useState<Record<string, number>>({});
  const paneElements = useRef(new Map<string, HTMLDivElement>());
  const weightOf = (id: string) => weights[id] ?? 1;

  const resizePair = (left: string, right: string, leftShare: number) => {
    const total = weightOf(left) + weightOf(right);
    setWeights((current) => ({ ...current, [left]: total * leftShare, [right]: total * (1 - leftShare) }));
  };

  const startResize = (left: string, right: string) => (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const leftElement = paneElements.current.get(left);
    const rightElement = paneElements.current.get(right);
    if (!leftElement || !rightElement) return;
    event.preventDefault();
    const handle = event.currentTarget;
    const pointerId = event.pointerId;
    const start = leftElement.getBoundingClientRect().left;
    const span = leftElement.getBoundingClientRect().width + rightElement.getBoundingClientRect().width;
    handle.setPointerCapture(pointerId);
    // A docked browser pane would take the pointer as the drag crossed it.
    useBrowserStore.getState().setHeld(true);
    document.body.style.cursor = "col-resize";
    const move = (moveEvent: PointerEvent) => {
      if (moveEvent.pointerId !== pointerId || span <= 0) return;
      const leftWidth = Math.min(span - MIN_PANE_WIDTH, Math.max(MIN_PANE_WIDTH, moveEvent.clientX - start));
      resizePair(left, right, leftWidth / span);
    };
    const finish = (endEvent: PointerEvent) => {
      if (endEvent.pointerId !== pointerId) return;
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      useBrowserStore.getState().setHeld(false);
      document.body.style.cursor = "";
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  };

  const resizeWithKeys = (left: string, right: string) => (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const share = weightOf(left) / (weightOf(left) + weightOf(right));
    const step = event.shiftKey ? KEY_RESIZE_STEP * 5 : KEY_RESIZE_STEP;
    if (event.key === "ArrowLeft") resizePair(left, right, Math.max(0.1, share - step));
    else if (event.key === "ArrowRight") resizePair(left, right, Math.min(0.9, share + step));
    else return;
    event.preventDefault();
  };

  if (panes.length === 0) return <ViewerPaneView standalone={standalone} paneId={null} />;

  return (
    <div className="flex h-full min-h-0 min-w-0">
      {panes.map((pane, index) => {
        const next = panes[index + 1];
        const share = next ? weightOf(pane.id) / (weightOf(pane.id) + weightOf(next.id)) : 0;
        return (
          <Fragment key={pane.id}>
            <div
              ref={(element) => {
                if (element) paneElements.current.set(pane.id, element);
                else paneElements.current.delete(pane.id);
              }}
              // Whatever is clicked or tabbed into in a pane makes it the
              // focused one, before the click itself is handled: back and
              // forward act on the focused pane's tab.
              onPointerDownCapture={() => focusPane(pane.id)}
              onFocusCapture={() => focusPane(pane.id)}
              className="h-full min-h-0 min-w-0"
              style={{ flex: `${weightOf(pane.id)} 1 0%` }}
            >
              <ViewerPaneView standalone={standalone} paneId={pane.id} />
            </div>
            {next && (
              <div
                role="separator"
                aria-orientation="vertical"
                aria-label={`Resize panes ${index + 1} and ${index + 2}`}
                aria-valuemin={10}
                aria-valuemax={90}
                aria-valuenow={Math.round(share * 100)}
                tabIndex={0}
                onPointerDown={startResize(pane.id, next.id)}
                onKeyDown={resizeWithKeys(pane.id, next.id)}
                onDoubleClick={() => resizePair(pane.id, next.id, 0.5)}
                className="w-1 flex-shrink-0 cursor-col-resize bg-[var(--border-main)] transition-colors hover:bg-[var(--accent-blue)] focus-visible:bg-[var(--accent-blue)] focus-visible:outline-none"
              />
            )}
          </Fragment>
        );
      })}
    </div>
  );
}

interface ViewerPaneViewProps {
  standalone: boolean;
  /** Null when no document is open, and so there are no panes. */
  paneId: string | null;
}

function ViewerPaneView({ standalone, paneId }: ViewerPaneViewProps) {
  const activeTab = useViewerStore((state) => (paneId ? paneActiveTab(state, paneId) : null));
  // Only the focused pane's document is the application's active document.
  const focused = useViewerStore((state) => paneId !== null && state.focusedPaneId === paneId);
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
  const workspaceBookmarks = useBookmarksStore((s) => s.bookmarks);
  const setChatReaderDoc = useChatStore((s) => s.setReaderDoc);
  const chatBackendsLoaded = useChatStore((s) => s.backendsLoaded);
  const hasAvailableChatBackend = useChatStore((s) => s.hasAvailableBackend);
  const openChatPaneAndSend = useChatStore((s) => s.openPaneAndSend);
  const workspaceGenerationReady = useGenerationStore((state) => state.ready);
  const workspaceSemanticReady = useSemanticStore((state) => state.readyForCurrentRoot);
  const workspaceSearchResults = useSearchStore((state) => state.results);
  const listedDoi = useSettingsStore((state) =>
    !standalone && selectedMatch
      ? state.fileList.find((entry) => entry.path === selectedMatch.path)?.doi ?? null
      : null,
  );
  const bookmarks = standalone ? [] : workspaceBookmarks;
  const generationReady = !standalone && workspaceGenerationReady;
  const semanticReady = !standalone && workspaceSemanticReady;
  const searchResults = standalone ? [] : workspaceSearchResults;
  const { addToast } = useToasts();
  const pdfAutoZoomTargetPx = useSettingsStore(
    (state) => state.settings?.pdf_auto_zoom_target_px,
  );
  const colorScheme = useSettingsStore((state) => state.colorScheme);
  // A document may show the files it sits beside, and only through here: the
  // reader has already resolved the reference against the document, and this is
  // the application saying it will serve what that names. Nothing remote
  // reaches it -- the readers refuse those before asking. Kept stable on its
  // own, because a reader re-parses its document when this changes.
  const resolveLocalAsset = useCallback((path: string) => api.resolveAssetUrl(path), []);
  const resolvePdfFont = useCallback((font: PdfFontRequest) => api.resolvePdfFont(font), []);
  // Everything the readers need from this application. They take it from here
  // rather than importing the Tauri bridge and the settings store directly, so
  // the same components can be mounted by a host that has neither.
  const readerHost = useMemo<ReaderHostServices>(
    () => ({
      openExternal: (url) =>
        openLink(url).catch((e) => console.error("Open link failed:", e)),
      colorScheme,
      pdfAutoZoomTargetPx,
      resolveLocalAsset,
      resolvePdfFont,
      positions: readerPositionStore,
      pdfEdits: readerPdfEditStore,
    }),
    [colorScheme, pdfAutoZoomTargetPx, resolveLocalAsset, resolvePdfFont],
  );
  const [sidePanel, setSidePanel] = useState<ViewerSidePanel>(null);
  const [textView, setTextView] = useState<"source" | "rendered">("rendered");
  // A PDF is edited where it is read; a text document is edited as its source,
  // through the view toggle.
  const [editingPdf, setEditingPdf] = useState(false);
  const pdfReaderRef = useRef<PdfReaderHandle>(null);
  const [savingEdits, setSavingEdits] = useState(false);
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
    setEditingPdf(false);
  }, [selectedMatch?.path]);

  // A reflowable book's pages. Wilkes lays them out and
  // renders them -- so there is nothing for the asset protocol to point at and
  // the bytes come over IPC instead. Keyed on the path, which is what the
  // reader caches the parsed document under.
  const [bookPages, setBookPages] = useState<{ key: string; bytes: ArrayBuffer } | null>(null);
  const bookPath =
    selectedMatch && "PdfPage" in selectedMatch.origin && !isPdfPath(selectedMatch.path) && !servesPagesOnDemand(selectedMatch.path)
      ? selectedMatch.path
      : null;
  // Keyed on the book's path and the tab's *id*, never on the tab object: that
  // object's identity changes whenever any of its fields does — `previewLoading`
  // flipping is enough — and depending on it re-ran this effect mid-flight,
  // throwing away the pages it had just fetched and starting again.
  const activeTabId = activeTab?.id ?? null;
  useEffect(() => {
    setBookPages(null);
    if (!bookPath) return;
    let cancelled = false;
    api
      .surrogateBytes(bookPath)
      .then((bytes) => {
        if (!cancelled) setBookPages({ key: bookPath, bytes });
      })
      .catch((error) => {
        if (!cancelled && activeTabId) reportTabLoadError(activeTabId, error);
      });
    return () => {
      cancelled = true;
    };
  }, [bookPath, activeTabId, reportTabLoadError]);

  const comicCount = previewData && "Comic" in previewData ? previewData.Comic.page_count : 0;
  const comicPath = selectedMatch && servesPagesOnDemand(selectedMatch.path) ? selectedMatch.path : null;
  const comicSource = useMemo<ImagePageSource | null>(() => comicPath && comicCount > 0 ? {
    key: comicPath,
    pageCount: comicCount,
    loadPage: (page, signal) => api.comicPage(comicPath, page, signal),
  } : null, [comicPath, comicCount]);

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
    setTextView(readTextViewMode(selectedMatch.path));
  }, [selectedMatch?.path]);

  const setRememberedTextView = (view: "source" | "rendered") => {
    if (!selectedMatch) return;
    saveTextViewMode(selectedMatch.path, view);
    setTextView(view);
  };

  // PreviewPane is the single owner of "what's currently being viewed" since
  // it's the only component that knows both the open file and -- via
  // PdfViewer's live scroll tracking below -- the page actually on screen.
  // Publish that application state directly for external MCP, then separately
  // mirror it into the private chat context when the desktop chat is present.
  useEffect(() => {
    if (standalone || !focused) return;
    if (!selectedMatch) {
      api.setActiveDocument?.(null).catch((error) =>
        console.error("mcp: failed to clear active document", error),
      );
      if (isTauri) setChatReaderDoc(null);
      return;
    }
    const page = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : null;
    api.setActiveDocument?.(selectedMatch.path, page).catch((error) =>
      console.error("mcp: failed to update active document", error),
    );
    if (isTauri) setChatReaderDoc(selectedMatch.path, page);
  }, [selectedMatch, setChatReaderDoc, standalone, focused]);

  const handleChatPageChange = (page: number) => {
    if (standalone || !focused) return;
    if (selectedMatch) {
      api.setActiveDocument?.(selectedMatch.path, page).catch((error) =>
        console.error("mcp: failed to update active document page", error),
      );
    }
    if (isTauri && selectedMatch) setChatReaderDoc(selectedMatch.path, page);
  };

  const [isPdfRendering, setIsPdfRendering] = useState(false);
  const prevPdfUrlRef = useRef<string | null>(null);
  const displayData = previewData;

  // Show the loading spinner only when a new PDF file is opened, not when
  // navigating to a different match within the same file.
  useEffect(() => {
    if (selectedMatch) {
      if (isPagedPath(selectedMatch.path)) {
        const newUrl = api.resolveAssetUrl(selectedMatch.path);
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

  if (!activeTab || !selectedMatch || !paneId) {
    return (
      <div className="flex h-full flex-col bg-[var(--bg-app)] text-[var(--text-dim)]">
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

  // Whether this document is drawn as pages. A PDF, a book and a comic all are;
  // they differ in where the pages come from. A PDF's are in the file the
  // webview fetches. A comic's are the archive's own images, requested one at a
  // time. A book has none of its own, so Wilkes lays them out and renders them,
  // and the bytes arrive over IPC.
  //
  // Asked of the file rather than of the preview that arrived, because the
  // preview goes briefly stale when switching documents: a `Text` preview left
  // over from a text file must not turn a PDF into a code listing.
  const isPagedFile = "PdfPage" in selectedMatch.origin;
  const isComic = isPagedFile && servesPagesOnDemand(selectedMatch.path);
  const isBook = isPagedFile && !isComic && !isPdfPath(selectedMatch.path);
  // A PDF's own pages can be edited in place. A book's are a rendering Wilkes
  // made of it, and a comic's are pictures: neither has text of its own there.
  const isEditablePdf = isPagedFile && !isComic && !isBook;
  // The edits live in the reader; what is written is what the reader says is
  // on screen. The document itself is never the file that is written.
  const saveEditedCopy = async () => {
    setSavingEdits(true);
    try {
      // The reader first lays out every page the edits reach, which in a long
      // document is more than it has shown.
      const edits = (await pdfReaderRef.current?.exportEdits()) ?? null;
      if (!edits) {
        addToast("Nothing has been changed", { type: "info" });
        return;
      }
      const saved = await api.savePdfEdits(selectedMatch.path, edits);
      if (saved) addToast(`Saved a copy as ${fileNameOf(saved)}`, { type: "success" });
    } catch (error) {
      console.error("Saving an edited copy failed:", error);
      addToast(`Could not save the edited copy: ${error instanceof Error ? error.message : String(error)}`, {
        type: "error",
      });
    } finally {
      setSavingEdits(false);
    }
  };
  const isMarkdownFile =
    !isPagedFile && displayData != null && "Text" in displayData && displayData.Text.language === "markdown";
  const isHtmlFile =
    !isPagedFile && displayData != null && "Text" in displayData && displayData.Text.language === "html";
  // The two kinds of text document that have a second presentation. What
  // differs between them is which reader renders it, which is decided below;
  // everything else -- the toggle, the remembered choice, which palette a
  // bookmark is drawn in -- is the same question for both.
  const isRenderableFile = isMarkdownFile || isHtmlFile;
  const renderedFileLabel = isHtmlFile ? "HTML" : "Markdown";
  // A text document has one toggle between viewing and its source. Where the
  // editor is available the source is edited in it; the standalone viewer has
  // no editor, and there only a renderable document has a source to switch to.
  const isTextFile = !isPagedFile && displayData != null && "Text" in displayData;
  const canEdit = !standalone && isTextFile;
  const editingSource = canEdit && textView === "source";
  const viewToggleLabel = textView === "rendered"
    ? (canEdit ? "Edit document" : `View ${renderedFileLabel} source`)
    : (isRenderableFile ? `View rendered ${renderedFileLabel}` : "View document");
  const shouldRestoreSourceScroll =
    !isPagedFile &&
    "TextFile" in selectedMatch.origin &&
    selectedMatch.origin.TextFile.line === 0 &&
    selectedMatch.text_range == null;
  const pdfPage = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.page : 1;
  const pdfBbox = "PdfPage" in selectedMatch.origin ? selectedMatch.origin.PdfPage.bbox : null;
  // What this document's reading says its own areas hold, so the reader can
  // offer that instead of the glyph runs the reading dropped. It arrives with
  // the preview rather than on a call of its own: opening the document is
  // already the request that asks what showing it needs.
  // A book's table of contents and links, in the reader's vocabulary. Null for
  // a PDF, which has its own inside the file.
  // A book's pages are fetched after its preview arrives, and a comic has none
  // to show until its preview has said how many there are, so `previewLoading`
  // is already false while they are in flight. Without this the pane renders
  // the final `null` of the chain below — blank, with nothing to say why.
  const waitingForPages = ((isBook && bookPages == null) || (isComic && !comicSource)) && !previewError;
  const bookOutline =
    displayData && "Book" in displayData ? readerOutline(displayData.Book.outline) : null;
  const bookLinks =
    displayData && "Book" in displayData ? readerLinks(displayData.Book.links) : null;
  const pdfSuperseded =
    displayData && "Pdf" in displayData ? displayData.Pdf.superseded : undefined;
  const selectedSearchMatch = isPagedFile
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
        className: isRenderableFile && textView === "rendered"
          ? "rendered-bookmark-highlight"
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
      onAddBookmark={standalone ? undefined : handleAddBookmark}
      showChatActions={chatSelectionActionsAvailable}
      onExplain={handleExplainSelection}
      onAsk={handleAskSelection}
    />
  );
  // Text locations stay in persisted UTF-8 document coordinates. Each renderer
  // translates only at its boundary (CodeMirror uses UTF-16; Markdown does not).
  const renderedHighlightRange =
    !isPagedFile && displayData && "Text" in displayData
      ? selectedMatch.text_range ?? displayData.Text.highlight_range
      : { start: 0, end: 0 };
  const bboxesEqual = (a: BoundingBox | null, b: BoundingBox | null) =>
    a != null &&
    b != null &&
    a.x === b.x &&
    a.y === b.y &&
    a.width === b.width &&
    a.height === b.height;
  // The reader locates the navigation target but only draws geometry it worked
  // out itself, so a target the host has coordinates for is marked from here.
  // A bookmark at the same origin is already drawing that spot: marking it
  // again is what stacked a second highlight under the bookmark's own.
  const targetIsBookmarked = bookmarks.some(
    (bookmark) =>
      bookmark.path === selectedMatch.path &&
      bookmark.rects.length > 0 &&
      "PdfPage" in bookmark.origin &&
      bookmark.origin.PdfPage.page === pdfPage &&
      bboxesEqual(bookmark.origin.PdfPage.bbox, pdfBbox),
  );
  const readerDecorations: Decoration[] =
    isPagedFile && pdfBbox && !pdfSearchLocator && !targetIsBookmarked
      ? [
          ...bookmarkDecorations,
          {
            id: "navigation-target",
            anchor: { kind: "rects", page: pdfPage, rects: [pdfBbox] },
          },
        ]
      : bookmarkDecorations;
  const title = headerTitle(viewerMetadata);
  const author = headerAuthor(viewerMetadata);
  const createdAt = formatDocumentMonthYear(viewerMetadata?.created_at);
  const links = buildExternalLinks(viewerMetadata?.doi, viewerMetadata?.title);
  const doi = links?.doi ?? null;

  const handleOpenDoi = () => {
    if (!links?.doiUrl) return;
    openLink(links.doiUrl).catch((e) => console.error("Open DOI failed:", e));
  };

  const handleOpenScholar = () => {
    if (!links) return;
    openLink(links.googleScholarUrl).catch((e) => console.error("Open Google Scholar failed:", e));
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
      <ViewerTabs standalone={standalone} paneId={paneId} />
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

        {!standalone && generationReady && (
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

        {!standalone && <Tooltip content={sidePanel === "related" ? "Hide related documents" : "Show related documents"}>
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
        </Tooltip>}

        {!standalone && currentDoi && (
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

        {!standalone && <Tooltip
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
        </Tooltip>}

        {(isRenderableFile || canEdit) && (
          <Tooltip content={viewToggleLabel}>
            <button
              type="button"
              onClick={() => setRememberedTextView(textView === "rendered" ? "source" : "rendered")}
              aria-label={viewToggleLabel}
              aria-pressed={canEdit ? editingSource : undefined}
              className={`inline-flex p-1 rounded border border-[var(--border-main)] text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-header)] hover:text-[var(--text-main)] ${
                editingSource ? "bg-[var(--bg-active)] text-[var(--text-main)]" : "bg-[var(--bg-active)]"
              }`}
            >
              {textView === "source" ? <Eye size={16} /> : canEdit ? <Edit3 size={16} /> : <Code size={16} />}
            </button>
          </Tooltip>
        )}

        {!standalone && isEditablePdf && (
          <Tooltip content={editingPdf ? "Return to document viewer" : "Edit document"}>
            <button
              type="button"
              onClick={() => setEditingPdf((current) => !current)}
              aria-label={editingPdf ? "Finish editing document" : "Edit document"}
              aria-pressed={editingPdf}
              className={`inline-flex rounded border border-[var(--border-main)] p-1 text-[var(--text-dim)] ${editingPdf ? "bg-[var(--bg-active)] text-[var(--text-main)]" : ""}`}
            >
              <Edit3 size={16} />
            </button>
          </Tooltip>
        )}

        {!standalone && editingPdf && isEditablePdf && (
          <Tooltip content="Save a copy with your changes">
            <button
              type="button"
              onClick={() => void saveEditedCopy()}
              disabled={savingEdits}
              aria-label="Save an edited copy"
              className="inline-flex rounded border border-[var(--border-main)] p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-header)] hover:text-[var(--text-main)] disabled:opacity-50"
            >
              <Save size={16} />
            </button>
          </Tooltip>
        )}

        {!standalone && editingPdf && isEditablePdf && (
          <Tooltip content="Discard your changes">
            <button
              type="button"
              onClick={() => {
                void confirmDialog(
                  "Discard every change made to this document? The document itself has not been altered.",
                ).then((confirmed) => {
                  if (confirmed) pdfReaderRef.current?.discardEdits();
                });
              }}
              aria-label="Discard changes"
              className="inline-flex rounded border border-[var(--border-main)] p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-header)] hover:text-[var(--text-main)]"
            >
              <RotateCcw size={16} />
            </button>
          </Tooltip>
        )}

      </div>

      {/* Content */}
      <div
        id={`viewer-tabpanel-${paneId}`}
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
            {(previewLoading || isPdfRendering || waitingForPages) && (
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
            {isComic && comicSource ? (
              <ImagePageViewer
                key={`${selectedMatch.path}:${pdfLoadAttempt}`}
                source={comicSource}
                page={pdfPage}
                highlight_bbox={pdfBbox}
                decorations={readerDecorations}
                onRenderSuccess={() => setIsPdfRendering(false)}
                onPageChange={handleChatPageChange}
              />
            ) : isPagedFile && !isComic && (!isBook || bookPages) ? (
              <PdfViewer
                key={selectedMatch.path}
                // A PDF is fetched from the file by the webview; a book has no
                // pages in its file at all, so Wilkes renders them and they
                // arrive as bytes with the book's path as their identity.
                source={bookPages ?? api.resolveAssetUrl(selectedMatch.path)}
                // Neither survives a rendering -- a page writer emits drawing
                // operations, and an outline and a link annotation are not
                // drawing operations -- so for a book the host supplies what it
                // resolved when it laid the book out. A PDF carries its own,
                // and passing nothing leaves the reader to read them.
                outline={bookOutline}
                links={bookLinks}
                loadAttempt={pdfLoadAttempt}
                page={pdfPage}
                highlight_bbox={pdfBbox}
                search_locator={pdfSearchLocator}
                decorations={readerDecorations}
                textSubstitutions={pdfSuperseded}
                editing={editingPdf && isEditablePdf}
                ref={pdfReaderRef}
                slots={{ selectionActions: selectionActionsSlot }}
                onRenderSuccess={() => setIsPdfRendering(false)}
                onLoadError={(error) => reportTabLoadError(activeTab.id, error)}
                onPageChange={handleChatPageChange}
              />
            ) : isMarkdownFile && textView === "rendered" ? (
              <MarkdownViewer
                content={displayData.Text.content}
                documentPath={selectedMatch.path}
                restoreScrollPosition={shouldRestoreSourceScroll}
                highlightRange={renderedHighlightRange}
                decorations={readerDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
              />
            ) : isHtmlFile && textView === "rendered" ? (
              <HtmlViewer
                content={displayData.Text.content}
                documentPath={selectedMatch.path}
                restoreScrollPosition={shouldRestoreSourceScroll}
                highlightRange={renderedHighlightRange}
                decorations={readerDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
              />
            ) : displayData && "Text" in displayData && editingSource ? (
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
                decorations={readerDecorations}
                slots={{ selectionActions: selectionActionsSlot }}
              />
            ) : null}
          </div>
          </ReaderHostProvider>
          {!standalone && sidePanel === "summary" && generationReady && (
            <DocumentSummaryPane
              path={selectedMatch.path}
              onClose={() => setSidePanel(null)}
            />
          )}
          {!standalone && sidePanel === "related" && (
            <RelatedDocumentsPane
              currentPath={selectedMatch.path}
              onOpenDocument={openFile}
              onClose={() => setSidePanel(null)}
            />
          )}
          {!standalone && sidePanel === "citations" && currentDoi && (
            <CitationGraphPane
              currentPath={selectedMatch.path}
              doi={currentDoi}
              onOpenDocument={openFile}
              onClose={() => setSidePanel(null)}
            />
          )}
          {!standalone && sidePanel === "topics" && semanticReady && (
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
