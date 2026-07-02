import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { usePdfOutline } from "./usePdfOutline";

function makePdf(getOutline: () => Promise<unknown>) {
  return { getOutline: vi.fn(getOutline) } as never;
}

describe("usePdfOutline", () => {
  it("returns null when the document has no outline", async () => {
    const pdf = makePdf(async () => null);
    const { result } = renderHook(() => usePdfOutline(pdf));
    // Give the effect a chance to run; value should stay null.
    await waitFor(() => expect(pdf.getOutline).toHaveBeenCalled());
    expect(result.current).toBeNull();
  });

  it("normalizes a nested outline into title/dest/url/items nodes", async () => {
    const pdf = makePdf(async () => [
      {
        title: "Chapter 1",
        dest: "ch1",
        items: [{ title: "Section 1.1", dest: ["ref", { name: "XYZ" }], items: [] }],
      },
      { title: "External", url: "https://example.com" },
    ]);

    const { result } = renderHook(() => usePdfOutline(pdf));

    await waitFor(() => expect(result.current).not.toBeNull());
    expect(result.current).toEqual([
      {
        title: "Chapter 1",
        dest: "ch1",
        url: null,
        items: [
          { title: "Section 1.1", dest: ["ref", { name: "XYZ" }], url: null, items: [] },
        ],
      },
      { title: "External", dest: null, url: "https://example.com", items: [] },
    ]);
  });

  it("returns null when pdf is null", () => {
    const { result } = renderHook(() => usePdfOutline(null));
    expect(result.current).toBeNull();
  });
});
