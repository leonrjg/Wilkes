import type { PDFDocumentProxy } from "pdfjs-dist";
import type { BoundingBox } from "./documentCoordinates";
import { loadPdfPageText, type PositionedPdfText } from "./pdfTextContent";

const WRAP_HYPHEN = "\ue000";
const LITERAL_ESCAPE = "\ue001";
const LETTER = /^\p{L}$/u;

interface RawUnit {
  value: string;
  itemIndex: number | null;
  itemStart: number;
  itemEnd: number;
  syntheticEol: boolean;
}

interface NormalizedUnit {
  value: string;
  wrapHyphen: boolean;
  itemIndex: number | null;
  itemStart: number;
  itemEnd: number;
}

interface ProjectionSpan {
  start: number;
  end: number;
  itemIndex: number | null;
  itemStart: number;
  itemEnd: number;
}

interface PageProjection {
  text: string;
  spans: ProjectionSpan[];
}

export interface PdfTextMatch {
  page: number;
  bbox: BoundingBox;
  rects: BoundingBox[];
  contextScore: number;
}

export interface PdfSearchLocator {
  matched_text: string;
  context_before: string;
  context_after: string;
}

function isLetter(value: string): boolean {
  return LETTER.test(value);
}

function isDiscretionaryHyphen(value: string): boolean {
  return (
    value === "-" ||
    value === "\u00ad" ||
    value === "\u2010" ||
    value === "\u2011"
  );
}

function isIgnoredFormatCharacter(value: string): boolean {
  return (
    value === "\u200b" ||
    value === "\u200c" ||
    value === "\u200d" ||
    value === "\u2060" ||
    value === "\ufeff"
  );
}

function rawUnits(items: PositionedPdfText[]): RawUnit[] {
  const units: RawUnit[] = [];
  items.forEach((item, itemIndex) => {
    for (let offset = 0; offset < item.text.length; ) {
      const codePoint = item.text.codePointAt(offset);
      if (codePoint === undefined) break;
      const value = String.fromCodePoint(codePoint);
      units.push({
        value,
        itemIndex,
        itemStart: offset,
        itemEnd: offset + value.length,
        syntheticEol: false,
      });
      offset += value.length;
    }
    if (item.hasEOL) {
      units.push({
        value: "\n",
        itemIndex: null,
        itemStart: 0,
        itemEnd: 0,
        syntheticEol: true,
      });
    }
  });
  return units;
}

function stringUnits(input: string): RawUnit[] {
  const units: RawUnit[] = [];
  for (let offset = 0; offset < input.length; ) {
    const codePoint = input.codePointAt(offset);
    if (codePoint === undefined) break;
    const value = String.fromCodePoint(codePoint);
    units.push({
      value,
      itemIndex: null,
      itemStart: offset,
      itemEnd: offset + value.length,
      syntheticEol:
        value === "\n" ||
        value === "\r" ||
        value === "\u2028" ||
        value === "\u2029",
    });
    offset += value.length;
  }
  return units;
}

function normalizedCharacters(value: string): string {
  switch (value) {
    case "\ufb00":
      return "ff";
    case "\ufb01":
      return "fi";
    case "\ufb02":
      return "fl";
    case "\ufb03":
      return "ffi";
    case "\ufb04":
      return "ffl";
    case "\u2018":
    case "\u2019":
    case "\u201a":
    case "\u201b":
    case "\u02bc":
      return "'";
    case "\u201c":
    case "\u201d":
    case "\u201e":
    case "\u201f":
      return "\"";
    case "\u2010":
    case "\u2011":
      return "-";
    default:
      return value;
  }
}

function normalize(units: RawUnit[], trimOuterWhitespace: boolean): NormalizedUnit[] {
  const normalized: NormalizedUnit[] = [];
  let index = 0;

  while (index < units.length) {
    const unit = units[index];
    const previous = units[index - 1];

    if (isDiscretionaryHyphen(unit.value) && previous && isLetter(previous.value)) {
      let continuation = index + 1;
      let crossedLine = false;
      while (continuation < units.length && /\s/u.test(units[continuation].value)) {
        crossedLine ||= units[continuation].syntheticEol;
        continuation += 1;
      }
      if (
        crossedLine &&
        continuation < units.length &&
        isLetter(units[continuation].value)
      ) {
        normalized.push({
          value: WRAP_HYPHEN,
          wrapHyphen: true,
          itemIndex: unit.itemIndex,
          itemStart: unit.itemStart,
          itemEnd: unit.itemEnd,
        });
        index = continuation;
        continue;
      }
      if (
        unit.value === "\u00ad" &&
        units[index + 1] &&
        isLetter(units[index + 1].value)
      ) {
        normalized.push({
          value: WRAP_HYPHEN,
          wrapHyphen: true,
          itemIndex: unit.itemIndex,
          itemStart: unit.itemStart,
          itemEnd: unit.itemEnd,
        });
        index += 1;
        continue;
      }
    }

    if (/\s/u.test(unit.value)) {
      let next = index + 1;
      while (next < units.length && /\s/u.test(units[next].value)) {
        next += 1;
      }
      if (!trimOuterWhitespace || normalized.length > 0) {
        normalized.push({
          value: " ",
          wrapHyphen: false,
          itemIndex: unit.itemIndex,
          itemStart: unit.itemStart,
          itemEnd: unit.itemEnd,
        });
      }
      index = next;
      continue;
    }

    if (isIgnoredFormatCharacter(unit.value) || unit.value === "\u00ad") {
      index += 1;
      continue;
    }

    const characters = normalizedCharacters(unit.value);
    for (const value of characters) {
      normalized.push({
        value,
        wrapHyphen: false,
        itemIndex: unit.itemIndex,
        itemStart: unit.itemStart,
        itemEnd: unit.itemEnd,
      });
    }
    index += 1;
  }

  if (
    trimOuterWhitespace &&
    normalized.length > 0 &&
    normalized[normalized.length - 1].value === " "
  ) {
    normalized.pop();
  }
  return normalized;
}

