import type { ByteRange } from "../../lib/types";

export interface TextAnnotation {
  id: string;
  kind: "search" | "bookmark";
  range: ByteRange;
}

interface HastNode {
  type: string;
  tagName?: string;
  value?: string;
  position?: { start: { offset?: number }; end: { offset?: number } };
  properties?: Record<string, unknown>;
  children?: HastNode[];
}

function decodedEntity(source: string): string | null {
  const match = source.match(/^&(?:#x[\da-f]+|#\d+|[a-z][\da-z]+);/i);
  if (!match) return null;
  const textarea = window.document.createElement("textarea");
  textarea.innerHTML = match[0];
  return textarea.value;
}

/**
 * Map every UTF-16 boundary in rendered text back to a UTF-16 source boundary.
 * Markdown punctuation is skipped; escapes and entities are consumed as one
 * visible character. Positions supplied by mdast bound the search to the
 * originating source node, so repeated text elsewhere cannot interfere.
 */
export function renderedBoundaries(source: string, rendered: string, start: number, end: number): number[] {
  const slice = source.slice(start, end);
  const boundaries = new Array<number>(rendered.length + 1).fill(start);
  let sourceOffset = 0;
  let renderedOffset = 0;

  for (const character of rendered) {
    let tokenStart = sourceOffset;
    let tokenEnd = sourceOffset;
    while (tokenStart < slice.length) {
      if (slice.startsWith(character, tokenStart)) {
        tokenEnd = tokenStart + character.length;
        break;
      }
      if (slice[tokenStart] === "\\" && slice.startsWith(character, tokenStart + 1)) {
        tokenEnd = tokenStart + 1 + character.length;
        break;
      }
      const entity = decodedEntity(slice.slice(tokenStart));
      if (entity === character) {
        tokenEnd = tokenStart + slice.slice(tokenStart).match(/^&(?:#x[\da-f]+|#\d+|[a-z][\da-z]+);/i)![0].length;
        break;
      }
      tokenStart += slice.codePointAt(tokenStart)! > 0xffff ? 2 : 1;
    }

    if (tokenEnd === sourceOffset) {
      tokenStart = sourceOffset;
      tokenEnd = Math.min(sourceOffset + character.length, slice.length);
    }
    boundaries[renderedOffset] = start + tokenStart;
    for (let index = 1; index <= character.length; index += 1) {
      boundaries[renderedOffset + index] = start + tokenEnd;
    }
    sourceOffset = tokenEnd;
    renderedOffset += character.length;
  }

  return boundaries;
}

function annotationKey(start: number, end: number, annotations: TextAnnotation[]): string {
  const matches = annotations.filter((annotation) => annotation.range.start < end && annotation.range.end > start);
  const search = matches.some((annotation) => annotation.kind === "search");
  const bookmarks = matches.filter((annotation) => annotation.kind === "bookmark").map((annotation) => annotation.id);
  return `${search ? "search" : ""}|${bookmarks.join(",")}`;
}

/** Rehype plugin that makes every rendered text run addressable in source bytes. */
export function sourceMappedMarkdown(content: string, annotations: TextAnnotation[]) {
  const sourceByteBoundaries = new Array<number>(content.length + 1).fill(0);
  let utf16Offset = 0;
  let byteOffset = 0;
  const encoder = new TextEncoder();
  for (const character of content) {
    sourceByteBoundaries[utf16Offset] = byteOffset;
    byteOffset += encoder.encode(character).length;
    utf16Offset += character.length;
    sourceByteBoundaries[utf16Offset] = byteOffset;
  }
  return () => (tree: HastNode) => {
    const visit = (node: HastNode) => {
      if (!node.children) return;
      node.children = node.children.flatMap((child): HastNode[] => {
        if (child.type !== "text" || !child.value || child.position?.start.offset == null || child.position.end.offset == null) {
          visit(child);
          return [child];
        }

        const boundaries = renderedBoundaries(
          content,
          child.value,
          child.position.start.offset,
          child.position.end.offset,
        );
        const byteBoundaries = boundaries.map((offset) => sourceByteBoundaries[offset] ?? byteOffset);
        const characterBoundaries = [0];
        let characterOffset = 0;
        for (const character of child.value) {
          characterOffset += character.length;
          characterBoundaries.push(characterOffset);
        }
        const runs: HastNode[] = [];
        let runStart = 0;
        let key = annotationKey(
          byteBoundaries[0],
          byteBoundaries[characterBoundaries[1] ?? 0],
          annotations,
        );

        for (let characterIndex = 1; characterIndex < characterBoundaries.length; characterIndex += 1) {
          const index = characterBoundaries[characterIndex];
          const nextBoundary = characterBoundaries[characterIndex + 1];
          const nextKey = nextBoundary != null
            ? annotationKey(byteBoundaries[index], byteBoundaries[nextBoundary], annotations)
            : null;
          if (nextKey === key) continue;
          const [search, bookmarkIds] = key.split("|");
          const classes = [
            "markdown-source-run",
            search ? "markdown-search-highlight" : "",
            bookmarkIds ? "markdown-bookmark-highlight" : "",
          ].filter(Boolean);
          runs.push({
            type: "element",
            tagName: "span",
            properties: {
              className: classes,
              dataSourceBoundaries: byteBoundaries.slice(runStart, index + 1).join(","),
              ...(bookmarkIds ? { dataBookmarkIds: bookmarkIds } : {}),
            },
            children: [{ type: "text", value: child.value.slice(runStart, index) }],
          });
          runStart = index;
          key = nextKey ?? "|";
        }
        return runs;
      });
    };
    visit(tree);
  };
}

export function sourceBoundaryForDomPoint(node: Node, offset: number): number | null {
  const element = (node instanceof Element ? node : node.parentElement)?.closest<HTMLElement>(".markdown-source-run");
  if (!element) return null;
  const boundaries = element.dataset.sourceBoundaries?.split(",").map(Number);
  if (!boundaries || boundaries.some(Number.isNaN)) return null;
  const textOffset = node instanceof Element ? (offset === 0 ? 0 : boundaries.length - 1) : offset;
  return boundaries[Math.max(0, Math.min(textOffset, boundaries.length - 1))] ?? null;
}
