import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type Ref,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ByteRange } from "./documentCoordinates";
import type { DocumentSelection } from "./selection";
import SelectionLayer from "./SelectionLayer";
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
import { elementAnchor, rangeDecorations, type Decoration } from "./decorations";
import type { ReaderSlots } from "./slots";
import type { FindableReaderHandle, ZoomableReaderHandle } from "./readerHandle";

/** The reader's own emphasis for the navigation target. Kept distinct from the
 *  host's decorations: where a document opens is the reader's business. */
const SEARCH_DECORATION_ID = "reader:search";

export interface MarkdownReaderHandle extends FindableReaderHandle, ZoomableReaderHandle {}

export interface MarkdownViewerProps {
  content: string;
  documentPath: string;
  restoreScrollPosition?: boolean;
  highlightRange: ByteRange;
  /** Host-owned marks. Only `range`-anchored decorations are placeable here;
   *  `rects` anchors belong to the PDF reader and are ignored. */
  decorations?: Decoration[];
  slots?: ReaderSlots;
  ref?: Ref<MarkdownReaderHandle>;
}

export default function MarkdownViewer({
  content,
  documentPath,
  restoreScrollPosition = true,
  highlightRange,
  decorations = [],
  slots,
  ref,
}: MarkdownViewerProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const annotations = useMemo<TextAnnotation[]>(() => [
    ...(highlightRange.end > highlightRange.start
      ? [{ id: SEARCH_DECORATION_ID, className: "markdown-search-highlight", range: highlightRange }]
      : []),
    ...rangeDecorations(decorations),
  ], [decorations, highlightRange]);
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
  const domSelection = useDomDocumentSelection({
    rootRef,
    mapSelection,
    dismissOnCollapsedSelection: true,
  });

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

  useImperativeHandle(
    ref,
    (): MarkdownReaderHandle => ({
      scrollToDecoration: (id) => {
        const element = rootRef.current?.querySelector<HTMLElement>(
          `[data-decoration-ids~="${id}"], [data-decoration-ids^="${id},"], [data-decoration-ids*=",${id},"], [data-decoration-ids$=",${id}"]`,
        );
        element?.scrollIntoView?.({ block: "center" });
      },
      openFind: (query) => {
        if (query !== undefined) find.setQuery(query);
        find.open();
      },
      closeFind: find.close,
      getZoom: () => zoom,
      setZoom: (next) => changeZoom(() => next),
    }),
    [ref, find, zoom, changeZoom],
  );

  useEffect(() => {
    if (restoreScrollPosition || highlightRange.end <= highlightRange.start) return;
    const highlighted = rootRef.current?.querySelector<HTMLElement>(".markdown-search-highlight");
    highlighted?.scrollIntoView?.({ block: "center" });
  }, [highlightRange, restoreScrollPosition, content]);

  return (
    <div
      ref={rootRef}
      onClick={(event) => {
        if (!(event.target instanceof Element)) return;
        const marked = event.target.closest<HTMLElement>("[data-decoration-ids]");
        const ids = marked?.dataset.decorationIds?.split(",") ?? [];
        if (!marked) return;
        // Overlapping decorations produce one span carrying every id. Activate
        // the first that actually wants activation rather than the first id,
        // which may belong to a purely visual mark.
        for (const id of ids) {
          const decoration = decorations.find((candidate) => candidate.id === id);
          if (decoration?.onActivate) {
            decoration.onActivate(id, elementAnchor(marked));
            return;
          }
        }
      }}
      onMouseUp={domSelection.readSelection}
      className="relative h-full overflow-hidden"
    >
      <div ref={scrollRef} className="h-full overflow-auto px-6 py-5 text-sm text-[var(--text-main)]">
        <article className="prose prose-document" style={{ fontSize: `${zoom}rem` }}>
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
      <SelectionLayer
        positioned={domSelection.positioned}
        api={domSelection.slotApi}
        slot={slots?.selectionActions}
      />
      {find.isOpen && (
        <div className="absolute top-4 right-4 z-20">
          <FindBar find={find} matchCount={matchCount} />
        </div>
      )}
      <div className="absolute bottom-4 right-4 z-20 flex flex-col gap-2 items-end">
        <div className="flex items-center gap-1.5 bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-lg px-2.5 py-1.5 text-sm text-[var(--text-main)]">
          {slots?.toolbar}
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
