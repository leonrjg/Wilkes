import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PdfViewer from "./PdfViewer";

// Mock react-pdf
vi.mock("react-pdf", () => ({
  Document: ({ children }: any) => <div data-testid="pdf-document">{children}</div>,
  Page: () => <div data-testid="pdf-page" />,
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

describe("PdfViewer", () => {
  const defaultProps = {
    url: "test.pdf",
    page: 1,
    highlight_bbox: null,
  };

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders correctly", () => {
    render(<PdfViewer {...defaultProps} />);
    expect(screen.getByTestId("pdf-document")).toBeInTheDocument();
  });

  it("toggles search input", () => {
    render(<PdfViewer {...defaultProps} />);
    const searchBtn = screen.getByTitle(/Find in document/);
    fireEvent.click(searchBtn);
    expect(screen.getByPlaceholderText("Find in document...")).toBeInTheDocument();
  });

  it("zooms in and out", () => {
    render(<PdfViewer {...defaultProps} />);
    const zoomIn = screen.getByText("+");
    const zoomOut = screen.getByText("−");
    
    fireEvent.click(zoomIn);
    expect(screen.getByText("125%")).toBeInTheDocument();
    
    fireEvent.click(zoomOut);
    expect(screen.getByText("100%")).toBeInTheDocument();
  });
});
