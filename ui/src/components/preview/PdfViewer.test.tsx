import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { StrictMode } from "react";
import PdfViewer from "./PdfViewer";
import { savePdfScrollPosition } from "./pdfScrollMemory";

const { mockVirtualizer } = vi.hoisted(() => ({
  mockVirtualizer: {
    getTotalSize: () => 1000,
    getVirtualItems: () => [
      { index: 0, key: "0", start: 0 },
      { index: 1, key: "1", start: 900 },
      { index: 2, key: "2", start: 1800 },
    ],
    scrollToIndex: vi.fn(),
    measure: vi.fn(),
  },
}));

const { mockUsePdfInnerSearch } = vi.hoisted(() => ({
  mockUsePdfInnerSearch: {
    value: {
      searchInputRef: { current: null },
      isSearchOpen: false,
      setIsSearchOpen: vi.fn(),
      innerQuery: "",
      setInnerQuery: vi.fn(),
      innerMatches: [],
      currentMatchIdx: -1,
      isSearching: false,
      handleNextMatch: vi.fn(),
      handlePrevMatch: vi.fn(),
      handleSearchInputKeyDown: vi.fn(),
    },
  },
}));

const { mockUsePdfPageMetrics } = vi.hoisted(() => ({
  mockUsePdfPageMetrics: {
    value: {
      pageMetrics: [
        { width: 600, height: 800 },
        { width: 600, height: 800 },
        { width: 600, height: 800 },
      ],
      isLoadingPageMetrics: false,
      hasPageMetrics: true,
    },
  },
}));

// The `pdf` document proxy handed to the viewer via <Document onLoadSuccess>.
// Defaults to a textless stub so auto-zoom measures no body text and stays at
// 100%; auto-zoom tests override `getPage` to return sized glyphs.
const { mockPdfDoc } = vi.hoisted(() => ({
  mockPdfDoc: {
    value: {
      numPages: 10,
      getPage: async (_pageNumber: number) => ({
        view: [0, 0, 600, 800],
        getTextContent: async () => ({ items: [] as unknown[] }),
      }),
    } as {
      numPages: number;
      getPage: (pageNumber: number) => Promise<{
        view: number[];
        getTextContent: () => Promise<{ items: unknown[] }>;
      }>;
    },
  },
}));

const mockPage = vi.fn(({ pageNumber, onLoadSuccess, onRenderSuccess }: any) => {
  if (onLoadSuccess && pageNumber === 1) {
    setTimeout(() => onLoadSuccess({ getViewport: () => ({ width: 600, height: 800 }) }), 0);
  }
  if (onRenderSuccess) {
    setTimeout(() => onRenderSuccess(), 0);
  }
  return <div data-testid={`pdf-page-${pageNumber}`} />;
});

// Mock react-pdf
vi.mock("react-pdf", () => ({
  Document: ({ children, onLoadSuccess }: any) => {
    // Simulate loading success
    if (onLoadSuccess) {
      setTimeout(() => onLoadSuccess(mockPdfDoc.value), 0);
    }
    return <div data-testid="pdf-document">{children}</div>;
  },
  Page: (props: any) => mockPage(props),
  pdfjs: { GlobalWorkerOptions: { workerSrc: "" } },
}));

// The document proxy now comes from the shared LRU cache hook rather than
// react-pdf's <Document onLoadSuccess>. Hand the viewer the same stub directly.
vi.mock("./pdfDocumentCache", () => ({
  usePdfDocument: () => mockPdfDoc.value,
  peekCachedPdfDocument: () => mockPdfDoc.value,
  loadPdfDocument: async () => mockPdfDoc.value,
}));

// Mock the text-selection overlay; it loads pdf.js' viewer-components bundle,
// which is out of scope for these PdfViewer rendering/navigation unit tests.
vi.mock("./PdfTextLayer", () => ({
  default: () => null,
}));