function project(items: PositionedPdfText[]): PageProjection {
  const units = normalize(rawUnits(items), false);
  let text = "";
  const spans: ProjectionSpan[] = [];
  for (const unit of units) {
    const emit = (value: string) => {
      const start = text.length;
      text += value;
      spans.push({
        start,
        end: text.length,
        itemIndex: unit.itemIndex,
        itemStart: unit.itemStart,
        itemEnd: unit.itemEnd,
      });
    };
    if (
      !unit.wrapHyphen &&
      (unit.value === WRAP_HYPHEN || unit.value === LITERAL_ESCAPE)
    ) {
      emit(LITERAL_ESCAPE);
    }
    emit(unit.value);
  }
  return { text, spans };
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function literalMatcher(query: string, caseSensitive: boolean): RegExp | null {
  const units = normalize(stringUnits(query), true);
  if (units.length === 0) return null;
  let pattern = "";
  for (const [index, unit] of units.entries()) {
    const previousIsLetter = index > 0 && isLetter(units[index - 1].value);
    const nextIsLetter =
      index + 1 < units.length && isLetter(units[index + 1].value);
    const betweenLetters = previousIsLetter && nextIsLetter;
    if (unit.wrapHyphen && betweenLetters) {
      pattern += `[${WRAP_HYPHEN}-]`;
    } else if (unit.wrapHyphen) {
      pattern += escapeRegex(WRAP_HYPHEN);
    } else if (unit.value === "-" && betweenLetters) {
      pattern += `[${WRAP_HYPHEN}-]`;
    } else {
      if (unit.value === WRAP_HYPHEN || unit.value === LITERAL_ESCAPE) {
        pattern += escapeRegex(LITERAL_ESCAPE);
      }
      pattern += escapeRegex(unit.value);
    }
    if (isLetter(unit.value) && nextIsLetter) {
      pattern += `${escapeRegex(WRAP_HYPHEN)}?`;
    }
  }
  return new RegExp(pattern, caseSensitive ? "gu" : "giu");
}

function rectForFragment(
  item: PositionedPdfText,
  start: number,
  end: number,
): BoundingBox | null {
  if (!item.text || end <= start) return null;
  if (!item.horizontal) {
    return { x: item.x, y: item.top, width: item.width, height: item.height };
  }
  const startFraction = Math.max(0, Math.min(start / item.text.length, 1));
  const endFraction = Math.max(startFraction, Math.min(end / item.text.length, 1));
  const leftFraction = item.direction === "rtl" ? 1 - endFraction : startFraction;
  return {
    x: item.x + item.width * leftFraction,
    y: item.top,
    width: Math.max(item.width * (endFraction - startFraction), 0.5),
    height: item.height,
  };
}

function mergeRectsByLine(rects: BoundingBox[]): BoundingBox[] {
  const lines: BoundingBox[] = [];
  for (const rect of [...rects].sort((a, b) => a.y - b.y || a.x - b.x)) {
    const last = lines[lines.length - 1];
    const sameLine =
      last && rect.y < last.y + last.height && rect.y + rect.height > last.y;
    if (!sameLine) {
      lines.push({ ...rect });
      continue;
    }
    const right = Math.max(last.x + last.width, rect.x + rect.width);
    const bottom = Math.max(last.y + last.height, rect.y + rect.height);
    last.x = Math.min(last.x, rect.x);
    last.y = Math.min(last.y, rect.y);
    last.width = right - last.x;
    last.height = bottom - last.y;
  }
  return lines;
}

function unionBox(rects: BoundingBox[]): BoundingBox {
  const x = Math.min(...rects.map((rect) => rect.x));
  const y = Math.min(...rects.map((rect) => rect.y));
  const right = Math.max(...rects.map((rect) => rect.x + rect.width));
  const bottom = Math.max(...rects.map((rect) => rect.y + rect.height));
  return { x, y, width: right - x, height: bottom - y };
}

function commonSuffixLength(left: string, right: string, limit = 120): number {
  let length = 0;
  while (
    length < limit &&
    length < left.length &&
    length < right.length &&
    left[left.length - length - 1].toLocaleLowerCase() ===
      right[right.length - length - 1].toLocaleLowerCase()
  ) {
    length += 1;
  }
  return length;
}

function commonPrefixLength(left: string, right: string, limit = 120): number {
  let length = 0;
  while (
    length < limit &&
    length < left.length &&
    length < right.length &&
    left[length].toLocaleLowerCase() === right[length].toLocaleLowerCase()
  ) {
    length += 1;
  }
  return length;
}

function normalizedString(input: string): string {
  return normalize(stringUnits(input), false).map((unit) => unit.value).join("");
}

function matchRects(
  items: PositionedPdfText[],
  projection: PageProjection,
  start: number,
  end: number,
): BoundingBox[] {
  const fragments = new Map<number, { start: number; end: number }>();
  for (const span of projection.spans) {
    if (span.start >= end || span.end <= start || span.itemIndex === null) continue;
    const existing = fragments.get(span.itemIndex);
    fragments.set(span.itemIndex, {
      start: existing ? Math.min(existing.start, span.itemStart) : span.itemStart,
      end: existing ? Math.max(existing.end, span.itemEnd) : span.itemEnd,
    });
  }
  return mergeRectsByLine(
    [...fragments.entries()].flatMap(([itemIndex, fragment]) => {
      const rect = rectForFragment(items[itemIndex], fragment.start, fragment.end);
      return rect ? [rect] : [];
    }),
  );
}

export function findPdfTextMatchesInItems(
  items: PositionedPdfText[],
  page: number,
  query: string,
  options: {
    caseSensitive?: boolean;
    contextBefore?: string;
    contextAfter?: string;
  } = {},
): PdfTextMatch[] {
  const matcher = literalMatcher(query, options.caseSensitive ?? false);
  if (!matcher) return [];
  const projection = project(items);
  const contextBefore = normalizedString(options.contextBefore ?? "");
  const contextAfter = normalizedString(options.contextAfter ?? "");
  const matches: PdfTextMatch[] = [];

  for (const match of projection.text.matchAll(matcher)) {
    const start = match.index;
    const end = start + match[0].length;
    const rects = matchRects(items, projection, start, end);
    if (rects.length === 0) continue;
    const contextScore =
      commonSuffixLength(projection.text.slice(0, start), contextBefore) +
      commonPrefixLength(projection.text.slice(end), contextAfter);
    matches.push({ page, rects, bbox: unionBox(rects), contextScore });
  }
  return matches;
}

export async function findPdfTextMatchesOnPage(
  pdf: PDFDocumentProxy,
  page: number,
  query: string,
  options: {
    caseSensitive?: boolean;
    contextBefore?: string;
    contextAfter?: string;
  } = {},
): Promise<PdfTextMatch[]> {
  const items = await loadPdfPageText(pdf, page);
  return findPdfTextMatchesInItems(items, page, query, options);
}

export async function findAllPdfTextMatches(
  pdf: PDFDocumentProxy,
  query: string,
  signal?: AbortSignal,
): Promise<PdfTextMatch[]> {
  const matches: PdfTextMatch[] = [];
  for (let page = 1; page <= pdf.numPages; page += 1) {
    if (signal?.aborted) return [];
    matches.push(...(await findPdfTextMatchesOnPage(pdf, page, query)));
  }
  return signal?.aborted ? [] : matches;
}

/**
 * Resolve an indexed result close to its chunk-level page hint. The bounded
 * neighborhood covers a chunk whose text begins on one page and whose match is
 * on the next without turning result navigation into a full-document scan.
 */
export async function locatePdfSearchResult(
  pdf: PDFDocumentProxy,
  coarsePage: number,
  locator: PdfSearchLocator,
  signal?: AbortSignal,
): Promise<PdfTextMatch | null> {
  const offsets = [0, 1, -1, 2, -2];
  const pages = offsets
    .map((offset) => coarsePage + offset)
    .filter(
      (page, index, all) =>
        page >= 1 && page <= pdf.numPages && all.indexOf(page) === index,
    );
  const candidates: Array<PdfTextMatch & { pagePriority: number }> = [];
  for (const [pagePriority, page] of pages.entries()) {
    if (signal?.aborted) return null;
    const matches = await findPdfTextMatchesOnPage(pdf, page, locator.matched_text, {
      caseSensitive: true,
      contextBefore: locator.context_before,
      contextAfter: locator.context_after,
    });
    candidates.push(...matches.map((match) => ({ ...match, pagePriority })));
  }
  candidates.sort(
    (left, right) =>
      right.contextScore - left.contextScore || left.pagePriority - right.pagePriority,
  );
  const best = candidates[0];
  if (signal?.aborted || !best) return null;
  const { pagePriority: _pagePriority, ...match } = best;
  return match;
}
