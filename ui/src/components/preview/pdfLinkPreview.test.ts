import { describe, expect, it, vi } from "vitest";
import {
  extractTextBlockAtDestination,
  getPdfLinkPreview,
  type PositionedPdfText,
} from "./pdfLinkPreview";

function line(
  text: string,
  x: number,
  top: number,
  width = Math.max(text.length * 4, 20),
  height = 8,
): PositionedPdfText {
  return {
    text,
    hasEOL: false,
    direction: "ltr",
    x,
    top,
    width,
    height,
    horizontal: true,
  };
}

describe("extractTextBlockAtDestination", () => {
  it("selects a hanging-indent block in the correct column without syntax assumptions", () => {
    const items = [
      line("Earlier block", 50, 90, 230),
      line("Earlier continuation", 65, 100, 210),
      line("Opaque target first line", 50, 110, 230),
      line("Opaque target continuation", 65, 120, 210),
      line("Following block", 50, 130, 230),
      // Unrelated right-column text shares the same visual rows.
      line("Right column row one", 330, 110, 220),
      line("Right column row two", 330, 120, 220),
    ];

    const text = extractTextBlockAtDestination(items, { x: 50, y: 109 });

    expect(text).toBe(
      "Opaque target first line\nOpaque target continuation",
    );
    expect(text).not.toContain("Following");
    expect(text).not.toContain("Right column");
  });

  it("extracts an ordinary prose block based on spacing and column continuity", () => {
    const items = [
      line("A section heading", 50, 50, 180, 14),
      line("The first line of its prose.", 50, 72, 240, 9),
      line("The prose continues here.", 50, 83, 230, 9),
      line("A spatially separate block.", 50, 110, 220, 9),
    ];

    expect(
      extractTextBlockAtDestination(items, { x: 52, y: 49 }),
    ).toBe(
      "A section heading\nThe first line of its prose.\nThe prose continues here.",
    );
  });

  it("declines a coordinate that is ambiguous between columns", () => {
    const items = [
      line("Left target", 50, 100, 220),
      line("Right target", 330, 100, 220),
    ];

    expect(
      extractTextBlockAtDestination(items, { x: null, y: 99 }),
    ).toBeNull();
  });

  it("does not return unrelated text when the nearest line is far from the anchor", () => {
    expect(
      extractTextBlockAtDestination(
        [line("Distant content", 50, 200, 220)],
        { x: 50, y: 20 },
      ),
    ).toBeNull();
  });
});

describe("getPdfLinkPreview", () => {
  it("uses an opaque named destination and caches destination/page work", async () => {
    const getTextContent = vi.fn().mockResolvedValue({
      items: [
        {
          str: "Generic destination text",
          transform: [8, 0, 0, 8, 50, 692],
          width: 120,
          height: 8,
        },
      ],
    });
    const page = {
      view: [0, 0, 600, 800],
      getViewport: () => ({
        transform: [1, 0, 0, -1, 0, 800],
        convertToViewportPoint: (x: number, y: number) => [x, 800 - y],
      }),
      getTextContent,
    };
    const pdf = {
      getDestination: vi
        .fn()
        .mockResolvedValue([
          { ref: 1 },
          { name: "XYZ" },
          50,
          701,
          null,
        ]),
      getPageIndex: vi.fn().mockResolvedValue(0),
      getPage: vi.fn().mockResolvedValue(page),
    } as never;

    const first = await getPdfLinkPreview(pdf, "opaque.destination");
    const second = await getPdfLinkPreview(pdf, "opaque.destination");

    expect(first).toEqual({
      pageNumber: 1,
      text: "Generic destination text",
    });
    expect(second).toEqual(first);
    expect(getTextContent).toHaveBeenCalledOnce();
  });

  it("returns no text for a page-only destination", async () => {
    const pdf = {
      getDestination: vi
        .fn()
        .mockResolvedValue([{ ref: 1 }, { name: "Fit" }]),
      getPageIndex: vi.fn().mockResolvedValue(0),
      getPage: vi.fn(),
    } as never;

    await expect(
      getPdfLinkPreview(pdf, "page.only"),
    ).resolves.toBeNull();
    expect(pdf.getPage).not.toHaveBeenCalled();
  });
});
