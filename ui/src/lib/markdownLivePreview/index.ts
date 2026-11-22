import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { LanguageSupport, syntaxTree } from "@codemirror/language";
import { Prec, StateField, type Extension } from "@codemirror/state";
import { EditorView, gutterLineClass, keymap } from "@codemirror/view";
import { getLanguageExtension } from "@leonrjg/wilkes-reader";
import { indentListItem, insertLink, moveByLine, toggleInline } from "./commands";
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

/** The reader's monospace stack, for inline code and code blocks. */
const MONO = 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace';

/** The reader's page around `.prose .prose-document`: `px-6 py-5` padding
 *  and a 90rem measure. `.cm-content` is border-box, so the measure is
 *  widened by the padding. */
const PAGE_PADDING_X = "24px";
const PAGE_PADDING_Y = "20px";
const MEASURE = `calc(90rem + 2 * ${PAGE_PADDING_X})`;

/** Margins the reader puts around a block and around a heading. */
const BLOCK_MARGIN = "0.55rem";
const HEADING_MARGIN_TOP = "0.7rem";
const HEADING_MARGIN_BOTTOM = "0.35rem";

const BORDER = "1px solid var(--border-main)";

/** Height of a line of body text, and of a code line, which the line numbers
 *  take so they sit level with the text beside them. */
const TEXT_LINE = "calc(16px * 1.65)";
const CODE_LINE = "calc(0.92 * 16px * 1.65)";

/** Markdown being edited is laid out the way the reader renders it (the
 *  `.prose` and `.prose-document` rules of wilkes-reader's stylesheet): the
 *  same face, size, measure, headings, block spacing, code and tables, so
 *  switching between reading and editing moves nothing but the source the
 *  caret reveals. */
