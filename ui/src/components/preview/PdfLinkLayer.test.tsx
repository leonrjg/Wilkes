import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { beforeEach, describe, it, expect, vi } from "vitest";
import PdfLinkLayer from "./PdfLinkLayer";

const { mockGetPdfLinkPreview } = vi.hoisted(() => ({
  mockGetPdfLinkPreview: vi.fn(),
}));
vi.mock("./pdfLinkPreview", () => ({
  getPdfLinkPreview: mockGetPdfLinkPreview,
}));

function makePdf(annotations: unknown[]) {
  return {
    getPage: vi.fn().mockResolvedValue({
      getAnnotations: vi.fn().mockResolvedValue(annotations),
      // Identity-ish mapping at scale 1 keeps the rect math easy to assert.
      getViewport: () => ({
        convertToViewportPoint: (x: number, y: number) => [x, y],
      }),
    }),
  } as never;
}

describe("PdfLinkLayer", () => {
  beforeEach(() => {
    mockGetPdfLinkPreview.mockReset();
    mockGetPdfLinkPreview.mockResolvedValue(null);
  });

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
    expect(screen.getAllByTestId("pdf-link")[0]).toHaveStyle({ zIndex: "2" });
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

  it("loads an internal-link preview on focus without changing click navigation", async () => {
    const onNavigate = vi.fn();
    mockGetPdfLinkPreview.mockResolvedValue({
      pageNumber: 12,
      text: "Target text from an opaque destination",
    });
    const pdf = makePdf([
      { subtype: "Link", dest: "opaque.destination", rect: [10, 20, 60, 40] },
    ]);

    render(
      <PdfLinkLayer
        pdf={pdf}
        pageNumber={1}
        scale={1}
        onNavigateToDestination={onNavigate}
        onOpenExternal={vi.fn()}
      />,
    );

    const link = await screen.findByTestId("pdf-link");
    fireEvent.focus(link);

    await waitFor(() =>
      expect(screen.getByRole("tooltip")).toHaveTextContent(
        "Target text from an opaque destination",
      ),
    );
    const tooltip = screen.getByRole("tooltip");
    expect(tooltip).toHaveTextContent("Page 12");
    expect(tooltip).toHaveClass("pointer-events-auto", "select-text");
    expect(mockGetPdfLinkPreview).toHaveBeenCalledWith(
      pdf,
      "opaque.destination",
    );

    fireEvent.click(link);
    expect(onNavigate).toHaveBeenCalledWith("opaque.destination");
  });
});
