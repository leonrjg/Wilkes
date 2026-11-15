import { syntaxTree } from "@codemirror/language";
import { RangeSet, type EditorState, type Range } from "@codemirror/state";
import { Decoration, GutterMarker, type DecorationSet, type WidgetType } from "@codemirror/view";
import type { SyntaxNode } from "@lezer/common";
import { mathSource } from "./mathSyntax";
import {
  BulletWidget,
  CheckboxWidget,
  ImageWidget,
  MathWidget,
  MermaidWidget,
  RuleWidget,
  TableWidget,
  type InlineNode,
  type PreviewHost,
  type TableCell,
  type TableModel,
} from "./widgets";

export interface Span {
  from: number;
  to: number;
}

/** Gives a line's gutter element the classes of the line itself, so the
 *  line numbers can follow the heights the preview gives its lines. */
class LineClassMarker extends GutterMarker {
  constructor(readonly elementClass: string) {
    super();
  }

  eq(other: LineClassMarker) {
    return other.elementClass === this.elementClass;
  }
}

export interface PreviewState {
  decorations: DecorationSet;
  /** The preview's line classes again, for the gutter elements beside them. */
  gutterClasses: RangeSet<GutterMarker>;
  /** Line spans replaced by a block widget, for keyboard navigation into them. */
  blocks: readonly Span[];
}

const INLINE_MARKS = new Set([
  "EmphasisMark",
  "StrikethroughMark",
  "CodeMark",
  "LinkMark",
  "InlineMathMark",
  "URL",
  "LinkTitle",
  "LinkLabel",
]);

function overlaps(spans: readonly Span[], from: number, to: number): boolean {
  return spans.some((span) => span.from <= to && span.to >= from);
}

/** The inline content of `parent` between `from` and `to`, as the value-typed
 *  tree a table cell renders from. */
function inlineContent(state: EditorState, parent: SyntaxNode, from: number, to: number): InlineNode[] {
  const nodes: InlineNode[] = [];
  const text = (start: number, end: number) => {
    if (end <= start) return;
    const value = state.sliceDoc(start, end);
    const last = nodes[nodes.length - 1];
    if (last?.kind === "text") last.text += value;
    else nodes.push({ kind: "text", text: value });
  };
  let position = from;
  for (let child = parent.firstChild; child; child = child.nextSibling) {
    if (child.to <= from || child.from >= to) continue;
    text(position, child.from);
    position = child.to;
    const name = child.name;
    if (INLINE_MARKS.has(name)) continue;
    switch (name) {
      case "TableCell":
        nodes.push(...inlineContent(state, child, Math.max(child.from, from), Math.min(child.to, to)));
        break;
      case "Emphasis":
        nodes.push({ kind: "em", children: inlineContent(state, child, child.from, child.to) });
        break;
      case "StrongEmphasis":
        nodes.push({ kind: "strong", children: inlineContent(state, child, child.from, child.to) });
        break;
      case "Strikethrough":
        nodes.push({ kind: "del", children: inlineContent(state, child, child.from, child.to) });
        break;
      case "InlineCode": {
        const marks = child.getChildren("CodeMark");
        const start = marks[0]?.to ?? child.from;
        const end = marks.length > 1 ? marks[marks.length - 1].from : child.to;
        nodes.push({ kind: "code", text: state.sliceDoc(start, end).trim() });
        break;
      }
      case "InlineMath":
        nodes.push({ kind: "math", tex: mathSource(state.sliceDoc(child.from, child.to)).tex });
        break;
      case "Link": {
        const marks = child.getChildren("LinkMark");
        const url = child.getChild("URL");
        if (marks.length < 2) {
          text(child.from, child.to);
          break;
        }
        const label = inlineContent(state, child, marks[0].to, marks[1].from);
        nodes.push(url
          ? { kind: "link", href: state.sliceDoc(url.from, url.to), children: label }
          : { kind: "text", text: state.sliceDoc(child.from, child.to) });
        break;
      }
      case "Image": {
        const marks = child.getChildren("LinkMark");
        const url = child.getChild("URL");
        if (!url || marks.length < 2) {
          text(child.from, child.to);
          break;
        }
        nodes.push({
          kind: "image",
          src: state.sliceDoc(url.from, url.to),
          alt: state.sliceDoc(marks[0].to, marks[1].from),
        });
        break;
      }
      case "Autolink":
      case "URL": {
        const url = name === "URL" ? child : child.getChild("URL");
        const href = url ? state.sliceDoc(url.from, url.to) : state.sliceDoc(child.from, child.to);
        nodes.push({ kind: "link", href, children: [{ kind: "text", text: href }] });
        break;
      }
      case "Escape":
        text(child.from + 1, child.to);
        break;
      default:
        text(child.from, child.to);
    }
  }
  text(position, to);
  return nodes;
}