// Mock the link-annotation overlay; it calls pdf.js page APIs absent from the
// lightweight `pdf` stub these rendering/navigation unit tests use.
vi.mock("./PdfLinkLayer", () => ({
  default: () => null,
}));

// Mock @tanstack/react-virtual
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: vi.fn().mockReturnValue(mockVirtualizer),
}));

vi.mock("./usePdfInnerSearch", () => ({
  usePdfInnerSearch: vi.fn(() => mockUsePdfInnerSearch.value),
}));

// The outline hook calls pdf.getOutline(), absent from the lightweight `pdf`
// stub; drive its return value per-test via mockUsePdfOutline.
const { mockUsePdfOutline } = vi.hoisted(() => ({
  mockUsePdfOutline: { value: null as unknown },
}));
vi.mock("./usePdfOutline", () => ({
  usePdfOutline: vi.fn(() => mockUsePdfOutline.value),
}));

// Render the real outline panel so its presence/absence is observable.
vi.mock("./PdfOutline", () => ({
  default: () => <div data-testid="pdf-outline-panel" />,
}));

vi.mock("./usePdfPageMetrics", async () => {
  const actual = await vi.importActual<typeof import("./usePdfPageMetrics")>("./usePdfPageMetrics");
  return {
    ...actual,
    usePdfPageMetrics: vi.fn(() => mockUsePdfPageMetrics.value),
  };
});

// Non-firing ResizeObserver: leaves `containerWidth` at its 600px placeholder
// (pageScale = 1, which the overlay-position assertions rely on). Auto-zoom does
// not depend on the observed width — it measures against a fixed reference — so
// nothing here needs to report a size.
global.ResizeObserver = class {
  observe = vi.fn();
  unobserve = vi.fn();
  disconnect = vi.fn();
} as unknown as typeof ResizeObserver;

