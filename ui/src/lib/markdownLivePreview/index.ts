import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { LanguageSupport, syntaxTree } from "@codemirror/language";
import { Prec, StateField, type Extension } from "@codemirror/state";
import { EditorView, keymap, type Command } from "@codemirror/view";
import { getLanguageExtension } from "@leonrjg/wilkes-reader";
import { buildPreview, type PreviewState } from "./decorations";
import { mathSyntax } from "./mathSyntax";
import { classifyReference } from "./references";
import type { PreviewHost } from "./widgets";

export interface LivePreviewOptions {
  /** The document's own path, which relative links and images resolve against. */
  documentPath: string;
  dark: boolean;
  /** URL the application serves a local file at. */
  resolveAsset(path: string): string;
  /** Open a web address or a local path. */
  openLink(target: string): void;
}

/** Info-string spellings that name a language `getLanguageExtension` knows by
 *  another name. */
const CODE_LANGUAGE_ALIASES: Record<string, string> = {
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  py: "python",
  rs: "rust",
  yml: "yaml",
  "c++": "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  h: "c",
  golang: "go",
  htm: "html",
  svg: "xml",
  md: "markdown",
};

function codeLanguage(info: string) {
  const name = info.trim().split(/\s+/)[0]?.toLowerCase() ?? "";
  const extension = getLanguageExtension(CODE_LANGUAGE_ALIASES[name] ?? name);
  return extension instanceof LanguageSupport ? extension.language : null;
}

const MONO = 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';

const previewTheme = () => EditorView.theme({
  ".cm-content": {
    fontFamily: 'ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif',
    lineHeight: "1.65",
  },
  // The syntax highlighter colours and underlines the spans inside a line;
  // rendered constructs take the document's own colours instead, so the
  // rules reach those spans.
  ".cm-md-heading": { fontWeight: "700" },
  ".cm-md-heading, .cm-md-heading *": { color: "var(--text-main)", textDecoration: "none" },
  ".cm-md-h1": { fontSize: "1.75em", lineHeight: "1.4" },
  ".cm-md-h2": { fontSize: "1.45em", lineHeight: "1.4" },
  ".cm-md-h3": { fontSize: "1.25em" },
  ".cm-md-h4": { fontSize: "1.1em" },
  ".cm-md-h6": { color: "var(--text-muted)" },
  ".cm-md-em": { fontStyle: "italic" },
  ".cm-md-strong": { fontWeight: "700" },
  ".cm-md-strike": { textDecoration: "line-through" },
  ".cm-md-inline-code": {
    fontFamily: MONO,
    fontSize: "0.88em",
    backgroundColor: "var(--bg-active)",
    borderRadius: "4px",
    padding: "1px 4px",
  },
  ".cm-md-link, .cm-md-link *": { color: "var(--accent-blue)", textDecoration: "underline", textUnderlineOffset: "2px" },
  ".cm-line.cm-md-quote": {
    borderLeft: "3px solid var(--border-strong)",
    paddingLeft: "12px",
    color: "var(--text-muted)",
  },
  ".cm-line.cm-md-codeblock": {
    fontFamily: MONO,
    fontSize: "0.88em",
    backgroundColor: "var(--bg-active)",
    paddingLeft: "12px",
    paddingRight: "12px",
  },
  ".cm-line.cm-md-codeblock-first": { borderTopLeftRadius: "6px", borderTopRightRadius: "6px", paddingTop: "4px" },
  ".cm-line.cm-md-codeblock-last": { borderBottomLeftRadius: "6px", borderBottomRightRadius: "6px", paddingBottom: "4px" },
  ".cm-md-code-label": { fontSize: "0.8em", color: "var(--text-dim)", fontFamily: MONO },
  ".cm-line.cm-md-table-source, .cm-line.cm-md-math-source, .cm-md-math-source": { fontFamily: MONO, fontSize: "0.88em" },
  ".cm-md-table-source *": { textDecoration: "none" },
  ".cm-md-table-wrap": { overflowX: "auto", padding: "6px 0", cursor: "text" },
  ".cm-md-table": { borderCollapse: "collapse", fontSize: "0.95em" },
  ".cm-md-table th, .cm-md-table td": {
    border: "1px solid var(--border-main)",
    padding: "4px 10px",
    verticalAlign: "top",
  },
  ".cm-md-table th": { backgroundColor: "var(--bg-header)", fontWeight: "600" },
  ".cm-md-math-block": { padding: "6px 0", overflowX: "auto", cursor: "text" },
  ".cm-md-math-block .katex-display": { margin: "0" },
  ".cm-md-math-preview": { opacity: "0.8", borderTop: "1px dashed var(--border-main)", cursor: "default" },
  ".cm-md-math-inline": { cursor: "text" },
  ".cm-md-render-error": { color: "var(--accent-red)", fontFamily: MONO, fontSize: "0.85em", whiteSpace: "pre-wrap" },
  ".cm-md-image-wrap": { cursor: "text" },
  ".cm-md-image": { maxWidth: "100%", maxHeight: "28em", verticalAlign: "middle", borderRadius: "4px" },
  ".cm-md-image-missing": { color: "var(--text-muted)", fontStyle: "italic" },
  ".cm-md-rule": {
    display: "inline-block",
    width: "100%",
    verticalAlign: "middle",
    borderTop: "1px solid var(--border-strong)",
  },
  ".cm-md-bullet": { color: "var(--text-muted)", display: "inline-block", minWidth: "0.9em" },
  ".cm-md-list-number, .cm-md-list-number *": { color: "var(--text-muted)" },
  ".cm-md-checkbox": { margin: "0 6px 0 0", verticalAlign: "middle", cursor: "pointer" },
  ".cm-md-task-done": { color: "var(--text-dim)", textDecoration: "line-through" },
  ".cm-md-mermaid": {
    display: "flex",
    justifyContent: "center",
    padding: "8px 0",
    color: "var(--text-dim)",
    cursor: "text",
  },
  ".cm-md-mermaid svg": { maxWidth: "100%", height: "auto" },
});

