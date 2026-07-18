import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ByteRange } from "../../lib/types";
import SelectionActions, { type DocumentSelection } from "./SelectionActions";
import FindBar from "./FindBar";
import ZoomControls, { ZOOM_STEP } from "./ZoomControls";
import { useDocumentFind } from "./useDocumentFind";
import { useMarkdownFind } from "./useMarkdownFind";
import { sourceBoundaryForDomPoint, sourceMappedMarkdown, type TextAnnotation } from "./markdownSourceMap";
import {
  readTextScrollPosition,
  saveTextScrollPosition,
  readMarkdownZoom,
  saveMarkdownZoom,
  MARKDOWN_MIN_ZOOM,
  MARKDOWN_MAX_ZOOM,
} from "./textScrollMemory";
import { utf8ByteOffsetToUtf16Offset } from "./textOffsets";
import { useDomDocumentSelection } from "./useDomDocumentSelection";
import { bookmarkAnchorFor, type BookmarkOpenHandler } from "./bookmarkPosition";

interface MarkdownViewerProps {
  content: string;
  documentPath: string;
  restoreScrollPosition?: boolean;
  highlightRange: ByteRange;
  bookmarkHighlights?: Array<{ id: string; range: ByteRange }>;
  onBookmarkOpen?: BookmarkOpenHandler;
  onAddBookmark?: (selection: DocumentSelection) => void;
  showChatSelectionActions?: boolean;
  onExplainSelection?: (selection: DocumentSelection) => void;
  onAskSelection?: (selection: DocumentSelection, question: string) => void;
}

