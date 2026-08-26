import type { ByteRange } from "./documentCoordinates";

/** A byte range the rendered markdown should mark up.
 *
 *  `className` rather than a fixed `kind`: what a mark *means* is the host's
 *  business, and this plugin only needs to know which runs share a class so it
 *  can cut spans at the boundaries between them. */
export interface TextAnnotation {
  id: string;
  range: ByteRange;
  className?: string;
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

/** Identity of the annotation set covering a run, used only to decide where to
 *  cut spans. Two runs with the same key are rendered as one span. */
function annotationKey(start: number, end: number, annotations: TextAnnotation[]): string {
  return annotations
    .filter((annotation) => annotation.range.start < end && annotation.range.end > start)
    .map((annotation) => annotation.id)
    .join(",");
}

/** Rehype plugin that makes every rendered text run addressable in source bytes. */
export function sourceMappedMarkdown(content: string, annotations: TextAnnotation[]) {
  const annotationsById = new Map(annotations.map((annotation) => [annotation.id, annotation]));
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
    const visit = (node: HastNode, inPre: boolean) => {
      if (!node.children) return;
      // Newlines inside <pre>/<code> are literal content; only soft breaks in
      // flowing text become <br>.
      const childInPre = inPre || node.tagName === "pre" || node.tagName === "code";
      node.children = node.children.flatMap((child): HastNode[] => {
        if (child.type !== "text" || !child.value || child.position?.start.offset == null || child.position.end.offset == null) {
          visit(child, childInPre);
          return [child];
        }

        const value = child.value;
        const boundaries = renderedBoundaries(
          content,
          value,
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
          const ids = key ? key.split(",") : [];
          const classes = [
            "markdown-source-run",
            ...new Set(
              ids
                .map((id) => annotationsById.get(id)?.className)
                .filter((className): className is string => Boolean(className)),
            ),
          ];
          const span = (from: number, to: number): HastNode => ({
            type: "element",
            tagName: "span",
            properties: {
              className: classes,
              dataSourceBoundaries: byteBoundaries.slice(from, to + 1).join(","),
              ...(key ? { dataDecorationIds: key } : {}),
            },
            children: [{ type: "text", value: value.slice(from, to) }],
          });
          if (childInPre) {
            runs.push(span(runStart, index));
          } else {
            // A soft break (`\n`) renders as <br>; the newline itself carries no
            // visible glyph, so it is dropped from the addressable spans.
            let segmentStart = runStart;
            for (let offset = runStart; offset < index; offset += 1) {
              if (value[offset] !== "\n") continue;
              if (offset > segmentStart) runs.push(span(segmentStart, offset));
              runs.push({ type: "element", tagName: "br", properties: {}, children: [] });
              segmentStart = offset + 1;
            }
            if (segmentStart < index) runs.push(span(segmentStart, index));
          }
          runStart = index;
          key = nextKey ?? "|";
        }
        return runs;
      });
    };
    visit(tree, false);
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
