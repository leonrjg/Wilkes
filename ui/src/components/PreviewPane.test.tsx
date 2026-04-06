import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PreviewPane from "./PreviewPane";
import { useSearchStore } from "../stores/useSearchStore";

vi.mock("./preview/CodeViewer", () => ({ default: () => <div data-testid="code-viewer">CodeViewer</div> }));
vi.mock("./preview/PdfViewer", () => ({ default: () => <div data-testid="pdf-viewer">PdfViewer</div> }));

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

  it("calls clearPreview when close button is clicked", () => {
    const clearPreviewMock = vi.fn();
    useSearchStore.setState({
      selectedMatch: { path: "test.txt" } as any,
      previewData: { Text: {} } as any,
      clearPreview: clearPreviewMock,
    });

    render(<PreviewPane />);
    const closeButton = screen.getByTitle("Close preview");
    fireEvent.click(closeButton);

    expect(clearPreviewMock).toHaveBeenCalled();
  });
});
