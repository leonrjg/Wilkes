import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import PdfLinkLayer from "./PdfLinkLayer";

function makePdf(annotations: unknown[]) {
  return {
    getPage: vi.fn().mockResolvedValue({
      getAnnotations: vi.fn().mockResolvedValue(annotations),
      // Identity-ish mapping at scale 1 keeps the rect math easy to assert.
      getViewport: () => ({
        convertToViewportRectangle: (rect: number[]) => rect,
      }),
    }),
  } as never;
}

describe("PdfLinkLayer", () => {
  it("renders overlays only for Link annotations that navigate somewhere", async () => {
    const pdf = makePdf([
      { subtype: "Link", dest: "sec.1", rect: [10, 20, 60, 40] },
      { subtype: "Link", url: "https://example.com", rect: [10, 50, 60, 70] },
      { subtype: "Link", rect: [0, 0, 5, 5] }, // no dest/url → skipped
      { subtype: "Text", dest: "sec.2", rect: [0, 0, 5, 5] }, // not a Link → skipped
    ]);

    render(
      <PdfLinkLayer
        pdf={pdf}
        pageNumber={1}
        scale={1}
        onNavigateToDestination={vi.fn()}
        onOpenExternal={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getAllByTestId("pdf-link")).toHaveLength(2));
  });

  it("invokes navigation for an internal link and external open for a URL link", async () => {
    const onNavigate = vi.fn();
    const onOpen = vi.fn();
    const pdf = makePdf([
      { subtype: "Link", dest: "sec.1", rect: [10, 20, 60, 40] },
      { subtype: "Link", url: "https://example.com", rect: [10, 50, 60, 70] },
    ]);

    render(
      <PdfLinkLayer
        pdf={pdf}
        pageNumber={1}
        scale={1}
        onNavigateToDestination={onNavigate}
        onOpenExternal={onOpen}
      />,
    );

    await waitFor(() => expect(screen.getAllByTestId("pdf-link")).toHaveLength(2));
    const [internal, external] = screen.getAllByTestId("pdf-link");

    fireEvent.click(internal);
    expect(onNavigate).toHaveBeenCalledWith("sec.1");

    fireEvent.click(external);
    expect(onOpen).toHaveBeenCalledWith("https://example.com");
  });
});
