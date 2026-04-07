import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PdfViewer from "./PdfViewer";

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
  useVirtualizer: vi.fn().mockReturnValue({
    getTotalSize: () => 1000,
    getVirtualItems: () => [{ index: 0, key: "0", start: 0 }],
    scrollToIndex: vi.fn(),
    measure: vi.fn(),
  }),
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
  });

  it("renders correctly and handles load success", async () => {
    render(<PdfViewer {...defaultProps} />);
    expect(screen.getByTestId("pdf-document")).toBeInTheDocument();
    
    // Wait for async load success
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 10));
    });
    
    expect(screen.getByText("100%")).toBeInTheDocument();
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
});
