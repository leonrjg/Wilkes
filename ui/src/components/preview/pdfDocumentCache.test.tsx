import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { getDocument } from "pdfjs-dist";
import { usePdfDocument } from "./pdfDocumentCache";
import { pdfjsAssetUrls } from "./pdfjsAssetUrls";

vi.mock("pdfjs-dist", () => ({
  getDocument: vi.fn(),
}));

describe("pdfDocumentCache", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("reports a failed load and retries when the attempt changes", async () => {
    const proxy = { numPages: 1, destroy: vi.fn() } as any;
    vi.mocked(getDocument)
      .mockReturnValueOnce({ promise: Promise.reject(new Error("PDF file not found")) } as any)
      .mockReturnValueOnce({ promise: Promise.resolve(proxy) } as any);
    const onLoadError = vi.fn();
    const { result, rerender } = renderHook(
      ({ attempt }) => usePdfDocument("retryable-missing.pdf", attempt, onLoadError),
      { initialProps: { attempt: 0 } },
    );

    await waitFor(() => expect(onLoadError).toHaveBeenCalledWith(
      expect.objectContaining({ message: "PDF file not found" }),
    ));
    expect(result.current).toBeNull();

    rerender({ attempt: 1 });

    await waitFor(() => expect(result.current).toBe(proxy));
    expect(getDocument).toHaveBeenCalledTimes(2);
  });

  it("tells pdf.js where its decoders, fonts and character maps live", async () => {
    // Without these pdf.js cannot decode a scanned page, and the document
    // renders blank behind an invisible, still-selectable OCR text layer.
    const proxy = { numPages: 1 } as any;
    vi.mocked(getDocument).mockReturnValue({ promise: Promise.resolve(proxy) } as any);

    renderHook(() => usePdfDocument("assets.pdf"));

    await waitFor(() => expect(getDocument).toHaveBeenCalled());
    expect(vi.mocked(getDocument).mock.calls[0][0]).toMatchObject({
      url: "assets.pdf",
      ...pdfjsAssetUrls(),
    });
  });
});
