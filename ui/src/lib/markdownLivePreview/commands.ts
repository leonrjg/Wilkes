import { syntaxTree } from "@codemirror/language";
import { countColumn, EditorSelection, type ChangeSpec, type EditorState, type Line, type StateField } from "@codemirror/state";
import type { Command } from "@codemirror/view";
import type { SyntaxNode } from "@lezer/common";
import type { PreviewState } from "./decorations";

/** Up and Down over what the preview draws rather than over source lines.
 *  A blank line drawn as the margin between two blocks is passed over, as the
 *  gap it looks like: landing on it would open it into a line and shift the
 *  text below at every paragraph on the way through a document. (Left and
 *  Right, and the mouse, still reach it.) A construct drawn as a block widget
 *  is stepped into, which reveals its source; the cursor would otherwise move
 *  straight past it. */
export function moveByLine(field: StateField<PreviewState>, forward: boolean): Command {
  return (view) => {
    const { state } = view;
    const { doc } = state;
    const main = state.selection.main;
    if (state.selection.ranges.length > 1 || !main.empty) return false;
    const line = doc.lineAt(main.head);
    const moved = view.moveVertically(main, forward);
    // Another row of the same wrapped line is the ordinary motion.
    if (moved.head !== main.head && doc.lineAt(moved.head).number === line.number) return false;

    const { margins, blocks } = state.field(field);
    const step = forward ? 1 : -1;
    let neighbour = line.number + step;
    while (neighbour >= 1 && neighbour <= doc.lines && margins.has(doc.line(neighbour).from)) neighbour += step;
    // A margin with nothing beyond it ends the document: the place to go on
    // writing, so the ordinary motion lands on it.
    if (neighbour < 1 || neighbour > doc.lines) return false;

    const target = doc.line(neighbour);
    const block = blocks.find((span) => (forward ? span.from === target.from : span.to === target.to));
    if (block) {
      const anchor = forward ? block.from : doc.lineAt(block.to).from;
      view.dispatch({ selection: { anchor }, scrollIntoView: true, userEvent: "select" });
      return true;
    }
    if (neighbour === line.number + step) return false;

    // Past the margins by the same measure as any other step, so the cursor
    // keeps its column.
    let range = moved;
    while (margins.has(doc.lineAt(range.head).from)) {
      const next = view.moveVertically(range, forward);
      if (next.head === range.head) return false;
      range = next;
    }
    view.dispatch({ selection: EditorSelection.create([range]), scrollIntoView: true, userEvent: "select" });
    return true;
  };
}

/** CommonMark's tab stop, which is what list nesting is measured in. */
const TAB_STOP = 4;

function leadingSpace(line: Line): string {
  return /^[ \t]*/.exec(line.text)![0];
}

function columnOf(state: EditorState, position: number): number {
  const line = state.doc.lineAt(position);
  return countColumn(line.text, TAB_STOP, position - line.from);
}

function ancestor(node: SyntaxNode | null, names: ReadonlySet<string>): SyntaxNode | null {
  for (let current = node; current; current = current.parent) {
    if (names.has(current.name)) return current;
  }
  return null;
}

const LIST_ITEM = new Set(["ListItem"]);
const CODE = new Set(["FencedCode", "CodeBlock"]);

/** The non-blank lines the selection touches. */
function selectedLines(state: EditorState): Line[] {
  const lines = new Map<number, Line>();
  for (const range of state.selection.ranges) {
    const last = state.doc.lineAt(range.to).number;
    for (let number = state.doc.lineAt(range.from).number; number <= last; number++) {
      const line = state.doc.line(number);
      if (line.text.trim() !== "") lines.set(number, line);
    }
  }
  return [...lines.values()];
}

/** Tab and Shift-Tab in Markdown. In a list they nest an item under the one
 *  before it, or lift it out to its parent's level, moving everything the
 *  item holds with it; the indentation is the parent's content column, which
 *  is what makes Markdown read it as nested (two columns under `- `, three
 *  under `1. `). In code they are left to the editor's own indenting. In
 *  prose they do nothing, and are still taken: Tab there would otherwise
 *  move focus out of the editor, and indenting a paragraph turns it into a
 *  code block. */
