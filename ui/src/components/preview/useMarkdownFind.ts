import { useEffect } from "react";

// `CSS.highlights` is a document-global registry, so these names are shared
// with every other script on the page. They are the reader's, not any one
// application's -- an app name here would travel into every host that mounts
// the reader and collide with whatever else that host registers.
const ALL_HIGHLIGHT = "reader-find";
const ACTIVE_HIGHLIGHT = "reader-find-active";
// Block containers whose boundaries should break a match: "end.Start" across two
// paragraphs must not read as the contiguous word "end.Start".
const BLOCK_SELECTOR = "p,li,h1,h2,h3,h4,h5,h6,pre,blockquote,td,th,figcaption,dt,dd,div";

interface TextPiece {
  node: Text;
  start: number;
}

/** Flatten the rendered text nodes into one string, inserting a break between
 *  separate block elements so matches cannot span block boundaries. */
function collectText(root: HTMLElement): { text: string; pieces: TextPiece[] } {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const pieces: TextPiece[] = [];
  let text = "";
  let previousBlock: Element | null = null;
  let seenAny = false;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const textNode = node as Text;
    if (!textNode.nodeValue) continue;
    const block = textNode.parentElement?.closest(BLOCK_SELECTOR) ?? null;
    if (seenAny && block !== previousBlock) text += "\n";
    pieces.push({ node: textNode, start: text.length });
    text += textNode.nodeValue;
    previousBlock = block;
    seenAny = true;
  }
  return { text, pieces };
}

/** Resolve a global offset in the flattened text back to a DOM (node, offset). */
function pointFor(pieces: TextPiece[], offset: number): { node: Text; offset: number } | null {
  for (const piece of pieces) {
    const length = piece.node.nodeValue?.length ?? 0;
    if (offset >= piece.start && offset <= piece.start + length) {
      return { node: piece.node, offset: offset - piece.start };
    }
  }
  return null;
}

/** Locate every case-insensitive occurrence of `query` in the rendered text and
 *  return a DOM Range per match. Exported for testing without the Highlight API. */
export function findMatchRanges(root: HTMLElement, query: string): Range[] {
  const { text, pieces } = collectText(root);
  const haystack = text.toLowerCase();
  const needle = query.toLowerCase();
  const ranges: Range[] = [];
  for (let from = haystack.indexOf(needle); from !== -1; from = haystack.indexOf(needle, from + needle.length)) {
    const start = pointFor(pieces, from);
    const end = pointFor(pieces, from + needle.length);
    if (!start || !end) continue;
    const range = document.createRange();
    range.setStart(start.node, start.offset);
    range.setEnd(end.node, end.offset);
    ranges.push(range);
  }
  return ranges;
}

// Two persistent Highlight objects, registered once and reused. WebKit repaints
// when the range set of a Highlight it already tracks changes, but not reliably
// when a registry entry is deleted -- so erasing means emptying these objects'
// ranges, never removing them from the registry.
let allHighlight: Highlight | null = null;
let activeHighlight: Highlight | null = null;

function ensureHighlights(): boolean {
  if (typeof Highlight === "undefined" || typeof CSS === "undefined" || !CSS.highlights) return false;
  if (!allHighlight) {
    allHighlight = new Highlight();
    CSS.highlights.set(ALL_HIGHLIGHT, allHighlight);
  }
  if (!activeHighlight) {
    activeHighlight = new Highlight();
    activeHighlight.priority = 1; // drawn on top of the base match highlight
    CSS.highlights.set(ACTIVE_HIGHLIGHT, activeHighlight);
  }
  return true;
}

function clearHighlights() {
  allHighlight?.clear();
  activeHighlight?.clear();
}

interface Options {
  rootRef: React.RefObject<HTMLElement | null>;
  content: string;
  query: string;
  isOpen: boolean;
  currentIdx: number;
  onMatchCount: (count: number) => void;
}

/**
 * In-document find for the rendered Markdown viewer. Matches are painted with
 * the CSS Custom Highlight API rather than by wrapping DOM nodes, so React keeps
 * sole ownership of the tree and the source-map spans are left untouched.
 */
export function useMarkdownFind({ rootRef, content, query, isOpen, currentIdx, onMatchCount }: Options) {
  useEffect(() => {
    if (!ensureHighlights()) {
      if (isOpen && query.trim()) {
        console.warn("CSS Custom Highlight API unavailable; Markdown find is disabled.");
      }
      onMatchCount(0);
      return;
    }

    const root = rootRef.current;
    const trimmed = query.trim();
    if (!root || !isOpen || !trimmed) {
      clearHighlights();
      onMatchCount(0);
      return;
    }

    const ranges = findMatchRanges(root, trimmed);
    onMatchCount(ranges.length);

    clearHighlights();
    for (const range of ranges) allHighlight!.add(range);
    const active = ranges[currentIdx];
    if (active) {
      activeHighlight!.add(active);
      (active.startContainer.parentElement ?? root).scrollIntoView?.({ block: "center" });
    }
  }, [rootRef, content, query, isOpen, currentIdx, onMatchCount]);

  useEffect(() => clearHighlights, []);
}
