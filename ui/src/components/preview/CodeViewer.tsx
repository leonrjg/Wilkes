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
  highlightLine: number;
  highlightRange: { start: number; end: number };
}

export default function CodeViewer({ content, language, highlightLine, highlightRange }: CodeViewerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
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
      highlightTheme,
      EditorView.lineWrapping,
    ];
    if (isDark) extensions.push(oneDark);
    if (langExt) extensions.push(langExt);

    const state = EditorState.create({ doc: content, extensions });
    const view = new EditorView({ state, parent: containerRef.current });
    viewRef.current = view;

    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, [content, language, isDark]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view || !content) return;

    const docLen = view.state.doc.length;
    const from = Math.min(highlightRange.start, docLen);
    const to = Math.min(highlightRange.end, docLen);

    view.dispatch({ effects: setHighlight.of({ from, to }) });

    if (highlightLine > 0 && highlightLine <= view.state.doc.lines) {
      const lineInfo = view.state.doc.line(highlightLine);
      view.dispatch({
        effects: EditorView.scrollIntoView(lineInfo.from, { y: "center" }),
      });
    }
  }, [content, highlightLine, highlightRange]);

  return <div ref={containerRef} className="h-full w-full overflow-auto text-sm" />;
}
