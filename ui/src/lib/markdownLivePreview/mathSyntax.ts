import type { BlockContext, Element, InlineContext, Line, MarkdownConfig } from "@lezer/markdown";

const DOLLAR = 36;
const BACKSLASH = 92;

function isSpace(code: number): boolean {
  return code === 32 || code === 9 || code === 10 || code === 13;
}

function isDigit(code: number): boolean {
  return code >= 48 && code <= 57;
}

/** `$…$` and `$$…$$` inside a paragraph. Follows Pandoc's rule for the single
 *  dollar so prices do not turn into formulas: the opening `$` must be followed
 *  by a non-space, the closing one preceded by a non-space and not followed by
 *  a digit. `\$` never opens or closes: the Escape parser has consumed it. */
function parseInlineMath(cx: InlineContext, next: number, pos: number): number {
  if (next !== DOLLAR) return -1;
  const width = cx.char(pos + 1) === DOLLAR ? 2 : 1;
  const contentStart = pos + width;
  if (contentStart >= cx.end) return -1;
  if (width === 1 && isSpace(cx.char(contentStart))) return -1;
  for (let i = contentStart; i < cx.end; i++) {
    const code = cx.char(i);
    if (code === BACKSLASH) {
      i++;
      continue;
    }
    if (code !== DOLLAR) continue;
    if (width === 2) {
      if (cx.char(i + 1) !== DOLLAR) continue;
      if (i === contentStart) return -1;
      return cx.addElement(cx.elt("InlineMath", pos, i + 2, [
        cx.elt("InlineMathMark", pos, contentStart),
        cx.elt("InlineMathMark", i, i + 2),
      ]));
    }
    if (i === contentStart || isSpace(cx.char(i - 1)) || isDigit(cx.char(i + 1))) continue;
    return cx.addElement(cx.elt("InlineMath", pos, i + 1, [
      cx.elt("InlineMathMark", pos, contentStart),
      cx.elt("InlineMathMark", i, i + 1),
    ]));
  }
  return -1;
}

/** A display formula that opens with `$$` at the start of a line. It closes on
 *  the first line that ends in `$$` (which may be the opening line itself). An
 *  unclosed one stops at the first blank line -- LaTeX rejects a blank line in
 *  display math anyway -- or the end of its container, so a stray `$$` does not
 *  swallow the rest of the document while it is being typed. */
function parseBlockMath(cx: BlockContext, line: Line): boolean {
  if (line.next !== DOLLAR || line.text.charCodeAt(line.pos + 1) !== DOLLAR) return false;
  const from = cx.lineStart + line.pos;
  const children: Element[] = [cx.elt("BlockMathMark", from, from + 2)];
  const rest = line.text.slice(line.pos + 2).trimEnd();
  if (rest.length >= 2 && rest.endsWith("$$")) {
    const closeAt = cx.lineStart + line.pos + 2 + rest.length - 2;
    children.push(cx.elt("BlockMathMark", closeAt, closeAt + 2));
    cx.nextLine();
    cx.addElement(cx.elt("BlockMath", from, closeAt + 2, children));
    return true;
  }
  let to = cx.lineStart + line.text.length;
  while (cx.nextLine()) {
    if (line.pos >= line.text.length) break;
    children.push(...line.markers);
    to = cx.lineStart + line.text.length;
    const trimmed = line.text.trimEnd();
    if (trimmed.endsWith("$$")) {
      const closeAt = cx.lineStart + trimmed.length - 2;
      children.push(cx.elt("BlockMathMark", closeAt, closeAt + 2));
      cx.nextLine();
      break;
    }
  }
  cx.addElement(cx.elt("BlockMath", from, to, children));
  return true;
}

/** TeX math for the Markdown parser: `InlineMath` and `BlockMath` nodes whose
 *  delimiters are `InlineMathMark` / `BlockMathMark` children. */
export const mathSyntax: MarkdownConfig = {
  defineNodes: [
    { name: "InlineMath" },
    { name: "InlineMathMark" },
    { name: "BlockMath", block: true },
    { name: "BlockMathMark" },
  ],
  parseInline: [{ name: "InlineMath", parse: parseInlineMath, after: "InlineCode" }],
  parseBlock: [{ name: "BlockMath", parse: parseBlockMath, before: "FencedCode" }],
};

/** The TeX source between a math node's delimiters. */
export function mathSource(text: string): { tex: string; display: boolean } {
  const display = text.startsWith("$$");
  const width = display ? 2 : 1;
  const closed = text.length >= width * 2 && text.endsWith(display ? "$$" : "$");
  return { tex: text.slice(width, closed ? text.length - width : text.length).trim(), display };
}
