import { useEffect, useRef, useState } from "react";
import { EditorState, RangeSetBuilder, StateField, StateEffect } from "@codemirror/state";
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
import type { ByteRange } from "../../lib/types";
import SelectionActions, {
  type DocumentSelection,
  type PositionedSelection,
} from "./SelectionActions";
import { textSelectionFromUtf16Range, utf8ByteRangeToUtf16Range } from "./textOffsets";
import { readTextScrollPosition, saveTextScrollPosition } from "./textScrollMemory";

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
        builder.add(from, to, Decoration.mark({ class: "cm-highlight-match" }));
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
  ".cm-bookmark-highlight": {
    backgroundColor: "rgba(59, 130, 246, 0.16)",
    borderBottom: "1px solid rgba(59, 130, 246, 0.55)",
  },
});

const setBookmarkHighlights = StateEffect.define<Array<{ id: string; range: ByteRange }>>();

const bookmarkHighlightField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const effect of tr.effects) {
      if (effect.is(setBookmarkHighlights)) {
        const builder = new RangeSetBuilder<Decoration>();
        for (const { id, range } of [...effect.value].sort((a, b) => a.range.start - b.range.start)) {
          if (range.end <= range.start) continue;
          builder.add(
            range.start,
            range.end,
            Decoration.mark({ class: "cm-bookmark-highlight", attributes: { "data-bookmark-id": id } }),
          );
        }
        return builder.finish();
      }
    }
    return deco.map(tr.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
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

// ── Component ─────────────────────────────────────────────────────────────────

export interface CodeViewerProps {
  content: string;
  language: string | null;
  documentPath: string;
  restoreScrollPosition?: boolean;
  highlightLine: number;
  highlightRange: { start: number; end: number };
  bookmarkHighlights?: Array<{ id: string; range: ByteRange }>;
  onAddBookmark?: (selection: DocumentSelection) => void;
  showChatSelectionActions?: boolean;
  onExplainSelection?: (selection: DocumentSelection) => void;
  onAskSelection?: (selection: DocumentSelection, question: string) => void;
}

export default function CodeViewer({
  content,
  language,
  documentPath,
  restoreScrollPosition = false,
  highlightLine,
  highlightRange,
  bookmarkHighlights = [],
  onAddBookmark,
  showChatSelectionActions = false,
  onExplainSelection,
  onAskSelection,
}: CodeViewerProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [selectionAction, setSelectionAction] = useState<PositionedSelection | null>(null);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!containerRef.current) return;

    const langExt = getLanguageExtension(language);
    const extensions = [
      basicSetup,
      EditorState.readOnly.of(true),
      highlightField,
      bookmarkHighlightField,
      highlightTheme,
      EditorView.lineWrapping,
      EditorView.updateListener.of((update) => {
        if (!update.selectionSet) return;
        const range = update.state.selection.main;
        if (range.empty) {
          setSelectionAction(null);
          return;
        }
        const from = Math.min(range.from, range.to);
        const to = Math.max(range.from, range.to);
        const quote = update.state.sliceDoc(from, to).trim();
        const root = rootRef.current;
        const coords = update.view.coordsAtPos(to);
        if (!quote || !root || !coords) {
          setSelectionAction(null);
          return;
        }
        const fullText = update.state.doc.toString();
        const line = update.state.doc.lineAt(from);
        const rootRect = root.getBoundingClientRect();
        setSelectionAction({
          selection: textSelectionFromUtf16Range(fullText, from, to, line.number, line.from),
          left: Math.min(Math.max(coords.left - rootRect.left, 8), Math.max(rootRect.width - 128, 8)),
          top: Math.min(Math.max(coords.bottom - rootRect.top + 3, 8), Math.max(rootRect.height - 40, 8)),
        });
      }),
    ];
    if (isDark) extensions.push(oneDark);
    if (langExt) extensions.push(langExt);

    const state = EditorState.create({ doc: content, extensions });
    const view = new EditorView({ state, parent: containerRef.current });
    viewRef.current = view;
    const savePosition = () => {
      const maximum = view.scrollDOM.scrollHeight - view.scrollDOM.clientHeight;
      saveTextScrollPosition(documentPath, "source", maximum > 0 ? view.scrollDOM.scrollTop / maximum : 0);
    };
    const onScroll = () => savePosition();
    view.scrollDOM.addEventListener("scroll", onScroll, { passive: true });

    let frame: number | null = null;
    if (restoreScrollPosition) {
      const position = readTextScrollPosition(documentPath, "source");
      if (position !== null) {
        frame = window.requestAnimationFrame(() => {
          view.scrollDOM.scrollTop = position * Math.max(view.scrollDOM.scrollHeight - view.scrollDOM.clientHeight, 0);
        });
      }
    }

    return () => {
      if (frame !== null) window.cancelAnimationFrame(frame);
      savePosition();
      view.scrollDOM.removeEventListener("scroll", onScroll);
      view.destroy();
      viewRef.current = null;
    };
  }, [content, language, isDark, documentPath, restoreScrollPosition]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view || !content) return;

    const docLen = view.state.doc.length;
    const from = Math.min(highlightRange.start, docLen);
    const to = Math.min(highlightRange.end, docLen);

    view.dispatch({ effects: setHighlight.of({ from, to }) });

    if (!restoreScrollPosition && highlightLine > 0 && highlightLine <= view.state.doc.lines) {
      const lineInfo = view.state.doc.line(highlightLine);
      view.dispatch({
        effects: EditorView.scrollIntoView(lineInfo.from, { y: "center" }),
      });
    }
  }, [content, highlightLine, highlightRange]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const converted = bookmarkHighlights.map(({ id, range }) => ({
      id,
      range: utf8ByteRangeToUtf16Range(content, range),
    }));
    view.dispatch({ effects: setBookmarkHighlights.of(converted) });
  }, [bookmarkHighlights, content]);

  return (
    <div ref={rootRef} className="relative h-full w-full overflow-hidden">
      <div ref={containerRef} className="h-full w-full overflow-auto text-sm" />
      <SelectionActions
        positioned={selectionAction}
        onAddBookmark={onAddBookmark}
        showChatActions={showChatSelectionActions}
        onExplain={onExplainSelection}
        onAsk={onAskSelection}
        onDismiss={() => setSelectionAction(null)}
        onClearSelection={() => {
          const view = viewRef.current;
          if (!view) return;
          view.dispatch({ selection: { anchor: view.state.selection.main.head } });
        }}
      />
    </div>
  );
}
