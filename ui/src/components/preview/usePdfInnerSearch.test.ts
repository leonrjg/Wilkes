import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { usePdfInnerSearch } from "./usePdfInnerSearch";

describe("usePdfInnerSearch", () => {
  let mockPdf: { numPages: number; getPage: ReturnType<typeof vi.fn> };

  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
    const mockPage = {
      getTextContent: vi.fn().mockResolvedValue({
        items: [
          {
            str: "Hello world",
            hasEOL: false,
            dir: "ltr",
            transform: [1, 0, 0, 1, 10, 10],
            width: 50,
            height: 1,
          },
        ],
      }),
      getViewport: vi.fn().mockReturnValue({
        width: 600,
        height: 800,
        transform: [1, 0, 0, -1, 0, 800],
      }),
    };
    mockPdf = {
      numPages: 2,
      getPage: vi.fn().mockResolvedValue(mockPage),
    };
  });

  it("returns no matches without a query", () => {
    const { result } = renderHook(() => usePdfInnerSearch(mockPdf as any, "", true));
    expect(result.current.matches).toEqual([]);
    expect(result.current.isSearching).toBe(false);
  });

  it("stays idle while disabled even with a query", async () => {
    renderHook(() => usePdfInnerSearch(mockPdf as any, "hello", false));
    await act(async () => {
      vi.advanceTimersByTime(300);
    });
    expect(mockPdf.getPage).not.toHaveBeenCalled();
  });

  it("scans every page and returns a match per page when enabled", async () => {
    const { result } = renderHook(() => usePdfInnerSearch(mockPdf as any, "hello", true));

    await act(async () => {
      vi.advanceTimersByTime(300);
    });

    expect(mockPdf.getPage).toHaveBeenCalledWith(1);
    expect(mockPdf.getPage).toHaveBeenCalledWith(2);
    expect(result.current.matches.length).toBe(2);
    expect(result.current.matches[0].page).toBe(1);
  });

  it("clears matches when the query is emptied", async () => {
    const { result, rerender } = renderHook(
      ({ query }) => usePdfInnerSearch(mockPdf as any, query, true),
      { initialProps: { query: "hello" } },
    );

    await act(async () => {
      vi.advanceTimersByTime(300);
    });
    expect(result.current.matches.length).toBe(2);

    rerender({ query: "" });
    expect(result.current.matches).toEqual([]);
  });
});