/** Cells of a header or body row: the text between its pipes, keeping empty
 *  cells (which have no TableCell node) in their column. */
function rowCells(state: EditorState, row: SyntaxNode, tableStart: number): TableCell[] {
  const pipes = row.getChildren("TableDelimiter");
  const cells: TableCell[] = [];
  const boundaries = [...pipes.map((pipe) => ({ from: pipe.from, to: pipe.to })), { from: row.to, to: row.to }];
  let start = row.from;
  boundaries.forEach((boundary, index) => {
    const raw = state.sliceDoc(start, boundary.from);
    const leadingPipe = index === 0 && pipes.length > 0 && raw.trim() === "";
    const trailingPipe = index === boundaries.length - 1 && pipes.length > 0 && raw.trim() === "";
    if (!leadingPipe && !trailingPipe) {
      const contentFrom = start + (raw.length - raw.trimStart().length);
      const contentTo = boundary.from - (raw.length - raw.trimEnd().length);
      cells.push({
        offset: contentFrom - tableStart,
        content: inlineContent(state, row, contentFrom, Math.max(contentFrom, contentTo)),
      });
    }
    start = boundary.to;
  });
  return cells;
}

function tableModel(state: EditorState, table: SyntaxNode, tableStart: number): TableModel | null {
  const header = table.getChild("TableHeader");
  const delimiter = table.getChildren("TableDelimiter")[0];
  if (!header || !delimiter) return null;
  const align = state.sliceDoc(delimiter.from, delimiter.to)
    .split("|")
    .map((cell) => cell.trim())
    .filter(Boolean)
    .map((cell) => {
      const left = cell.startsWith(":");
      const right = cell.endsWith(":");
      return left && right ? "center" : right ? "right" : left ? "left" : null;
    });
  const width = align.length;
  const fit = (cells: TableCell[]) => cells.slice(0, width);
  return {
    align,
    header: fit(rowCells(state, header, tableStart)),
    rows: table.getChildren("TableRow").map((row) => fit(rowCells(state, row, tableStart))),
  };
}

function inside(node: SyntaxNode, name: string): boolean {
  for (let parent = node.parent; parent; parent = parent.parent) {
    if (parent.name === name) return true;
  }
  return false;
}

/** Headings the reader sets apart with margins of their own; `h5` and `h6`
 *  read as body text there. */
const SPACED_HEADING = /^(?:ATX|Setext)Heading[1-4]$/;

/** Constructs whose blank lines are content, not the space between blocks. */
const VERBATIM_BLOCKS = new Set(["FencedCode", "CodeBlock", "BlockMath"]);

/** Compute every decoration of the live preview from the syntax tree and the
 *  selection. Syntax shows as source wherever the selection is, line-wise for
 *  inline markup and construct-wise for blocks drawn as widgets. */
