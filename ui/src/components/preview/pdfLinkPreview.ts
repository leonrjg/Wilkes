import type { PDFDocumentProxy } from "pdfjs-dist";
import { resolveDestination, type PdfDestination } from "./pdfDestinations";
import { loadPdfPageText, type PositionedPdfText } from "./pdfTextContent";

export type { PositionedPdfText } from "./pdfTextContent";

export interface PdfLinkPreview {
  pageNumber: number;
  text: string;
}

interface TextLine {
  text: string;
  x: number;
  top: number;
  width: number;
  height: number;
}

interface PreviewAnchor {
  x: number | null;
  y: number;
}

const MAX_PREVIEW_LINES = 5;
const MAX_PREVIEW_CHARACTERS = 600;

const pageLineCache = new WeakMap<
  PDFDocumentProxy,
  Map<number, Promise<TextLine[]>>
>();
const previewCache = new WeakMap<
  PDFDocumentProxy,
  Map<string, Promise<PdfLinkPreview | null>>
>();

function median(values: number[]): number {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2
    ? sorted[middle]
    : (sorted[middle - 1] + sorted[middle]) / 2;
}

function destinationKey(dest: PdfDestination): string {
  if (typeof dest === "string") return `named:${dest}`;
  try {
    return `explicit:${JSON.stringify(dest)}`;
  } catch {
    // Explicit destinations are normally small arrays of page refs, names, and
    // numbers. An unusual non-serializable value should merely miss deduping,
    // not prevent a preview from being generated.
    return `explicit:${dest.map((part) => String(part)).join("|")}`;
  }
}

function joinItems(items: PositionedPdfText[]): string {
  const sorted = [...items].sort((a, b) => a.x - b.x);
  let result = "";
  let previous: PositionedPdfText | null = null;

  for (const item of sorted) {
    const text = item.text.replace(/\s+/g, " ");
    if (!text) continue;
    if (
      result &&
      previous &&
      !/\s$/.test(result) &&
      !/^\s/.test(text) &&
      item.x - (previous.x + previous.width) >
        Math.max(0.75, Math.min(previous.height, item.height) * 0.12)
    ) {
      result += " ";
    }
    result += text;
    previous = item;
  }

  return result.trim();
}

/**
 * Group positioned PDF.js text items into visual line segments. A baseline can
 * contain unrelated items from two columns, so a large horizontal gap splits a
 * row into independent lines before any destination matching occurs.
 */
export function groupPdfTextLines(items: PositionedPdfText[]): TextLine[] {
  const rows: Array<{
    items: PositionedPdfText[];
    top: number;
    bottom: number;
    center: number;
    centers: number[];
    typicalHeight: number;
  }> = [];
  const sorted = [...items].sort(
    (a, b) => a.top + a.height / 2 - (b.top + b.height / 2) || a.x - b.x,
  );

  for (const item of sorted) {
    const center = item.top + item.height / 2;
    let bestRow: (typeof rows)[number] | null = null;
    let bestDistance = Number.POSITIVE_INFINITY;

    // Rows are sorted vertically, so only nearby rows can overlap this item.
    for (let index = rows.length - 1; index >= 0; index--) {
      const row = rows[index];
      if (
        center - row.center >
        Math.max(item.height, row.typicalHeight) * 1.2
      ) {
        break;
      }
      const distance = Math.abs(center - row.center);
      const tolerance = Math.max(
        1.5,
        Math.min(item.height, row.typicalHeight) * 0.45,
      );
      if (distance <= tolerance) {
        if (distance < bestDistance) {
          bestRow = row;
          bestDistance = distance;
        }
      }
    }

    if (!bestRow) {
      rows.push({
        items: [item],
        top: item.top,
        bottom: item.top + item.height,
        center,
        centers: [center],
        typicalHeight: item.height,
      });
      continue;
    }

    bestRow.items.push(item);
    bestRow.top = Math.min(bestRow.top, item.top);
    bestRow.bottom = Math.max(bestRow.bottom, item.top + item.height);
    bestRow.centers.push(center);
    bestRow.center = median(bestRow.centers);
    bestRow.typicalHeight = median(
      bestRow.items.map((rowItem) => rowItem.height),
    );
  }

  const lines: TextLine[] = [];
  for (const row of rows) {
    const rowItems = [...row.items].sort((a, b) => a.x - b.x);
    const typicalHeight = median(rowItems.map((item) => item.height));
    const splitGap = Math.max(24, typicalHeight * 3.5);
    let segment: PositionedPdfText[] = [];

    const flush = () => {
      if (segment.length === 0) return;
      const x = Math.min(...segment.map((item) => item.x));
      const right = Math.max(
        ...segment.map((item) => item.x + item.width),
      );
      const top = Math.min(...segment.map((item) => item.top));
      const bottom = Math.max(
        ...segment.map((item) => item.top + item.height),
      );
      const text = joinItems(segment);
      if (text) {
        lines.push({
          text,
          x,
          top,
          width: right - x,
          height: bottom - top,
        });
      }
      segment = [];
    };

    for (const item of rowItems) {
      const previous = segment[segment.length - 1];
      if (previous && item.x - (previous.x + previous.width) > splitGap) {
        flush();
      }
      segment.push(item);
    }
    flush();
  }

  return lines.sort((a, b) => a.top - b.top || a.x - b.x);
}

function horizontalDistance(line: TextLine, x: number): number {
  if (x < line.x) return line.x - x;
  if (x > line.x + line.width) return x - (line.x + line.width);
  return 0;
}