export default function MarkdownViewer({
  content,
  documentPath,
  restoreScrollPosition = true,
  highlightRange,
  bookmarkHighlights = [],
  onBookmarkOpen,
  onAddBookmark,
  showChatSelectionActions = false,
  onExplainSelection,
  onAskSelection,
}: MarkdownViewerProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const annotations = useMemo<TextAnnotation[]>(() => [
    ...(highlightRange.end > highlightRange.start ? [{ id: "search", kind: "search" as const, range: highlightRange }] : []),
    ...bookmarkHighlights.map(({ id, range }) => ({ id, kind: "bookmark" as const, range })),
  ], [bookmarkHighlights, highlightRange]);
  const rehypePlugins = useMemo(() => [sourceMappedMarkdown(content, annotations)], [content, annotations]);

  const mapSelection = useCallback((range: Range, selection: Selection): DocumentSelection | null => {
    const start = sourceBoundaryForDomPoint(range.startContainer, range.startOffset);
    const end = sourceBoundaryForDomPoint(range.endContainer, range.endOffset);
    if (start == null || end == null || end <= start) return null;
    const prefix = content.slice(0, utf8ByteOffsetToUtf16Offset(content, start));
    const lineStart = prefix.lastIndexOf("\n") + 1;
    return {
      quote: selection.toString().trim(),
      origin: {
        TextFile: {
          line: prefix.split("\n").length,
          col: start - new TextEncoder().encode(content.slice(0, lineStart)).length,
        },
      },
      text_range: { start, end },
      rects: [],
    };
  }, [content]);
  const domSelection = useDomDocumentSelection({ rootRef, mapSelection });

  const [zoom, setZoom] = useState(() => readMarkdownZoom(documentPath));
  const changeZoom = useCallback((next: (zoom: number) => number) => {
    setZoom((current) => {
      const clamped = Math.min(Math.max(+next(current).toFixed(2), MARKDOWN_MIN_ZOOM), MARKDOWN_MAX_ZOOM);
      saveMarkdownZoom(documentPath, clamped);
      return clamped;
    });
  }, [documentPath]);

  // The viewer stays mounted across documents, so pick up the newly opened
  // file's remembered zoom the same way the scroll effect re-reads its position.
  useEffect(() => setZoom(readMarkdownZoom(documentPath)), [documentPath]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      if (event.key === "=" || event.key === "+") {
        event.preventDefault();
        changeZoom((z) => z + ZOOM_STEP);
      } else if (event.key === "-") {
        event.preventDefault();
        changeZoom((z) => z - ZOOM_STEP);
      } else if (event.key === "0") {
        event.preventDefault();
        changeZoom(() => 1);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [changeZoom]);

  const [matchCount, setMatchCount] = useState(0);
  const find = useDocumentFind(matchCount);
  useMarkdownFind({
    rootRef: scrollRef,
    content,
    query: find.query,
    isOpen: find.isOpen,
    currentIdx: find.currentIdx,
    onMatchCount: setMatchCount,
  });

  useEffect(() => {
    const scroll = scrollRef.current;
    if (!scroll) return;

    const savePosition = () => {
      const maximum = scroll.scrollHeight - scroll.clientHeight;
      saveTextScrollPosition(documentPath, "rendered", maximum > 0 ? scroll.scrollTop / maximum : 0);
    };
    const onScroll = () => savePosition();
    scroll.addEventListener("scroll", onScroll, { passive: true });

    let frame: number | null = null;
    if (restoreScrollPosition) {
      const position = readTextScrollPosition(documentPath, "rendered");
      if (position !== null) {
        frame = window.requestAnimationFrame(() => {
          scroll.scrollTop = position * Math.max(scroll.scrollHeight - scroll.clientHeight, 0);
        });
      }
    }

    return () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      savePosition();
      scroll.removeEventListener("scroll", onScroll);
    };
  }, [content, documentPath, restoreScrollPosition]);

  useEffect(() => {
    if (restoreScrollPosition || highlightRange.end <= highlightRange.start) return;
    const highlighted = rootRef.current?.querySelector<HTMLElement>(".markdown-search-highlight");
    highlighted?.scrollIntoView?.({ block: "center" });
  }, [highlightRange, restoreScrollPosition, content]);

  return (
    <div
      ref={rootRef}
      onClick={(event) => {
        if (!onBookmarkOpen || !(event.target instanceof Element)) return;
        const highlight = event.target.closest<HTMLElement>("[data-bookmark-ids]");
        const ids = highlight?.dataset.bookmarkIds;
        const bookmarkId = ids?.split(",")[0];
        if (bookmarkId && highlight) onBookmarkOpen(bookmarkId, bookmarkAnchorFor(highlight));
      }}
      onMouseUp={domSelection.readSelection}
      className="relative h-full overflow-hidden"
    >
      <div ref={scrollRef} className="h-full overflow-auto px-6 py-5 text-sm text-[var(--text-main)]">
        <article className="prose-document" style={{ fontSize: `${zoom}rem` }}>
          <ReactMarkdown
            remarkPlugins={[remarkGfm]}
            rehypePlugins={rehypePlugins}
            components={{
              a: ({ children, href }) => (
                <a href={href} target="_blank" rel="noreferrer">
                  {children}
                </a>
              ),
            }}
          >
            {content}
          </ReactMarkdown>
        </article>
      </div>
      <SelectionActions
        positioned={domSelection.positioned}
        onAddBookmark={onAddBookmark}
        showChatActions={showChatSelectionActions}
        onExplain={onExplainSelection}
        onAsk={onAskSelection}
        onDismiss={domSelection.dismiss}
        onClearSelection={domSelection.clearSelection}
        dismissOnCollapsedDomSelection
      />
      <div className="absolute bottom-4 right-4 z-20 flex flex-col gap-2 items-end">
        {find.isOpen && <FindBar find={find} matchCount={matchCount} />}
        <div className="flex items-center gap-1 bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-lg px-2 py-1 text-xs text-[var(--text-main)]">
          <ZoomControls
            zoom={zoom}
            onZoomIn={() => changeZoom((z) => z + ZOOM_STEP)}
            onZoomOut={() => changeZoom((z) => z - ZOOM_STEP)}
          />
        </div>
      </div>
    </div>
  );
}