/** Arrow keys step into a construct drawn as a block widget, which the cursor
 *  would otherwise move straight past without ever revealing its source. */
function stepIntoBlock(field: StateField<PreviewState>, forward: boolean): Command {
  return (view) => {
    const { state } = view;
    const main = state.selection.main;
    if (!main.empty) return false;
    const line = state.doc.lineAt(main.head);
    const neighbour = forward ? line.number + 1 : line.number - 1;
    if (neighbour < 1 || neighbour > state.doc.lines) return false;
    const target = state.doc.line(neighbour);
    const block = state.field(field).blocks.find((span) => (forward ? span.from === target.from : span.to === target.to));
    if (!block) return false;
    const anchor = forward ? block.from : state.doc.lineAt(block.to).from;
    view.dispatch({ selection: { anchor }, scrollIntoView: true });
    return true;
  };
}

/** Markdown that shows formatted while it is edited: headings, emphasis,
 *  links, lists, tasks, quotes, code blocks, images, tables, TeX math and
 *  Mermaid diagrams render in place, and the Markdown source of whatever the
 *  selection touches shows for editing. The document text is never rewritten:
 *  everything here is decoration over the plain buffer. */
export function markdownLivePreview(options: LivePreviewOptions): Extension {
  const host: PreviewHost = {
    dark: options.dark,
    imageUrl(reference) {
      const target = classifyReference(options.documentPath, reference);
      if (target?.kind === "data") return target.url;
      if (target?.kind === "local") return options.resolveAsset(target.path);
      // Remote images are not fetched, as in the readers.
      return null;
    },
    openLink(reference) {
      const target = classifyReference(options.documentPath, reference);
      if (!target) {
        console.warn(`Link has no destination to open: ${JSON.stringify(reference)}`);
        return;
      }
      options.openLink(target.kind === "local" ? target.path : target.url);
    },
  };

  const field = StateField.define<PreviewState>({
    create: (state) => buildPreview(state, host),
    update(value, transaction) {
      if (
        transaction.docChanged
        || transaction.selection
        || syntaxTree(transaction.startState) !== syntaxTree(transaction.state)
      ) {
        return buildPreview(transaction.state, host);
      }
      return value;
    },
    provide: (self) => EditorView.decorations.from(self, (value) => value.decorations),
  });

  return [
    markdown({ base: markdownLanguage, extensions: [mathSyntax], codeLanguages: codeLanguage }),
    field,
    previewTheme(),
    Prec.high(keymap.of([
      { key: "ArrowDown", run: stepIntoBlock(field, true) },
      { key: "ArrowUp", run: stepIntoBlock(field, false) },
    ])),
    EditorView.domEventHandlers({
      mousedown(event) {
        if (!(event.metaKey || event.ctrlKey) || event.button !== 0) return false;
        const target = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-href]") : null;
        if (!target) return false;
        event.preventDefault();
        host.openLink(target.dataset.href ?? "");
        return true;
      },
    }),
  ];
}