export function buildPreview(state: EditorState, host: PreviewHost): PreviewState {
  const { doc } = state;
  const selection = state.selection.ranges.map((range) => ({ from: range.from, to: range.to }));
  const selectedLines = selection.map((range) => ({
    from: doc.lineAt(range.from).from,
    to: doc.lineAt(range.to).to,
  }));
  const lineActive = (from: number, to: number) => overlaps(selectedLines, from, to);
  const ranges: Range<Decoration>[] = [];
  const gutterRanges: Range<GutterMarker>[] = [];
  const blocks: Span[] = [];
  /** Class one line, and its gutter element with it. */
  const classLine = (position: number, className: string) => {
    ranges.push(Decoration.line({ class: className }).range(position));
    gutterRanges.push(new LineClassMarker(className).range(position));
  };
  /** The headings the reader spaces: first line number to last (a setext
   *  heading's underline is its last line). */
  const spacedHeadings = new Map<number, number>();
  /** Line numbers inside verbatim blocks. */
  const verbatimLines = new Set<number>();
  const isBlank = (number: number) => /^\s*$/.test(doc.line(number).text);

  const slice = (node: { from: number; to: number }) => state.sliceDoc(node.from, node.to);
  const spaceAfter = (position: number) => (doc.sliceString(position, position + 1) === " " ? position + 1 : position);
  const spaceBefore = (position: number) => {
    let start = position;
    while (start > 0 && doc.sliceString(start - 1, start) === " ") start--;
    return start;
  };
  const hide = (from: number, to: number) => {
    if (to > from) ranges.push(Decoration.replace({}).range(from, to));
  };
  const replaceWith = (from: number, to: number, widget: WidgetType) => {
    ranges.push(Decoration.replace({ widget }).range(from, to));
  };
  const mark = (from: number, to: number, className: string, attributes?: Record<string, string>) => {
    if (to > from) ranges.push(Decoration.mark({ class: className, attributes }).range(from, to));
  };
  const lineClass = (from: number, to: number, className: string) => {
    const last = doc.lineAt(to).number;
    for (let number = doc.lineAt(from).number; number <= last; number++) classLine(doc.line(number).from, className);
  };
  const lineSpan = (node: SyntaxNode): Span => ({ from: doc.lineAt(node.from).from, to: doc.lineAt(node.to).to });
  // A block widget replaces whole lines, so it is only drawn where the
  // construct owns its lines outright: not inside a blockquote, whose `>`
  // markers it would swallow, and not sharing a line with other text.
  const ownsLines = (node: SyntaxNode, span: Span) =>
    !inside(node, "Blockquote")
    && doc.sliceString(span.from, node.from).trim() === ""
    && doc.sliceString(node.to, span.to).trim() === "";
  const blockWidget = (span: Span, widget: WidgetType) => {
    ranges.push(Decoration.replace({ widget, block: true }).range(span.from, span.to));
    blocks.push(span);
  };
  const hideMarks = (node: SyntaxNode, markName: string) => {
    if (lineActive(node.from, node.to)) return;
    for (const child of node.getChildren(markName)) hide(child.from, child.to);
  };

  syntaxTree(state).iterate({
    enter: (ref) => {
      const node = ref.node;
      const name = ref.name;
      if (SPACED_HEADING.test(name)) spacedHeadings.set(doc.lineAt(node.from).number, doc.lineAt(node.to).number);
      if (VERBATIM_BLOCKS.has(name)) {
        const last = doc.lineAt(node.to).number;
        for (let number = doc.lineAt(node.from).number; number <= last; number++) verbatimLines.add(number);
      }
      const heading = /^ATXHeading(\d)$/.exec(name);
      if (heading) {
        lineClass(node.from, node.from, `cm-md-heading cm-md-h${heading[1]}`);
        if (!lineActive(node.from, node.to)) {
          node.getChildren("HeaderMark").forEach((headerMark, index) => {
            if (index === 0 && headerMark.from === node.from) hide(headerMark.from, spaceAfter(headerMark.to));
            else hide(spaceBefore(headerMark.from), headerMark.to);
          });
        }
        return;
      }
      const setext = /^SetextHeading(\d)$/.exec(name);
      if (setext) {
        const underline = node.getChild("HeaderMark");
        const textEnd = underline ? Math.max(node.from, doc.lineAt(underline.from).from - 1) : node.to;
        lineClass(node.from, textEnd, `cm-md-heading cm-md-h${setext[1]}`);
        if (underline && !lineActive(node.from, node.to)) {
          hide(underline.from, underline.to);
          lineClass(underline.from, underline.from, "cm-md-gap");
        }
        return;
      }
      switch (name) {
        case "Emphasis":
          mark(node.from, node.to, "cm-md-em");
          hideMarks(node, "EmphasisMark");
          return;
        case "StrongEmphasis":
          mark(node.from, node.to, "cm-md-strong");
          hideMarks(node, "EmphasisMark");
          return;
        case "Strikethrough":
          mark(node.from, node.to, "cm-md-strike");
          hideMarks(node, "StrikethroughMark");
          return;
        case "InlineCode":
          mark(node.from, node.to, "cm-md-inline-code");
          hideMarks(node, "CodeMark");
          return false;
        case "Escape":
          if (!lineActive(node.from, node.to)) hide(node.from, node.from + 1);
          return false;
        case "Link": {
          const marks = node.getChildren("LinkMark");
          if (marks.length < 2) return;
          const url = node.getChild("URL");
          mark(marks[0].to, marks[1].from, "cm-md-link", url ? { "data-href": slice(url) } : undefined);
          if (!lineActive(node.from, node.to)) {
            hide(node.from, marks[0].to);
            hide(marks[1].from, node.to);
          }
          return;
        }
        case "Autolink": {
          const url = node.getChild("URL");
          if (url) mark(url.from, url.to, "cm-md-link", { "data-href": slice(url) });
          hideMarks(node, "LinkMark");
          return false;
        }
        case "URL": {
          const parent = node.parent?.name;
          if (parent !== "Link" && parent !== "Image" && parent !== "Autolink" && parent !== "LinkReference") {
            mark(node.from, node.to, "cm-md-link", { "data-href": slice(node) });
          }
          return false;
        }
        case "Image": {
          const marks = node.getChildren("LinkMark");
          const url = node.getChild("URL");
          if (url && marks.length >= 2 && !lineActive(node.from, node.to)) {
            replaceWith(node.from, node.to, new ImageWidget(slice(url), state.sliceDoc(marks[0].to, marks[1].from), host));
          }
          return false;
        }
        case "ListItem": {
          // The reader parts tight list items by a little; a loose list's
          // blank lines already part them.
          const line = doc.lineAt(node.from).number;
          if (node.prevSibling?.name === "ListItem" && line > 1 && !isBlank(line - 1)) {
            lineClass(node.from, node.from, "cm-md-item-next");
          }
          return;
        }
        case "Blockquote":
          lineClass(node.from, node.to, "cm-md-quote");
          return;
        case "QuoteMark":
          if (!lineActive(node.from, node.to)) hide(node.from, spaceAfter(node.to));
          return false;
        case "ListMark": {
          const item = node.parent;
          if (!item || lineActive(node.from, node.to)) return false;
          if (item.getChild("Task")) hide(node.from, spaceAfter(node.to));
          else if (item.parent?.name === "BulletList") replaceWith(node.from, node.to, new BulletWidget());
          else mark(node.from, node.to, "cm-md-list-number");
          return false;
        }
        case "TaskMarker": {
          const checked = /x/i.test(slice(node));
          if (checked && node.parent) mark(spaceAfter(node.to), node.parent.to, "cm-md-task-done");
          if (!lineActive(node.from, node.to)) replaceWith(node.from, node.to, new CheckboxWidget(checked));
          return false;
        }
        case "HorizontalRule":
          if (!lineActive(node.from, node.to)) replaceWith(node.from, node.to, new RuleWidget());
          return false;
        case "FencedCode": {
          const span = lineSpan(node);
          const active = overlaps(selection, span.from, span.to);
          const info = node.getChild("CodeInfo");
          const language = info ? slice(info).trim() : "";
          const marks = node.getChildren("CodeMark");
          const openLine = doc.lineAt(node.from);
          const close = marks.length > 1 ? marks[marks.length - 1] : null;
          const closeLine = close ? doc.lineAt(close.from) : null;
          if (language.toLowerCase() === "mermaid" && closeLine && closeLine.number > openLine.number && ownsLines(node, span)) {
            const code = closeLine.number > openLine.number + 1 ? doc.sliceString(openLine.to + 1, closeLine.from - 1) : "";
            if (!active) {
              blockWidget(span, new MermaidWidget(code, host.dark));
              return false;
            }
            ranges.push(Decoration.widget({ widget: new MermaidWidget(code, host.dark, true), block: true, side: 1 }).range(span.to));
          }
          lineClass(node.from, node.to, "cm-md-codeblock");
          lineClass(node.from, node.from, "cm-md-codeblock-first");
          lineClass(node.to, node.to, "cm-md-codeblock-last");
          if (!active) {
            // Off the cursor the fences leave only the block's padding, as
            // the reader draws it.
            hide(node.from, openLine.to);
            lineClass(node.from, node.from, "cm-md-fence");
            if (close && closeLine && closeLine.number > openLine.number) {
              hide(close.from, closeLine.to);
              lineClass(close.from, close.from, "cm-md-fence");
            }
          }
          return false;
        }
        case "CodeBlock":
          lineClass(node.from, node.to, "cm-md-codeblock");
          lineClass(node.from, node.from, "cm-md-codeblock-first");
          lineClass(node.to, node.to, "cm-md-codeblock-last");
          return false;
        case "Table": {
          const span = lineSpan(node);
          if (!overlaps(selection, span.from, span.to) && ownsLines(node, span)) {
            const model = tableModel(state, node, span.from);
            if (model) {
              blockWidget(span, new TableWidget(model, host));
              return false;
            }
          }
          lineClass(node.from, node.to, "cm-md-table-source");
          return false;
        }
        case "BlockMath": {
          const span = lineSpan(node);
          const marks = node.getChildren("BlockMathMark");
          const closed = marks.length > 1;
          const tex = state.sliceDoc(marks[0]?.to ?? node.from, closed ? marks[marks.length - 1].from : node.to).trim();
          const drawable = closed && ownsLines(node, span);
          if (drawable && !overlaps(selection, span.from, span.to)) {
            blockWidget(span, new MathWidget(tex, true));
            return false;
          }
          lineClass(node.from, node.to, "cm-md-math-source");
          if (drawable) {
            ranges.push(Decoration.widget({ widget: new MathWidget(tex, true, true), block: true, side: 1 }).range(span.to));
          }
          return false;
        }
        case "InlineMath":
          if (lineActive(node.from, node.to)) mark(node.from, node.to, "cm-md-math-source");
          else replaceWith(node.from, node.to, new MathWidget(mathSource(slice(node)).tex, false));
          return false;
      }
      return;
    },
  });

  blockSpacing(state, isBlank, spacedHeadings, verbatimLines, classLine);
  return { decorations: Decoration.set(ranges, true), gutterClasses: RangeSet.of(gutterRanges, true), blocks };
}

