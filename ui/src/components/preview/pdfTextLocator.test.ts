import { describe, expect, it, vi } from "vitest";
import type { PositionedPdfText } from "./pdfTextContent";
import {
  findPdfTextMatchesInItems,
  locatePdfSearchResult,
} from "./pdfTextLocator";

function item(
  text: string,
  x: number,
  top: number,
  options: { width?: number; height?: number; hasEOL?: boolean } = {},
): PositionedPdfText {
  return {
    text,
    hasEOL: options.hasEOL ?? false,
    direction: "ltr",
    x,
    top,
    width: options.width ?? Math.max(text.length * 5, 0.5),
    height: options.height ?? 10,
    horizontal: true,
  };
}

describe("findPdfTextMatchesInItems", () => {
  it("matches a dehyphenated query across PDF.js items and returns per-line rectangles", () => {
    const matches = findPdfTextMatchesInItems(
      [
        item("be reason-", 100, 10, { width: 100, hasEOL: true }),
        item("able by reasonable people.", 20, 25, { width: 130 }),
      ],
      31,
      "reasonable by reasonable people",
    );

    expect(matches).toHaveLength(1);
    expect(matches[0].page).toBe(31);
    expect(matches[0].rects).toHaveLength(2);
    // Only "reason-", not the leading "be ", is included on the first line.
    expect(matches[0].rects[0].x).toBeCloseTo(130);
    expect(matches[0].bbox.y).toBe(10);
  });

  it("also locates the raw backend quote containing the printed wrap hyphen", () => {
    const matches = findPdfTextMatchesInItems(
      [
        item("reason-", 100, 10, { hasEOL: true }),
        item("able by reasonable people", 20, 25),
      ],
      31,
      "reason-\nable by reasonable people",
      { caseSensitive: true },
    );

    expect(matches).toHaveLength(1);
    expect(matches[0].rects).toHaveLength(2);
  });

  it("does not erase a genuine inline hyphen", () => {
    const items = [item("well-being", 10, 10)];

    expect(findPdfTextMatchesInItems(items, 1, "wellbeing")).toEqual([]);
    expect(findPdfTextMatchesInItems(items, 1, "well-being")).toHaveLength(1);
  });

  it("uses surrounding context to score repeated text", () => {
    const matches = findPdfTextMatchesInItems(
      [item("first target ending. second target chosen.", 10, 10)],
      1,
      "target",
      { contextBefore: "second " },
    );

    expect(matches).toHaveLength(2);
    expect(matches[1].contextScore).toBeGreaterThan(matches[0].contextScore);
  });
});

describe("locatePdfSearchResult", () => {
  it("corrects a coarse chunk page using the following physical page", async () => {
    const getPage = vi.fn(async (page: number) => ({
      getViewport: () => ({ transform: [1, 0, 0, -1, 0, 800] }),
      getTextContent: async () => ({
        items: page === 31
          ? [
              {
                str: "reason-",
                hasEOL: true,
                dir: "ltr",
                transform: [10, 0, 0, 10, 100, 600],
                width: 40,
                height: 10,
              },
              {
                str: "able by reasonable people",
                hasEOL: false,
                dir: "ltr",
                transform: [10, 0, 0, 10, 50, 585],
                width: 120,
                height: 10,
              },
            ]
          : [],
      }),
    }));
    const pdf = { numPages: 31, getPage } as never;

    const match = await locatePdfSearchResult(pdf, 30, {
      matched_text: "reason-\nable by reasonable people",
      context_before: "found to be ",
      context_after: ". An effort",
    });

    expect(match?.page).toBe(31);
    expect(match?.rects).toHaveLength(2);
    expect(getPage).toHaveBeenCalledWith(30);
    expect(getPage).toHaveBeenCalledWith(31);
  });
});