export function indentListItem(forward: boolean): Command {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const tree = syntaxTree(state);
    const lines = selectedLines(state);
    const nodes = lines.map((line) => tree.resolveInner(line.from + leadingSpace(line).length, 1));
    const found = nodes.map((node) => ancestor(node, LIST_ITEM)).filter((item): item is SyntaxNode => item !== null);
    if (found.length === 0) {
      return !(lines.length > 0 && nodes.every((node) => ancestor(node, CODE) !== null));
    }

    // An item inside another selected item moves with it.
    const starts = new Set(found.map((item) => item.from));
    const items = new Map<number, SyntaxNode>();
    for (const item of found) {
      let nested = false;
      for (let outer = ancestor(item.parent, LIST_ITEM); outer; outer = ancestor(outer.parent, LIST_ITEM)) {
        if (starts.has(outer.from)) nested = true;
      }
      if (!nested) items.set(item.from, item);
    }

    const changes: ChangeSpec[] = [];
    for (const item of items.values()) {
      const column = columnOf(state, item.from);
      let delta: number;
      if (forward) {
        const previous = item.prevSibling;
        const mark = previous?.name === "ListItem" ? previous.getChild("ListMark") : null;
        // The first item of a list has nothing to nest under.
        if (!mark) continue;
        delta = columnOf(state, mark.to) + 1 - column;
      } else {
        const parent = ancestor(item.parent, LIST_ITEM);
        if (!parent) continue;
        delta = columnOf(state, parent.from) - column;
      }
      if (delta === 0) continue;
      const last = state.doc.lineAt(item.to).number;
      for (let number = state.doc.lineAt(item.from).number; number <= last; number++) {
        const line = state.doc.line(number);
        if (line.text.trim() === "") continue;
        const space = leadingSpace(line);
        const width = Math.max(0, countColumn(space, TAB_STOP) + delta);
        changes.push({ from: line.from, to: line.from + space.length, insert: " ".repeat(width) });
      }
    }
    if (changes.length > 0) view.dispatch({ changes, scrollIntoView: true, userEvent: forward ? "input.indent" : "delete.dedent" });
    return true;
  };
}

function enclosing(state: EditorState, name: string, from: number, to: number): SyntaxNode | null {
  const tree = syntaxTree(state);
  for (const side of [1, -1] as const) {
    for (let node: SyntaxNode | null = tree.resolveInner(from, side); node; node = node.parent) {
      if (node.name === name && node.from <= from && node.to >= to) return node;
    }
  }
  return null;
}

/** How many times `character` repeats on each side of `position`: the
 *  shorter of the two runs. */
function runAround(state: EditorState, position: number, character: string): number {
  let before = 0;
  while (before < position && state.sliceDoc(position - before - 1, position - before) === character) before++;
  let after = 0;
  while (state.sliceDoc(position + after, position + after + 1) === character) after++;
  return Math.min(before, after);
}

/** Put `marker` around each selection, or take it off where the selection is
 *  already inside that construct. With nothing selected the markers go in
 *  empty, with the cursor between them, and the same key takes them out. */
export function toggleInline(marker: string, nodeName: string, markName: string): Command {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const size = marker.length;
    const edit = state.changeByRange((range) => {
      const node = enclosing(state, nodeName, range.from, range.to);
      const marks = node?.getChildren(markName) ?? [];
      if (marks.length >= 2) {
        const open = marks[0];
        const close = marks[marks.length - 1];
        const changes = state.changes([{ from: open.from, to: open.to }, { from: close.from, to: close.to }]);
        return { changes, range: EditorSelection.range(changes.mapPos(range.anchor), changes.mapPos(range.head)) };
      }

      const selected = state.sliceDoc(range.from, range.to);
      const from = range.from + (selected.length - selected.trimStart().length);
      const to = Math.max(from, range.to - (selected.length - selected.trimEnd().length));
      if (from === to) {
        // Empty markers around the cursor are told by the run of the marker's
        // character on both sides, since `**` is not two `*`: an odd run
        // holds an emphasis, a run of two or more a strong emphasis.
        const run = runAround(state, from, marker[0]);
        if (size === 1 ? run % 2 === 1 : run >= size) {
          return {
            changes: { from: from - size, to: from + size },
            range: EditorSelection.cursor(from - size),
          };
        }
        return { changes: { from, insert: marker + marker }, range: EditorSelection.cursor(from + size) };
      }
      const reversed = range.head < range.anchor;
      return {
        changes: [{ from, insert: marker }, { from: to, insert: marker }],
        range: EditorSelection.range(reversed ? to + size : from + size, reversed ? from + size : to + size),
      };
    });
    view.dispatch(state.update(edit, { scrollIntoView: true, userEvent: "input" }));
    return true;
  };
}

/** Make the selection a link: `[selection](│)`, the cursor where the address
 *  goes. A selected web address becomes the destination instead, with the
 *  cursor where the text goes. */
export const insertLink: Command = (view) => {
  const { state } = view;
  if (state.readOnly) return false;
  const edit = state.changeByRange((range) => {
    const selected = state.sliceDoc(range.from, range.to);
    if (/^https?:\/\/\S+$/.test(selected)) {
      return {
        changes: [{ from: range.from, insert: "[](" }, { from: range.to, insert: ")" }],
        range: EditorSelection.cursor(range.from + 1),
      };
    }
    return {
      changes: [{ from: range.from, insert: "[" }, { from: range.to, insert: "]()" }],
      range: EditorSelection.cursor(range.to + 3),
    };
  });
  view.dispatch(state.update(edit, { scrollIntoView: true, userEvent: "input" }));
  return true;
};
