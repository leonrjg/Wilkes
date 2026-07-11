import { useCallback, useEffect, useMemo, useRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ByteRange } from "../../lib/types";
import SelectionActions, { type DocumentSelection } from "./SelectionActions";
import { sourceBoundaryForDomPoint, sourceMappedMarkdown, type TextAnnotation } from "./markdownSourceMap";
import { readTextScrollPosition, saveTextScrollPosition } from "./textScrollMemory";
import { utf8ByteOffsetToUtf16Offset } from "./textOffsets";
import { useDomDocumentSelection } from "./useDomDocumentSelection";

interface MarkdownViewerProps {
  content: string;
  documentPath: string;
  restoreScrollPosition?: boolean;
  highlightRange: ByteRange;
  bookmarkHighlights?: Array<{ id: string; range: ByteRange }>;
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
    <div ref={rootRef} onMouseUp={domSelection.readSelection} className="relative h-full overflow-hidden">
      <div ref={scrollRef} className="h-full overflow-auto px-6 py-5 text-sm text-[var(--text-main)]">
        <article className="prose-document">
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
    </div>
  );
}
