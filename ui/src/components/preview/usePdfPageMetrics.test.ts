import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { getScaledPageHeight, usePdfPageMetrics } from "./usePdfPageMetrics";

describe("usePdfPageMetrics", () => {
  it("loads intrinsic metrics for every page in a PDF", async () => {
    const mockPdf = {
      numPages: 3,
      getPage: vi
        .fn()
        .mockResolvedValueOnce({
          getViewport: vi.fn().mockReturnValue({ width: 600, height: 800 }),
        })
        .mockResolvedValueOnce({
          getViewport: vi.fn().mockReturnValue({ width: 600, height: 900 }),
        })
        .mockResolvedValueOnce({
          getViewport: vi.fn().mockReturnValue({ width: 700, height: 700 }),
        }),
    };

    const { result } = renderHook(() => usePdfPageMetrics(mockPdf as any, "test.pdf"));

    expect(result.current.isLoadingPageMetrics).toBe(true);

    await waitFor(() => {
      expect(result.current.hasPageMetrics).toBe(true);
    });

    expect(mockPdf.getPage).toHaveBeenCalledTimes(3);
    expect(result.current.pageMetrics).toEqual([
      { width: 600, height: 800 },
      { width: 600, height: 900 },
      { width: 700, height: 700 },
    ]);
  });

  it("resets metrics when no PDF is present", () => {
    const { result } = renderHook(() => usePdfPageMetrics(null, "test.pdf"));

    expect(result.current.pageMetrics).toEqual([]);
    expect(result.current.isLoadingPageMetrics).toBe(false);
    expect(result.current.hasPageMetrics).toBe(false);
  });

  it("scales page height from intrinsic dimensions", () => {
    expect(getScaledPageHeight({ width: 600, height: 900 }, 300)).toBe(450);
  });
});