describe("PdfViewer", () => {
  const defaultProps = {
    url: "test.pdf",
    page: 1,
    highlight_bbox: { x: 10, y: 10, width: 50, height: 20 },
    onRenderSuccess: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockPdfDoc.value = {
      numPages: 10,
      getPage: async (_pageNumber: number) => ({
        view: [0, 0, 600, 800],
        getTextContent: async () => ({ items: [] as unknown[] }),
      }),
    };
    mockUsePdfOutline.value = null;
    document.documentElement.classList.remove("dark");
    mockVirtualizer.getVirtualItems = () => [
      { index: 0, key: "0", start: 0 },
      { index: 1, key: "1", start: 900 },
      { index: 2, key: "2", start: 1800 },
    ];
    mockUsePdfInnerSearch.value = {
      searchInputRef: { current: null },
      isSearchOpen: false,
      setIsSearchOpen: vi.fn(),
      innerQuery: "",
      setInnerQuery: vi.fn(),
      innerMatches: [],
      currentMatchIdx: -1,
      isSearching: false,
      handleNextMatch: vi.fn(),
      handlePrevMatch: vi.fn(),
      handleSearchInputKeyDown: vi.fn(),
    };
    mockUsePdfPageMetrics.value = {
      pageMetrics: [
        { width: 600, height: 800 },
        { width: 600, height: 800 },
        { width: 600, height: 800 },
      ],
      isLoadingPageMetrics: false,
      hasPageMetrics: true,
    };
    global.requestAnimationFrame = ((cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    }) as typeof requestAnimationFrame;
  });

  it("renders correctly and handles load success", async () => {
    render(<PdfViewer {...defaultProps} />);
    expect(screen.getByTestId("pdf-page-1")).toBeInTheDocument();

    // Wait for async load success
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });
    
    expect(screen.getByText("100%")).toBeInTheDocument();
    expect(screen.getByText("1/10")).toBeInTheDocument();
  });

  it("changes zoom in 10 percent steps", async () => {
    render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    fireEvent.click(screen.getByRole("button", { name: "+" }));
    expect(screen.getByText("110%")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "−" }));
    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  // Build a document proxy whose sampled pages report a uniform body-font size
  // (via the text-transform vertical scale) on a page of the given point width.
  const sizedDoc = (fontSize: number, pageWidth: number) => ({
    numPages: 10,
    getPage: async (_pageNumber: number) => ({
      view: [0, 0, pageWidth, 800],
      getTextContent: async () => ({
        items: Array.from({ length: 3 }, () => ({
          str: "sample text",
          transform: [fontSize, 0, 0, fontSize, 0, 0],
        })),
      }),
    }),
  });

  it("auto-zooms in when body text renders small at fit-to-width", async () => {
    // 9pt body on a 612pt (US Letter) page renders ~13.2px at the 900px
    // reference fit, below the ~16.5px target -> 16.5 / 13.235 ≈ 1.25x.
    mockPdfDoc.value = sizedDoc(9, 612);

    // Render under StrictMode: its mount/unmount/remount cancels the first
    // measurement pass, so this guards against the once-per-doc guard being set
    // up front (which previously made the remount skip measuring entirely).
    render(
      <StrictMode>
        <PdfViewer {...defaultProps} />
      </StrictMode>,
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    await waitFor(() => expect(screen.getByText("125%")).toBeInTheDocument());
  });

  it("leaves documents with comfortable body text at 100%", async () => {
    // 12pt body on a small 439pt page is blown up ~2x by fit-to-width, well
    // above target, so the computed zoom is floored to 1.0 (never shrink).
    mockPdfDoc.value = sizedDoc(12, 439);

    render(<PdfViewer {...defaultProps} />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("does not auto-zoom (nor flicker) when text is only marginally small", async () => {
    // 16pt body on a 900pt page renders ~16px at the reference fit -> raw zoom
    // 16.5/16 = 1.03x, inside the deadband, so no setZoom fires.
    mockPdfDoc.value = sizedDoc(16, 900);

    render(<PdfViewer {...defaultProps} />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("does not auto-zoom a textless (scanned) document", async () => {
    // mockPdfDoc defaults to empty text content -> no font samples.
    render(<PdfViewer {...defaultProps} />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("uses an opaque white canvas background so PDF composition stays stable", async () => {
    document.documentElement.classList.add("dark");

    render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockPage).toHaveBeenCalled();
    expect(mockPage.mock.calls[0][0].canvasBackground).toBe("white");
  });

  it("renders highlight bounding box", async () => {
    render(<PdfViewer {...defaultProps} />);
    
    // Wait for async load success to set scale
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });
    
    // The highlight div should be present. It has background color rgba(250, 204, 21, 0.25)
    const highlight = document.querySelector('div[style*="background-color: rgba(250, 204, 21, 0.25)"]');
    expect(highlight).toBeInTheDocument();
  });

  it("emphasises the navigation target per-line when highlight_rects is provided", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        highlight_rects={[
          { x: 5, y: 5, width: 30, height: 8 },
          { x: 5, y: 15, width: 12, height: 8 },
        ]}
      />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    // Precise per-line emphasis is drawn instead of the single union box.
    const targets = screen.getAllByTestId("target-highlight");
    expect(targets).toHaveLength(2);
    expect(targets[0]).toHaveStyle({ left: "5px", top: "5px", width: "30px", height: "8px" });
    // The coarse union overlay must not also be present.
    expect(
      document.querySelectorAll('div[style*="background-color: rgba(250, 204, 21, 0.25)"]'),
    ).toHaveLength(2);
  });

  it("renders persisted bookmark highlights with scaled PDF coordinates", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        bookmarkHighlights={[
          { id: "bookmark-1", page: 1, rects: [{ x: 20, y: 30, width: 40, height: 10 }] },
          { id: "bookmark-2", page: 3, rects: [{ x: 1, y: 2, width: 3, height: 4 }] },
        ]}
      />,
    );

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    const highlights = screen.getAllByTestId("bookmark-highlight");
    expect(highlights).toHaveLength(2);
    expect(highlights[0]).toHaveStyle({
      left: "20px",
      top: "30px",
      width: "40px",
      height: "10px",
    });
  });

  it("shows the selection action below the selected text", async () => {
    render(<PdfViewer {...defaultProps} onAddBookmark={vi.fn()} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    const scrollContainer = document.querySelector(".overflow-auto") as HTMLElement;
    const root = document.querySelector(".h-full.relative") as HTMLElement;
    const pageWrapper = document.querySelector<HTMLElement>("[data-page-number='1']")!;

    root.getBoundingClientRect = () =>
      ({ top: 10, left: 20, width: 500, height: 500, bottom: 510, right: 520, x: 20, y: 10, toJSON: () => ({}) }) as DOMRect;
    pageWrapper.getBoundingClientRect = () =>
      ({ top: 50, left: 40, width: 600, height: 800, bottom: 850, right: 640, x: 40, y: 50, toJSON: () => ({}) }) as DOMRect;

    const selectionDomRect = {
      top: 70,
      left: 60,
      width: 100,
      height: 20,
      bottom: 90,
      right: 160,
      x: 60,
      y: 70,
      toJSON: () => ({}),
    } as DOMRect;
    const range = {
      startContainer: pageWrapper,
      getBoundingClientRect: () => selectionDomRect,
      getClientRects: () => [selectionDomRect] as unknown as DOMRectList,
    };
    vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => "selected text",
      removeAllRanges: vi.fn(),
    } as any);

    fireEvent.mouseUp(scrollContainer);

    const button = screen.getByRole("button", { name: "+ Bookmark" });
    expect(button.closest(".absolute")).toHaveStyle({ top: "83px", left: "40px" });
  });

  it("runs explain and inline ask actions for the selected text", async () => {
    const onExplainSelection = vi.fn();
    const onAskSelection = vi.fn();
    render(
      <PdfViewer
        {...defaultProps}
        onAddBookmark={vi.fn()}
        showChatSelectionActions
        onExplainSelection={onExplainSelection}
        onAskSelection={onAskSelection}
      />,
    );

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    const scrollContainer = document.querySelector(".overflow-auto") as HTMLElement;
    const root = document.querySelector(".h-full.relative") as HTMLElement;
    const pageWrapper = document.querySelector<HTMLElement>("[data-page-number='1']")!;

    root.getBoundingClientRect = () =>
      ({ top: 10, left: 20, width: 500, height: 500, bottom: 510, right: 520, x: 20, y: 10, toJSON: () => ({}) }) as DOMRect;
    pageWrapper.getBoundingClientRect = () =>
      ({ top: 50, left: 40, width: 600, height: 800, bottom: 850, right: 640, x: 40, y: 50, toJSON: () => ({}) }) as DOMRect;

    const selectionDomRect = {
      top: 70,
      left: 60,
      width: 100,
      height: 20,
      bottom: 90,
      right: 160,
      x: 60,
      y: 70,
      toJSON: () => ({}),
    } as DOMRect;
    const range = {
      startContainer: pageWrapper,
      getBoundingClientRect: () => selectionDomRect,
      getClientRects: () => [selectionDomRect] as unknown as DOMRectList,
    };
    const removeAllRanges = vi.fn();
    vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => "selected text",
      removeAllRanges,
    } as any);

    fireEvent.mouseUp(scrollContainer);
    fireEvent.click(screen.getByRole("button", { name: "Explain" }));

    expect(onExplainSelection).toHaveBeenCalledWith(
      expect.objectContaining({ quote: "selected text" }),
    );
    expect(removeAllRanges).toHaveBeenCalled();

    fireEvent.mouseUp(scrollContainer);
    fireEvent.click(screen.getByRole("button", { name: "Ask about this" }));
    fireEvent.change(screen.getByPlaceholderText("Ask about this…"), {
      target: { value: "Why is this important?" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(onAskSelection).toHaveBeenCalledWith(
      expect.objectContaining({ quote: "selected text" }),
      "Why is this important?",
    );
  });

  it("centers the ping animation on the highlighted match", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        highlight_bbox={{ x: 10, y: 20, width: 40, height: 10 }}
      />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    const ping = document.querySelector(".animate-ping") as HTMLElement | null;
    expect(ping).toBeInTheDocument();
    expect(ping?.style.left).toBe("10px");
    expect(ping?.style.top).toBe("5px");
  });

  it("updates the page indicator while scrolling", async () => {
    render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    const scrollContainer = document.querySelector(".overflow-auto");
    expect(scrollContainer).toBeInTheDocument();

    scrollContainer!.getBoundingClientRect = () =>
      ({ top: 0, height: 1000, bottom: 1000, left: 0, right: 0, width: 0, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;

    const pageWrappers = Array.from(document.querySelectorAll<HTMLElement>("[data-page-number]"));
    expect(pageWrappers).toHaveLength(3);

    const rects = new Map([
      ["1", { top: -1600, height: 800 }],
      ["2", { top: -700, height: 800 }],
      ["3", { top: 200, height: 800 }],
    ]);

    for (const pageWrapper of pageWrappers) {
      const rect = rects.get(pageWrapper.dataset.pageNumber!);
      pageWrapper.getBoundingClientRect = () =>
        ({
          top: rect!.top,
          height: rect!.height,
          bottom: rect!.top + rect!.height,
          left: 0,
          right: 0,
          width: 0,
          x: 0,
          y: rect!.top,
          toJSON: () => ({}),
        }) as DOMRect;
    }

    fireEvent.scroll(scrollContainer!);

    await waitFor(() => {
      expect(screen.getByText("3/10")).toBeInTheDocument();
    });
  });

  it("scrolls to target page when metrics arrive after mount", async () => {
    // Regression: prevNavigationTargetRef was set unconditionally in the scroll
    // effect, even when hasPageMetrics was false. This meant that when metrics
    // later became available the effect saw navigationChanged === false and
    // skipped the scroll entirely, leaving pages beyond the initial viewport
    // (roughly page 4+) unreachable on first load.
    mockUsePdfPageMetrics.value = {
      pageMetrics: [],
      isLoadingPageMetrics: true,
      hasPageMetrics: false,
    };

    const onRenderSuccess = vi.fn();
    const { rerender } = render(
      <PdfViewer url="test.pdf" page={7} highlight_bbox={null} onRenderSuccess={onRenderSuccess} />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    // No scroll while metrics are pending
    expect(mockVirtualizer.scrollToIndex).not.toHaveBeenCalled();

    // Metrics arrive
    mockUsePdfPageMetrics.value = {
      pageMetrics: Array.from({ length: 10 }, () => ({ width: 600, height: 800 })),
      isLoadingPageMetrics: false,
      hasPageMetrics: true,
    };

    rerender(
      <PdfViewer url="test.pdf" page={7} highlight_bbox={null} onRenderSuccess={onRenderSuccess} />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    // Must scroll to page 7 (0-based index 6)
    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(6, { align: "start" });
  });

  it("restores the remembered position when a document is reopened plainly", async () => {
    // A prior session left this document at page 3. Reopening it as a plain open
    // (page 1, no highlight target) must land back on page 3, not page 1.
    savePdfScrollPosition("remembered.pdf", { page: 3, offsetRatio: 0, zoom: 1 });

    render(<PdfViewer url="remembered.pdf" page={1} highlight_bbox={null} onRenderSuccess={vi.fn()} />);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(2, { align: "start" });
    expect(mockVirtualizer.scrollToIndex).not.toHaveBeenCalledWith(0, { align: "start" });
  });

  it("restores the remembered zoom and does not re-run auto-zoom on reopen", async () => {
    // Regression: auto-zoom used to re-run on every reopen and, applied after the
    // scroll position was restored, grew page heights and shifted the reader
    // upward. A remembered zoom is now restored synchronously and auto-zoom is
    // skipped, so the view opens exactly where (and how zoomed) it was left --
    // here 150%, not the ~125% the body-text measurement would compute.
    savePdfScrollPosition("zoomed.pdf", { page: 3, offsetRatio: 0, zoom: 1.5 });
    mockPdfDoc.value = sizedDoc(9, 612);

    render(<PdfViewer url="zoomed.pdf" page={1} highlight_bbox={null} onRenderSuccess={vi.fn()} />);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("150%")).toBeInTheDocument();
    expect(screen.queryByText("125%")).not.toBeInTheDocument();
  });

  it("clears the loading overlay via the remembered landing page when page 1 is off-screen", async () => {
    // Regression: the loading overlay (owned by PreviewPane) is cleared by
    // onRenderSuccess, which used to fire only for props.page (=1 on a plain
    // open). When a remembered position lands the viewer deep in the document,
    // page 1 never enters the render window, so the callback never fired and the
    // spinner hung until app restart. It must now fire for the page we land on.
    savePdfScrollPosition("deep.pdf", { page: 5, offsetRatio: 0, zoom: 1 });
    mockUsePdfPageMetrics.value = {
      pageMetrics: Array.from({ length: 10 }, () => ({ width: 600, height: 800 })),
      isLoadingPageMetrics: false,
      hasPageMetrics: true,
    };
    // Only pages 4, 5, 6 are rendered -- page 1 is nowhere in the DOM.
    mockVirtualizer.getVirtualItems = () => [
      { index: 3, key: "3", start: 2700 },
      { index: 4, key: "4", start: 3600 },
      { index: 5, key: "5", start: 4500 },
    ];

    const onRenderSuccess = vi.fn();
    render(
      <PdfViewer url="deep.pdf" page={1} highlight_bbox={null} onRenderSuccess={onRenderSuccess} />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(onRenderSuccess).toHaveBeenCalled();
  });

  it("lets an explicit navigation target win over the remembered position", async () => {
    // Same remembered page 5, but this open carries an explicit highlight target
    // (a search hit / bookmark). The explicit destination must win.
    savePdfScrollPosition("explicit.pdf", { page: 3, offsetRatio: 0, zoom: 1 });

    render(
      <PdfViewer
        url="explicit.pdf"
        page={2}
        highlight_bbox={{ x: 1, y: 1, width: 2, height: 2 }}
        onRenderSuccess={vi.fn()}
      />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(1, { align: "start" });
    expect(mockVirtualizer.scrollToIndex).not.toHaveBeenCalledWith(2, { align: "start" });
  });

  it("does not snap back to the original page when inner search closes", async () => {
    mockUsePdfInnerSearch.value = {
      ...mockUsePdfInnerSearch.value,
      isSearchOpen: true,
    };

    const { rerender } = render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledTimes(0);

    mockUsePdfInnerSearch.value = {
      ...mockUsePdfInnerSearch.value,
      isSearchOpen: false,
    };

    rerender(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledTimes(0);
  });

  it("shows a disabled TOC button when the document has no outline", async () => {
    mockUsePdfOutline.value = null;
    render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    const button = screen.getByTitle("This document has no table of contents");
    expect(button).toBeDisabled();
  });

  it("opens the outline panel when the TOC button is clicked", async () => {
    mockUsePdfOutline.value = [{ title: "Chapter 1", dest: "ch1", url: null, items: [] }];
    render(<PdfViewer {...defaultProps} />);

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.queryByTestId("pdf-outline-panel")).not.toBeInTheDocument();
    const button = screen.getByTitle("Table of contents");
    expect(button).toBeEnabled();

    fireEvent.click(button);
    expect(screen.getByTestId("pdf-outline-panel")).toBeInTheDocument();
  });
});
