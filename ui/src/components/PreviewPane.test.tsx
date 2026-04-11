import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PreviewPane from "./PreviewPane";
import { useSearchStore } from "../stores/useSearchStore";

vi.mock("./preview/CodeViewer", () => ({ default: () => <div data-testid="code-viewer">CodeViewer</div> }));

const mockPdfViewer = vi.fn(() => <div data-testid="pdf-viewer">PdfViewer</div>);
vi.mock("./preview/PdfViewer", () => ({ default: (props: any) => mockPdfViewer(props) }));

describe("PreviewPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSearchStore.setState({
      selectedMatch: null,
      previewData: null,
      previewLoading: false,
      clearPreview: vi.fn(),
    });
  });

  it("renders empty state when no match is selected", () => {
    render(<PreviewPane />);
    expect(screen.getByAltText("Wilkes")).toBeInTheDocument();
  });

  it("renders CodeViewer for text data", () => {
    const mockMatch = { path: "test.txt", origin: { TextFile: { line: 1, col: 1 } } } as any;
    const mockPreviewData = {
      Text: {
        content: "test",
        language: "text",
        highlight_line: 1,
        highlight_range: { start: 0, end: 4 },
      },
    };

    useSearchStore.setState({
      selectedMatch: mockMatch,
      previewData: mockPreviewData,
    });

    render(<PreviewPane />);
    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getAllByText("test.txt")[0]).toBeInTheDocument();
  });

  it("renders PdfViewer for pdf data", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    const mockPreviewData = {
      Pdf: {
        page: 1,
        highlight_bbox: null,
      },
    };

    useSearchStore.setState({
      selectedMatch: mockMatch,
      previewData: mockPreviewData,
    });

    render(<PreviewPane />);
    expect(screen.getByTestId("pdf-viewer")).toBeInTheDocument();
  });

  it("renders PdfViewer using selectedMatch.origin even when previewData is stale", () => {
    // Regression: page/bbox were read from displayData (which could be stale
    // data from a previously viewed file) instead of selectedMatch.origin.
    // This meant PdfViewer could mount with the wrong target page.
    useSearchStore.setState({
      selectedMatch: {
        path: "new-file.pdf",
        origin: { PdfPage: { page: 8, bbox: { x: 1, y: 2, width: 3, height: 4 } } },
      } as any,
      // Stale previewData from a different PDF file (different page)
      previewData: { Pdf: { page: 2, highlight_bbox: null } },
      previewLoading: false,
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("pdf-viewer")).toBeInTheDocument();
    const call = mockPdfViewer.mock.calls[mockPdfViewer.mock.calls.length - 1][0];
    expect(call.page).toBe(8);
    expect(call.highlight_bbox).toEqual({ x: 1, y: 2, width: 3, height: 4 });
  });

  it("renders PdfViewer when selectedMatch is PDF but previewData is stale Text data", () => {
    // Regression: viewer type was determined by displayData ("Text" in displayData),
    // not by selectedMatch.origin. When coming from a text file, the stale
    // displayData would show CodeViewer instead of PdfViewer.
    useSearchStore.setState({
      selectedMatch: {
        path: "report.pdf",
        origin: { PdfPage: { page: 3, bbox: null } },
      } as any,
      // Stale previewData from a text file
      previewData: {
        Text: { content: "old text", language: "text", highlight_line: 1, highlight_range: { start: 0, end: 4 } },
      },
      previewLoading: false,
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("pdf-viewer")).toBeInTheDocument();
    expect(screen.queryByTestId("code-viewer")).not.toBeInTheDocument();
  });

  it("calls clearPreview when close button is clicked", () => {
    const clearPreviewMock = vi.fn();
    useSearchStore.setState({
      selectedMatch: { path: "test.txt", origin: { TextFile: { line: 1, col: 1 } } } as any,
      previewData: { Text: { content: "", language: null, highlight_line: 1, highlight_range: { start: 0, end: 0 } } } as any,
      clearPreview: clearPreviewMock,
    });

    render(<PreviewPane />);
    const closeButton = screen.getByTitle("Close preview");
    fireEvent.click(closeButton);

    expect(clearPreviewMock).toHaveBeenCalled();
  });
});
