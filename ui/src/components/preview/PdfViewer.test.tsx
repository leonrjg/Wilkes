import { screen, fireEvent, act, waitFor } from "@testing-library/react";
import { renderWithReaderHost as render, stubSelectionSlot } from "./testing/readerHarness";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { createRef, StrictMode } from "react";
import PdfViewer, { type PdfReaderHandle } from "./PdfViewer";
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
      matches: [] as { page: number; bbox: unknown }[],
      isSearching: false,
    },
  },
}));

const { mockUseDocumentFind } = vi.hoisted(() => ({
  mockUseDocumentFind: {
    value: {
      inputRef: { current: null },
      isOpen: false,
      open: vi.fn(),
      close: vi.fn(),
      query: "",
      setQuery: vi.fn(),
      currentIdx: -1,
      next: vi.fn(),
      prev: vi.fn(),
      onInputKeyDown: vi.fn(),
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

const { mockUsePdfSearchResult } = vi.hoisted(() => ({
  mockUsePdfSearchResult: {
    value: null as null | {
      page: number;
      bbox: { x: number; y: number; width: number; height: number };
      rects: Array<{ x: number; y: number; width: number; height: number }>;
      contextScore: number;
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

// Mock the page canvas: it drives a real pdf.js render, which jsdom has no
// canvas for. These are rendering/navigation tests -- what the canvas paints is
// out of scope, only that the reader asks for the right page at the right size.
vi.mock("./PdfPageCanvas", () => ({
  default: (props: any) => mockPage(props),
}));

// The worker URL and AnnotationMode are all PdfViewer itself needs from pdf.js.
vi.mock("pdfjs-dist", async () => ({
  ...(await vi.importActual<Record<string, unknown>>("pdfjs-dist")),
  GlobalWorkerOptions: { workerSrc: "" },
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

vi.mock("./usePdfSearchResult", () => ({
  usePdfSearchResult: vi.fn(() => mockUsePdfSearchResult.value),
}));

vi.mock("./useDocumentFind", () => ({
  useDocumentFind: vi.fn(() => mockUseDocumentFind.value),
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

function domRect(top: number, left: number, width: number, height: number): DOMRect {
  return {
    top,
    left,
    width,
    height,
    bottom: top + height,
    right: left + width,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

function deferAnimationFrames() {
  let nextId = 1;
  const pending = new Map<number, FrameRequestCallback>();
  global.requestAnimationFrame = vi.fn((callback: FrameRequestCallback) => {
    const id = nextId++;
    pending.set(id, callback);
    return id;
  });
  global.cancelAnimationFrame = vi.fn((id: number) => {
    pending.delete(id);
  });
  return {
    flush: () => {
      const callbacks = [...pending.values()];
      pending.clear();
      callbacks.forEach((callback) => callback(0));
    },
  };
}

describe("PdfViewer", () => {
  const defaultProps = {
    url: "test.pdf",
    page: 1,
    highlight_bbox: { x: 10, y: 10, width: 50, height: 20 },
    onRenderSuccess: vi.fn(),
  };
  // The auto-zoom target is a host setting; most tests want the default one.
  const defaultHost = { pdfAutoZoomTargetPx: 15.5 };

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
      matches: [],
      isSearching: false,
    };
    mockUsePdfSearchResult.value = null;
    mockUseDocumentFind.value = {
      inputRef: { current: null },
      isOpen: false,
      open: vi.fn(),
      close: vi.fn(),
      query: "",
      setQuery: vi.fn(),
      currentIdx: -1,
      next: vi.fn(),
      prev: vi.fn(),
      onInputKeyDown: vi.fn(),
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
    global.cancelAnimationFrame = vi.fn();
  });

  it("renders correctly and handles load success", async () => {
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });
    expect(screen.getByTestId("pdf-page-1")).toBeInTheDocument();

    // Wait for async load success
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });
    
    expect(screen.getByText("100%")).toBeInTheDocument();
    expect(screen.getByText("1/10")).toBeInTheDocument();
  });

  it("changes zoom in 10 percent steps", async () => {
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));
    expect(screen.getByText("110%")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
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
    // reference fit, below the 15.5px target -> 15.5 / 13.235 ≈ 1.17x.
    mockPdfDoc.value = sizedDoc(9, 612);

    // Render under StrictMode: its mount/unmount/remount cancels the first
    // measurement pass, so this guards against the once-per-doc guard being set
    // up front (which previously made the remount skip measuring entirely).
    render(
      <StrictMode>
        <PdfViewer {...defaultProps} />
      </StrictMode>,
      { host: defaultHost },
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    await waitFor(() => expect(screen.getByText("117%")).toBeInTheDocument());
  });

  it("uses the configured auto-zoom target", async () => {
    mockPdfDoc.value = sizedDoc(9, 612);

    render(<PdfViewer {...defaultProps} url="configured-target.pdf" />, {
      host: { pdfAutoZoomTargetPx: 18 },
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    await waitFor(() => expect(screen.getByText("136%")).toBeInTheDocument());
  });

  it("leaves documents with comfortable body text at 100%", async () => {
    // 12pt body on a small 439pt page is blown up ~2x by fit-to-width, well
    // above target, so the computed zoom is floored to 1.0 (never shrink).
    mockPdfDoc.value = sizedDoc(12, 439);

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("does not auto-zoom (nor flicker) when text is only marginally small", async () => {
    // 16pt body on a 900pt page renders ~16px at the reference fit -> raw zoom
    // 15.5/16 < 1x, inside the deadband, so no setZoom fires.
    mockPdfDoc.value = sizedDoc(16, 900);

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("does not auto-zoom a textless (scanned) document", async () => {
    // mockPdfDoc defaults to empty text content -> no font samples.
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.getByText("100%")).toBeInTheDocument();
  });

  it("uses an opaque white canvas background so PDF composition stays stable", async () => {
    document.documentElement.classList.add("dark");

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockPage).toHaveBeenCalled();
    expect(mockPage.mock.calls[0][0].canvasBackground).toBe("white");
  });

  it("locates the navigation target without drawing it", async () => {
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    // Wait for async load success to set scale
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    // A host that knows where the target is draws it as a decoration. If the
    // reader drew `highlight_bbox` as well, a bookmarked target would carry
    // two stacked marks. The ping still points at it.
    expect(document.querySelector("div.pdf-highlight")).not.toBeInTheDocument();
    expect(document.querySelector("div.pdf-highlight-ping")).toBeInTheDocument();
    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalled();
  });

  it("reveals the active inner-search match within its page", () => {
    const frames = deferAnimationFrames();
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
      currentIdx: 0,
    };
    mockUsePdfInnerSearch.value = {
      matches: [{ page: 2, bbox: { x: 100, y: 700, width: 80, height: 20 } }],
      isSearching: false,
    };

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(1, { align: "start" });
    const container = document.querySelector<HTMLElement>(".overflow-auto")!;
    const page = document.querySelector<HTMLElement>('[data-page-number="2"]')!;
    container.scrollTop = 900;
    container.scrollLeft = 25;
    container.getBoundingClientRect = () => domRect(0, 0, 600, 400);
    page.getBoundingClientRect = () => domRect(0, 0, 600, 800);

    act(() => frames.flush());

    // Match centre is y=710, so it is centred on the 400px-tall viewport.
    expect(container.scrollTop).toBe(1410);
    // Its x range is already comfortably visible; vertical navigation must not
    // introduce unrelated lateral movement.
    expect(container.scrollLeft).toBe(25);
  });

  it("does not move an inner-search match that is already comfortably visible", () => {
    const frames = deferAnimationFrames();
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
      currentIdx: 0,
    };
    mockUsePdfInnerSearch.value = {
      matches: [{ page: 1, bbox: { x: 100, y: 100, width: 80, height: 20 } }],
      isSearching: false,
    };

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    const container = document.querySelector<HTMLElement>(".overflow-auto")!;
    const page = document.querySelector<HTMLElement>('[data-page-number="1"]')!;
    container.scrollTop = 300;
    container.scrollLeft = 20;
    container.getBoundingClientRect = () => domRect(0, 0, 600, 400);
    page.getBoundingClientRect = () => domRect(50, 0, 600, 800);

    act(() => frames.flush());

    expect(container.scrollTop).toBe(300);
    expect(container.scrollLeft).toBe(20);
  });

  it("reveals a horizontally hidden inner-search match after zooming in", () => {
    const frames = deferAnimationFrames();
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
      currentIdx: 0,
    };
    mockUsePdfInnerSearch.value = {
      matches: [{ page: 1, bbox: { x: 520, y: 100, width: 40, height: 20 } }],
      isSearching: false,
    };

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });
    fireEvent.click(screen.getByRole("button", { name: "Zoom in" }));

    const container = document.querySelector<HTMLElement>(".overflow-auto")!;
    const page = document.querySelector<HTMLElement>('[data-page-number="1"]')!;
    container.scrollTop = 200;
    container.scrollLeft = 30;
    container.getBoundingClientRect = () => domRect(0, 0, 600, 400);
    // At 110%, the 600-unit page is 660px wide and its left edge is already
    // shifted by the existing 30px horizontal scroll.
    page.getBoundingClientRect = () => domRect(50, -30, 660, 880);

    act(() => frames.flush());

    // Scaled match centre: -30 + (520 + 20) * 1.1 = 564. Centre it against
    // viewport x=300, starting from scrollLeft=30.
    expect(container.scrollLeft).toBeCloseTo(294);
    // The match is vertically visible, so horizontal reveal leaves y alone.
    expect(container.scrollTop).toBe(200);
  });

  it("cancels a stale deferred reveal when the active match changes", () => {
    const frames = deferAnimationFrames();
    const matches = [
      { page: 1, bbox: { x: 100, y: 700, width: 80, height: 20 } },
      { page: 1, bbox: { x: 100, y: 100, width: 80, height: 20 } },
    ];
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
      currentIdx: 0,
    };
    mockUsePdfInnerSearch.value = { matches, isSearching: false };

    const { rerender } = render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      currentIdx: 1,
    };
    rerender(<PdfViewer {...defaultProps} />, { host: defaultHost });

    const container = document.querySelector<HTMLElement>(".overflow-auto")!;
    const page = document.querySelector<HTMLElement>('[data-page-number="1"]')!;
    container.scrollTop = 300;
    container.getBoundingClientRect = () => domRect(0, 0, 600, 400);
    page.getBoundingClientRect = () => domRect(50, 0, 600, 800);

    act(() => frames.flush());

    // Only the second match's frame survives, and that match is already visible.
    // If the first frame were allowed to run it would move scrollTop to 860.
    expect(container.scrollTop).toBe(300);
    expect(global.cancelAnimationFrame).toHaveBeenCalled();
  });

  it("draws a decorated target once, not once per mechanism", async () => {
    const rects = [
      { x: 5, y: 5, width: 30, height: 8 },
      { x: 5, y: 15, width: 12, height: 8 },
    ];
    render(
      <PdfViewer
        {...defaultProps}
        decorations={[
          {
            id: "bookmark-1",
            anchor: { kind: "rects", page: defaultProps.page, rects },
            className: "pdf-highlight--bookmark",
          },
        ]}
      />,
      { host: defaultHost },
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    // Two rects, two marks. The reader adding its own emphasis for the same
    // navigation target is what stacked a second colour underneath.
    expect(document.querySelectorAll("div.pdf-highlight")).toHaveLength(2);
    expect(
      document.querySelectorAll("div.pdf-highlight--bookmark"),
    ).toHaveLength(2);
    expect(screen.queryAllByTestId("target-highlight")).toHaveLength(0);
  });

  it("uses a PDF.js-localized search page and rectangles instead of the coarse origin", async () => {
    mockUsePdfSearchResult.value = {
      page: 2,
      bbox: { x: 20, y: 25, width: 110, height: 23 },
      rects: [
        { x: 100, y: 25, width: 30, height: 8 },
        { x: 20, y: 40, width: 90, height: 8 },
      ],
      contextScore: 40,
    };

    render(
      <PdfViewer
        {...defaultProps}
        page={1}
        search_locator={{
          matched_text: "reason-\nable by reasonable people",
          context_before: "found to be ",
          context_after: ". An effort",
        }}
      />,
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(1, { align: "start" });
    const targets = screen.getAllByTestId("target-highlight");
    expect(targets).toHaveLength(2);
    expect(targets[0]).toHaveStyle({ left: "100px", top: "25px", width: "30px" });
  });

  it("applies the dark treatment from the host, not from the document", async () => {
    // Contradictory state on the document: if the reader were still reading the
    // class it would come out light here.
    document.documentElement.classList.remove("dark");
    document.documentElement.classList.add("light");

    const { setHost } = render(<PdfViewer {...defaultProps} />, {
      host: { ...defaultHost, colorScheme: "dark" },
    });
    // `.pdf-dark-mode .pdf-page canvas` is what inverts the page.
    expect(document.querySelector(".pdf-dark-mode")).not.toBeNull();

    setHost({ colorScheme: "light" });
    expect(document.querySelector(".pdf-dark-mode")).toBeNull();

    document.documentElement.classList.remove("light");
  });

  it("renders host content over a decoration's union box", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        decorations={[
          {
            id: "coverage-1",
            anchor: {
              kind: "rects",
              page: 1,
              rects: [
                { x: 10, y: 20, width: 30, height: 10 },
                { x: 10, y: 40, width: 50, height: 10 },
              ],
            },
            render: ({ page, box }) => <span>{`p${page} w${box.width}`}</span>,
          },
        ]}
      />,
    );

    const content = document.querySelector<HTMLElement>("[data-decoration-content='coverage-1']")!;
    // Union of the two rects, not either one of them.
    expect(content).toHaveStyle({ left: "10px", top: "20px", width: "50px", height: "30px" });
    expect(content).toHaveTextContent("p1 w50");
  });

  it("renders the page gutter slot beside every mounted page", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        slots={{ pageGutter: (page, { scale }) => <span>{`note ${page} @${scale}`}</span> }}
      />,
    );

    const gutters = document.querySelectorAll("[data-page-gutter]");
    expect(gutters).toHaveLength(3);
    expect(gutters[0]).toHaveTextContent("note 1 @1");
    // Positioned at the page's right edge, reserving no width of its own.
    expect(gutters[0]).toHaveStyle({ position: "absolute", left: "100%", top: "0px" });
  });

  it("renders the toolbar slot inside the reader's control cluster", () => {
    render(
      <PdfViewer {...defaultProps} slots={{ toolbar: <button>Review</button> }} />,
    );

    const toolbarButton = screen.getByRole("button", { name: "Review" });
    expect(toolbarButton.closest(".shadow-lg")).toContainElement(
      screen.getByRole("button", { name: "Zoom in" }),
    );
  });

  it("navigates, zooms and finds through the imperative handle", async () => {
    const handle = createRef<PdfReaderHandle>();
    render(
      <PdfViewer
        {...defaultProps}
        ref={handle}
        decorations={[
          {
            id: "finding-3",
            anchor: { kind: "rects", page: 3, rects: [{ x: 5, y: 6, width: 7, height: 8 }] },
          },
        ]}
      />,
    );

    expect(handle.current!.getPageCount()).toBe(10);

    act(() => handle.current!.goToPage(2));
    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(1, { align: "start" });
    expect(handle.current!.getCurrentPage()).toBe(2);

    // The same destination twice must navigate twice -- the reason this is a
    // command rather than a prop.
    mockVirtualizer.scrollToIndex.mockClear();
    act(() => handle.current!.goToPage(2));
    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(1, { align: "start" });

    act(() => handle.current!.scrollToDecoration("finding-3"));
    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledWith(2, { align: "start" });
    expect(handle.current!.getCurrentPage()).toBe(3);

    act(() => handle.current!.setZoom(1.5));
    expect(handle.current!.getZoom()).toBe(1.5);
    // Clamped to the reader's own limits, not the caller's.
    act(() => handle.current!.setZoom(99));
    expect(handle.current!.getZoom()).toBe(3);

    act(() => handle.current!.openFind("invariant"));
    expect(mockUseDocumentFind.value.setQuery).toHaveBeenCalledWith("invariant");
    expect(mockUseDocumentFind.value.open).toHaveBeenCalled();
  });

  it("ignores decorations anchored in a coordinate system it cannot place", () => {
    render(
      <PdfViewer
        {...defaultProps}
        decorations={[
          { id: "text-anchored", anchor: { kind: "range", range: { start: 0, end: 5 } } },
        ]}
      />,
    );

    expect(screen.queryByTestId("decoration")).not.toBeInTheDocument();
  });

  it("renders persisted bookmark highlights with scaled PDF coordinates", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        decorations={[
          {
            id: "bookmark-1",
            anchor: { kind: "rects", page: 1, rects: [{ x: 20, y: 30, width: 40, height: 10 }] },
            className: "pdf-highlight--bookmark",
          },
          {
            id: "bookmark-2",
            anchor: { kind: "rects", page: 3, rects: [{ x: 1, y: 2, width: 3, height: 4 }] },
            className: "pdf-highlight--bookmark",
          },
        ]}
      />,
    );

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    const highlights = screen.getAllByTestId("decoration");
    expect(highlights).toHaveLength(2);
    expect(highlights[0]).toHaveStyle({
      left: "20px",
      top: "30px",
      width: "40px",
      height: "10px",
    });
    // Stacking and palette come from the shared class, not inline styles.
    expect(highlights[0]).toHaveClass("pdf-highlight", "pdf-highlight--bookmark");
  });

  it("opens a persisted bookmark highlight", async () => {
    const onActivate = vi.fn();
    render(
      <PdfViewer
        {...defaultProps}
        decorations={[
          {
            id: "bookmark-1",
            anchor: { kind: "rects", page: 1, rects: [{ x: 20, y: 30, width: 40, height: 10 }] },
            className: "pdf-highlight--bookmark",
            onActivate,
          },
        ]}
      />,
    );

    const highlight = await screen.findByTestId("decoration");
    fireEvent.click(highlight);
    expect(onActivate).toHaveBeenCalledWith("bookmark-1", {
      left: 0,
      top: 0,
      right: 0,
      bottom: 0,
    });
  });

  it("shows the selection action below and to the right of the selected text", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        slots={{ selectionActions: stubSelectionSlot() }}
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
      endContainer: pageWrapper,
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

    const button = screen.getByRole("button", { name: "Stub action" });
    expect(button.closest(".absolute")).toHaveStyle({ top: "83px", left: "140px" });
  });

  it("anchors the selection action to the final line rather than the range bounding box", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        slots={{ selectionActions: stubSelectionSlot() }}
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

    const firstLineRect = { top: 70, left: 60, width: 300, height: 20, bottom: 90, right: 360, x: 60, y: 70, toJSON: () => ({}) } as DOMRect;
    const finalLineRect = { top: 95, left: 60, width: 100, height: 20, bottom: 115, right: 160, x: 60, y: 95, toJSON: () => ({}) } as DOMRect;
    const range = {
      startContainer: pageWrapper,
      endContainer: pageWrapper,
      getBoundingClientRect: () => firstLineRect,
      getClientRects: () => [firstLineRect, finalLineRect] as unknown as DOMRectList,
    };
    vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => "selected text",
      removeAllRanges: vi.fn(),
    } as any);

    fireEvent.mouseUp(scrollContainer);

    expect(screen.getByRole("button", { name: "Stub action" }).closest(".absolute")).toHaveStyle({
      top: "108px",
      left: "140px",
    });
  });

  it("holds the selection chrome open while the host has pinned it", async () => {
    render(
      <PdfViewer
        {...defaultProps}
        slots={{ selectionActions: stubSelectionSlot() }}
      />,
      { host: defaultHost },
    );

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    const scrollContainer = document.querySelector(".overflow-auto") as HTMLElement;
    const root = document.querySelector(".h-full.relative") as HTMLElement;
    const pageWrapper = document.querySelector<HTMLElement>("[data-page-number='1']")!;

    root.getBoundingClientRect = () =>
      ({ top: 10, left: 20, width: 500, height: 500, bottom: 510, right: 520, x: 20, y: 10, toJSON: () => ({}) }) as DOMRect;
    pageWrapper.getBoundingClientRect = () =>
      ({ top: 50, left: 40, width: 600, height: 800, bottom: 850, right: 640, x: 40, y: 50, toJSON: () => ({}) }) as DOMRect;

    const selectionDomRect = {
      top: 70, left: 60, width: 100, height: 20,
      bottom: 90, right: 160, x: 60, y: 70, toJSON: () => ({}),
    } as DOMRect;
    const range = {
      startContainer: pageWrapper,
      endContainer: pageWrapper,
      getBoundingClientRect: () => selectionDomRect,
      getClientRects: () => [selectionDomRect] as unknown as DOMRectList,
    };
    const liveSelection = {
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => "selected text",
      removeAllRanges: vi.fn(),
    };
    vi.spyOn(window, "getSelection").mockReturnValue(liveSelection as never);

    fireEvent.mouseUp(scrollContainer);
    const input = screen.getByPlaceholderText("Stub input");

    // Host chrome that takes focus collapses the document selection. Pinned, the
    // reader must not dismiss -- otherwise focusing the chrome destroys it.
    fireEvent.focus(input);
    liveSelection.isCollapsed = true;
    fireEvent(window.document, new Event("selectionchange"));
    expect(screen.getByPlaceholderText("Stub input")).toBeInTheDocument();

    // Unpinned, a collapsed selection dismisses as usual.
    fireEvent.blur(input);
    fireEvent(window.document, new Event("selectionchange"));
    await waitFor(() =>
      expect(screen.queryByPlaceholderText("Stub input")).not.toBeInTheDocument(),
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
    // Sized from the shorter side (height 10 x 1.2 = 12), centred on the bbox
    // centre (30, 25).
    expect(ping?.style.width).toBe("12px");
    expect(ping?.style.left).toBe("24px");
    expect(ping?.style.top).toBe("19px");
  });

  it("updates the page indicator while scrolling", async () => {
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

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

    render(<PdfViewer url="remembered.pdf" page={1} highlight_bbox={null} onRenderSuccess={vi.fn()} />, { host: defaultHost });

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

    render(<PdfViewer url="zoomed.pdf" page={1} highlight_bbox={null} onRenderSuccess={vi.fn()} />, { host: defaultHost });
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
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
    };

    const { rerender } = render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledTimes(0);

    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: false,
    };

    rerender(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });

    expect(mockVirtualizer.scrollToIndex).toHaveBeenCalledTimes(0);
  });

  it("positions the find bar at the top of the PDF viewer", () => {
    mockUseDocumentFind.value = {
      ...mockUseDocumentFind.value,
      isOpen: true,
    };

    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    const input = screen.getByPlaceholderText("Find in document...");
    expect(input.closest(".absolute")).toHaveClass("top-4");
    expect(input.closest(".absolute")).not.toHaveClass("bottom-4");
  });

  it("shows a disabled TOC button when the document has no outline", async () => {
    mockUsePdfOutline.value = null;
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    const button = screen.getByRole("button", { name: "This document has no table of contents" });
    expect(button).toBeDisabled();
    expect(button.parentElement).toHaveClass("px-2.5", "py-1.5", "text-sm");
    expect(button.querySelector("svg")).toHaveAttribute("width", "14");
  });

  it("opens the outline panel when the TOC button is clicked", async () => {
    mockUsePdfOutline.value = [{ title: "Chapter 1", dest: "ch1", url: null, items: [] }];
    render(<PdfViewer {...defaultProps} />, { host: defaultHost });

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 10));
    });

    expect(screen.queryByTestId("pdf-outline-panel")).not.toBeInTheDocument();
    const button = screen.getByRole("button", { name: "Table of contents" });
    expect(button).toBeEnabled();

    fireEvent.click(button);
    expect(screen.getByTestId("pdf-outline-panel")).toBeInTheDocument();
  });
});
