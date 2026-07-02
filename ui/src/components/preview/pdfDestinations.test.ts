import { describe, it, expect, vi } from "vitest";
import { resolveDestination } from "./pdfDestinations";

function makePdf(overrides: Record<string, unknown> = {}) {
  return {
    getDestination: vi.fn(),
    getPageIndex: vi.fn().mockResolvedValue(4),
    getPage: vi.fn().mockResolvedValue({
      getViewport: () => ({
        // Emulate pdf.js' bottom-left → top-left flip for a 800-high page.
        convertToViewportPoint: (_x: number, y: number) => [0, 800 - y],
      }),
    }),
    ...overrides,
  } as never;
}

describe("resolveDestination", () => {
  it("resolves an explicit XYZ destination to page index and top-left offset", async () => {
    const pdf = makePdf();
    const dest = [{ ref: 1 }, { name: "XYZ" }, 0, 700, null];

    const resolved = await resolveDestination(pdf, dest);

    expect(resolved).toEqual({ pageIndex: 4, offsetY: 100 });
  });

  it("resolves a named destination via getDestination", async () => {
    const pdf = makePdf({
      getDestination: vi.fn().mockResolvedValue([{ ref: 2 }, { name: "FitH" }, 600]),
    });

    const resolved = await resolveDestination(pdf, "section.1");

    expect(pdf.getDestination).toHaveBeenCalledWith("section.1");
    expect(resolved).toEqual({ pageIndex: 4, offsetY: 200 });
  });

  it("returns a null offset for destinations without a pinned position", async () => {
    const pdf = makePdf();
    const dest = [{ ref: 1 }, { name: "Fit" }];

    const resolved = await resolveDestination(pdf, dest);

    expect(resolved).toEqual({ pageIndex: 4, offsetY: null });
    expect(pdf.getPage).not.toHaveBeenCalled();
  });

  it("returns null for an unresolvable named destination", async () => {
    const pdf = makePdf({ getDestination: vi.fn().mockResolvedValue(null) });

    expect(await resolveDestination(pdf, "missing")).toBeNull();
  });
});