function findAnchorLine(
  lines: TextLine[],
  anchor: PreviewAnchor,
): number | null {
  if (lines.length === 0) return null;

  let bestIndex = -1;
  let bestScore = Number.POSITIVE_INFINITY;
  for (const [index, line] of lines.entries()) {
    const deltaFromTop = line.top - anchor.y;
    // PDF destinations usually sit at the upper edge of their target. Penalize
    // lines materially above the anchor so an anchor between two tightly packed
    // bibliography/prose lines chooses the following line.
    const verticalDistance =
      deltaFromTop >= -line.height * 0.25
        ? Math.abs(deltaFromTop)
        : Math.abs(deltaFromTop) + line.height * 2;
    const xDistance =
      anchor.x === null ? 0 : horizontalDistance(line, anchor.x);
    const score = verticalDistance * 4 + xDistance;
    if (score < bestScore) {
      bestIndex = index;
      bestScore = score;
    }
  }

  if (bestIndex < 0) return null;
  const best = lines[bestIndex];
  if (Math.abs(best.top - anchor.y) > Math.max(36, best.height * 4)) {
    return null;
  }
  if (
    anchor.x !== null &&
    horizontalDistance(best, anchor.x) > Math.max(24, best.height * 4)
  ) {
    return null;
  }

  if (anchor.x === null) {
    // A horizontal-only destination in a multi-column row does not identify
    // which column is intended. Returning either would be confidently wrong.
    const sameRow = lines.filter(
      (line) =>
        line !== best &&
        Math.abs(line.top - best.top) <=
          Math.max(1.5, Math.min(line.height, best.height) * 0.2),
    );
    if (sameRow.length > 0) return null;
  }

  return bestIndex;
}

function linesShareColumn(a: TextLine, b: TextLine): boolean {
  const overlap =
    Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
  const overlapRatio =
    overlap > 0 ? overlap / Math.max(Math.min(a.width, b.width), 1) : 0;
  const leftDistance = Math.abs(a.x - b.x);
  return (
    overlapRatio >= 0.2 ||
    leftDistance <= Math.max(18, Math.min(a.height, b.height) * 2.5)
  );
}

function truncateByCharacters(text: string, limit: number): string {
  const characters = Array.from(text);
  return characters.length <= limit
    ? text
    : `${characters.slice(0, limit - 1).join("").trimEnd()}…`;
}

/**
 * Select a bounded visual text block at a spatial destination. The algorithm is
 * deliberately unaware of citation markers, heading syntax, or destination
 * names. It follows line geometry, column continuity, spacing, and generic
 * hanging-indent boundaries only.
 */
function extractTextBlockFromLines(
  lines: TextLine[],
  anchor: PreviewAnchor,
): string | null {
  const anchorIndex = findAnchorLine(lines, anchor);
  if (anchorIndex === null) return null;

  const selected = [lines[anchorIndex]];
  const base = lines[anchorIndex];
  let current = base;
  let sawContinuationIndent = false;
  const indentThreshold = Math.max(4, base.height * 0.6);

  for (
    let index = anchorIndex + 1;
    index < lines.length && selected.length < MAX_PREVIEW_LINES;
    index++
  ) {
    const next = lines[index];
    if (next.top <= current.top + 1) continue;

    const verticalGap = next.top - (current.top + current.height);
    const maximumGap = Math.max(12, Math.max(current.height, next.height) * 1.5);
    if (verticalGap > maximumGap) break;
    if (!linesShareColumn(base, next)) continue;

    const indent = next.x - base.x;
    if (indent < -indentThreshold) break;
    if (sawContinuationIndent && indent <= indentThreshold * 0.35) break;
    if (indent > indentThreshold) sawContinuationIndent = true;

    selected.push(next);
    current = next;
  }

  return truncateByCharacters(
    selected.map((line) => line.text).join("\n"),
    MAX_PREVIEW_CHARACTERS,
  );
}

export function extractTextBlockAtDestination(
  items: PositionedPdfText[],
  anchor: PreviewAnchor,
): string | null {
  return extractTextBlockFromLines(groupPdfTextLines(items), anchor);
}

async function loadPageLines(
  pdf: PDFDocumentProxy,
  pageIndex: number,
): Promise<TextLine[]> {
  let documentPages = pageLineCache.get(pdf);
  if (!documentPages) {
    documentPages = new Map();
    pageLineCache.set(pdf, documentPages);
  }
  const cached = documentPages.get(pageIndex);
  if (cached) return cached;

  const promise = (async () => {
    const items = await loadPdfPageText(pdf, pageIndex + 1);
    return groupPdfTextLines(items.filter((item) => item.text.trim()));
  })();
  documentPages.set(pageIndex, promise);
  promise.catch(() => documentPages?.delete(pageIndex));
  return promise;
}

export async function getPdfLinkPreview(
  pdf: PDFDocumentProxy,
  dest: PdfDestination,
): Promise<PdfLinkPreview | null> {
  let documentPreviews = previewCache.get(pdf);
  if (!documentPreviews) {
    documentPreviews = new Map();
    previewCache.set(pdf, documentPreviews);
  }
  const key = destinationKey(dest);
  const cached = documentPreviews.get(key);
  if (cached) return cached;

  const promise = (async () => {
    const resolved = await resolveDestination(pdf, dest);
    if (!resolved || resolved.offsetY === null) return null;
    const lines = await loadPageLines(pdf, resolved.pageIndex);
    const text = extractTextBlockFromLines(lines, {
      x: resolved.offsetX,
      y: resolved.offsetY,
    });
    return text
      ? { pageNumber: resolved.pageIndex + 1, text }
      : null;
  })();

  documentPreviews.set(key, promise);
  promise.catch(() => documentPreviews?.delete(key));
  return promise;
}