const previewTheme = () => EditorView.theme({
  // Over the source editor's dark background, the page's own, as the reader.
  "&.cm-editor": { backgroundColor: "var(--bg-app)" },
  ".cm-content": {
    fontFamily: "var(--default-font-family)",
    fontSize: "16px",
    lineHeight: "1.65",
    color: "var(--text-main)",
    maxWidth: MEASURE,
    margin: "0 auto",
    padding: `${PAGE_PADDING_Y} ${PAGE_PADDING_X}`,
  },
  ".cm-line": { padding: "0" },
  // Line numbers, when the setting shows them, on the page rather than in a
  // panel of their own. Each gutter element carries its line's classes (see
  // `gutterClasses`), so a number takes its line's height and padding and is
  // left out where a line is only a margin.
  "&.cm-editor .cm-gutters": {
    backgroundColor: "var(--bg-app)",
    color: "var(--text-dim)",
    borderRight: "none",
  },
  // The line's classes bring its type (a heading's weight and size, a
  // formula's face) with its metrics; the number keeps the gutter's own.
  ".cm-lineNumbers .cm-gutterElement": {
    lineHeight: TEXT_LINE,
    paddingLeft: "12px",
    fontFamily: MONO,
    fontSize: "13px",
    fontWeight: "400",
    color: "var(--text-dim)",
  },
  ".cm-gutterElement.cm-md-gap, .cm-gutterElement.cm-md-fence": { visibility: "hidden" },
  ".cm-gutterElement.cm-md-space-above": { paddingTop: HEADING_MARGIN_TOP },
  ".cm-gutterElement.cm-md-item-next": { paddingTop: "0.2rem" },
  ".cm-gutterElement.cm-md-codeblock": { lineHeight: CODE_LINE },
  ".cm-gutterElement.cm-md-codeblock-first": { paddingTop: "calc(0.6rem + 1px)" },
  ".cm-gutterElement.cm-md-h1": { lineHeight: "calc(1.15 * 16px * 1.25)" },
  ".cm-gutterElement.cm-md-h2": { lineHeight: "calc(1.08 * 16px * 1.25)" },
  ".cm-gutterElement.cm-md-h3, .cm-gutterElement.cm-md-h4": { lineHeight: "calc(16px * 1.25)" },
  // One blank line between two blocks is the margin between them (see
  // `blockSpacing`); a line with no height of its own is all margin.
  ".cm-line.cm-md-gap": { lineHeight: "0" },
  ".cm-line.cm-md-gap-block": { paddingTop: BLOCK_MARGIN },
  ".cm-line.cm-md-gap-heading": { paddingTop: HEADING_MARGIN_TOP },
  ".cm-line.cm-md-space-above": { paddingTop: HEADING_MARGIN_TOP },
  ".cm-line.cm-md-space-below": { paddingBottom: HEADING_MARGIN_BOTTOM },
  // The syntax highlighter colours and underlines the spans inside a line;
  // rendered constructs take the document's own colours instead, so the
  // rules reach those spans.
  ".cm-md-heading, .cm-md-heading *": { color: "var(--text-main)", textDecoration: "none" },
  ".cm-md-h1, .cm-md-h2, .cm-md-h3, .cm-md-h4": { fontWeight: "650", lineHeight: "1.25" },
  ".cm-md-h1": { fontSize: "1.15em" },
  ".cm-md-h2": { fontSize: "1.08em" },
  ".cm-md-em": { fontStyle: "italic" },
  ".cm-md-strong": { fontWeight: "700" },
  ".cm-md-strike": { textDecoration: "line-through" },
  ".cm-md-inline-code": {
    fontFamily: MONO,
    fontSize: "0.92em",
    backgroundColor: "var(--bg-active)",
    border: BORDER,
    borderRadius: "4px",
    padding: "0.05rem 0.25rem",
  },
  ".cm-md-link, .cm-md-link *": { color: "var(--accent-blue)", textDecoration: "underline", textUnderlineOffset: "2px" },
  ".cm-line.cm-md-quote": {
    borderLeft: "3px solid var(--border-strong)",
    paddingLeft: "0.75rem",
    color: "var(--text-muted)",
  },
  ".cm-line.cm-md-item-next": { paddingTop: "0.2rem" },
  ".cm-line.cm-md-codeblock": {
    fontFamily: MONO,
    fontSize: "0.92em",
    backgroundColor: "var(--bg-active)",
    borderLeft: BORDER,
    borderRight: BORDER,
    paddingLeft: "0.7rem",
    paddingRight: "0.7rem",
  },
  ".cm-line.cm-md-codeblock-first": {
    borderTop: BORDER,
    borderTopLeftRadius: "6px",
    borderTopRightRadius: "6px",
    paddingTop: "0.6rem",
  },
  ".cm-line.cm-md-codeblock-last": {
    borderBottom: BORDER,
    borderBottomLeftRadius: "6px",
    borderBottomRightRadius: "6px",
    paddingBottom: "0.6rem",
  },
  // A hidden fence is only the block's padding. CodeMirror's widget buffers
  // around the hidden text are 1em tall, so the em goes too.
  ".cm-line.cm-md-fence": { lineHeight: "0", fontSize: "0" },
  ".cm-line.cm-md-table-source, .cm-line.cm-md-math-source, .cm-md-math-source": { fontFamily: MONO, fontSize: "0.88em" },
  ".cm-md-table-source *": { textDecoration: "none" },
  ".cm-md-table-wrap": { overflowX: "auto", cursor: "text" },
  ".cm-md-table": { borderCollapse: "collapse", width: "100%", tableLayout: "auto" },
  ".cm-md-table th, .cm-md-table td": {
    border: BORDER,
    padding: "0.25rem 0.45rem",
    minWidth: "7rem",
    overflowWrap: "break-word",
    textAlign: "center",
    verticalAlign: "middle",
  },
  ".cm-md-table th": { backgroundColor: "var(--bg-active)", fontWeight: "650" },
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
  // List markers in the text's own colour, as the reader's. A bullet and the
  // source it stands for take the same room.
  ".cm-md-bullet": { display: "inline-block", minWidth: "0.9em" },
  // A list's indentation in the monospace face: two spaces of it are within
  // a pixel of the 1.25rem the reader indents a nested list by.
  ".cm-md-indent": { fontFamily: MONO },
  ".cm-md-list-number, .cm-md-list-number *": { color: "var(--text-main)" },
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
    provide: (self) => [
      EditorView.decorations.from(self, (value) => value.decorations),
      gutterLineClass.from(self, (value) => value.gutterClasses),
    ],
  });

  return [
    markdown({ base: markdownLanguage, extensions: [mathSyntax], codeLanguages: codeLanguage }),
    field,
    previewTheme(),
    Prec.high(keymap.of([
      { key: "ArrowDown", run: moveByLine(field, true) },
      { key: "ArrowUp", run: moveByLine(field, false) },
      { key: "Tab", run: indentListItem(true) },
      { key: "Shift-Tab", run: indentListItem(false) },
      { key: "Mod-b", run: toggleInline("**", "StrongEmphasis", "EmphasisMark") },
      // Over the editor's "select parent syntax", which prose has no use for.
      { key: "Mod-i", run: toggleInline("*", "Emphasis", "EmphasisMark") },
      { key: "Mod-k", run: insertLink },
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
