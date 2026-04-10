import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PdfViewer from "./PdfViewer";

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

// Mock react-pdf
vi.mock("react-pdf", () => ({
  Document: ({ children, onLoadSuccess }: any) => {
    // Simulate loading success
    if (onLoadSuccess) {
      setTimeout(() => onLoadSuccess({ numPages: 10 }), 0);
    }
    return <div data-testid="pdf-document">{children}</div>;
  },
  Page: ({ pageNumber, onLoadSuccess, onRenderSuccess }: any) => {
    if (onLoadSuccess && pageNumber === 1) {
      setTimeout(() => onLoadSuccess({ getViewport: () => ({ width: 600, height: 800 }) }), 0);
    }
    if (onRenderSuccess) {
      setTimeout(() => onRenderSuccess(), 0);
    }
    return <div data-testid={`pdf-page-${pageNumber}`} />;
  },
  pdfjs: { GlobalWorkerOptions: { workerSrc: "" } },
}));

// Mock @tanstack/react-virtual
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: vi.fn().mockReturnValue(mockVirtualizer),
}));

// Mock ResizeObserver
global.ResizeObserver = class {
    observe = vi.fn();
    unobserve = vi.fn();
    disconnect = vi.fn();
} as any;

describe("PdfViewer", () => {
  const defaultProps = {
    url: "test.pdf",
    page: 1,
    highlight_bbox: { x: 10, y: 10, width: 50, height: 20 },
    onRenderSuccess: vi.fn(),
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockVirtualizer.getVirtualItems = () => [
      { index: 0, key: "0", start: 0 },
      { index: 1, key: "1", start: 900 },
      { index: 2, key: "2", start: 1800 },
    ];
    global.requestAnimationFrame = ((cb: FrameRequestCallback) => {
      cb(0);
      return 0;
    }) as typeof requestAnimationFrame;
  });

  it("renders correctly and handles load success", async () => {
    render(<PdfViewer {...defaultProps} />);
    expect(screen.getByTestId("pdf-document")).toBeInTheDocument();
    
    // Wait for async load success
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });
    
    expect(screen.getByText("100%")).toBeInTheDocument();
    expect(screen.getByText("1/10")).toBeInTheDocument();
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
});