/** The reader's vertical rhythm. A run of blank lines between two blocks is
 *  the margin the reader puts between them, not a line of text: it shrinks to
 *  that margin (taller before a heading, whose top margin is larger) and any
 *  further blank lines in the run to nothing. A blank line holding the caret
 *  keeps its full height, so there is a line to type on. A heading that
 *  touches a neighbour without a blank line keeps its own margin on that side. */
function blockSpacing(
  state: EditorState,
  isBlank: (number: number) => boolean,
  spacedHeadings: ReadonlyMap<number, number>,
  verbatimLines: ReadonlySet<number>,
  classLine: (position: number, className: string) => void,
) {
  const { doc } = state;
  const caretLines = new Set(
    state.selection.ranges.filter((range) => range.empty).map((range) => doc.lineAt(range.head).number),
  );
  const gap = (number: number) => isBlank(number) && !verbatimLines.has(number);
  const lineClass = (number: number, className: string) => classLine(doc.line(number).from, className);

  for (const [first, last] of spacedHeadings) {
    if (first > 1 && !isBlank(first - 1)) lineClass(first, "cm-md-space-above");
    if (last < doc.lines && !isBlank(last + 1)) lineClass(first, "cm-md-space-below");
  }

  let number = 1;
  while (number <= doc.lines) {
    if (!gap(number)) {
      number++;
      continue;
    }
    let end = number;
    while (end < doc.lines && gap(end + 1)) end++;
    // Before the first block and after the last there is no margin at all.
    const between = number > 1 && end < doc.lines;
    const size = spacedHeadings.has(end + 1) ? "cm-md-gap-heading" : "cm-md-gap-block";
    for (let line = number; line <= end; line++) {
      if (caretLines.has(line)) continue;
      lineClass(line, between && line === number ? `cm-md-gap ${size}` : "cm-md-gap");
    }
    number = end + 1;
  }
}
