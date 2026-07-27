import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PreviewPane from "./PreviewPane";
import { useViewerStore } from "../stores/useViewerStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { api } from "../services";
import { saveMarkdownViewMode } from "./preview/textScrollMemory";

const mockCodeViewer = vi.fn(() => <div data-testid="code-viewer">CodeViewer</div>);
vi.mock("./preview/CodeViewer", () => ({ default: (props: any) => mockCodeViewer(props) }));

const mockMarkdownViewer = vi.fn(() => <div data-testid="markdown-viewer">MarkdownViewer</div>);
vi.mock("./preview/MarkdownViewer", () => ({ default: (props: any) => mockMarkdownViewer(props) }));

const mockPdfViewer = vi.fn(() => <div data-testid="pdf-viewer">PdfViewer</div>);
vi.mock("./preview/PdfViewer", () => ({ default: (props: any) => mockPdfViewer(props) }));
vi.mock("./Toast", () => ({ useToasts: () => ({ addToast: vi.fn() }) }));
vi.mock("../services", () => ({
  isTauri: false,
  api: {
    openPath: vi.fn((url: string) => {
      window.open(url, "_blank", "noopener,noreferrer");
      return Promise.resolve();
    }),
    writeClipboard: vi.fn((text: string) => {
      navigator.clipboard.writeText(text);
      return Promise.resolve();
    }),
    resolvePdfUrl: vi.fn((path: string) => path),
    relatedDocuments: vi.fn(() => Promise.resolve([])),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
    preview: vi.fn(() => Promise.resolve({
      Text: {
        content: "",
        language: "text",
        highlight_line: 0,
        highlight_range: { start: 0, end: 0 },
      },
    })),
    getFileMetadata: vi.fn(() => Promise.resolve(null)),
    resolveFileMetadata: vi.fn(() => Promise.resolve(null)),
  },
}));

function setViewerState(state: {
  selectedMatch?: any;
  previewData?: any;
  previewLoading?: boolean;
  viewerMetadata?: any;
  viewerMetadataStatus?: any;
}) {
  if (!state.selectedMatch) {
    useViewerStore.setState({ tabs: [], activeTabId: null });
    return;
  }
  const match = state.selectedMatch;
  useViewerStore.setState({
    activeTabId: "test-tab",
    tabs: [
      {
        id: "test-tab",
        path: match.path,
        match,
        history: [match],
        historyIndex: 0,
        previewData: state.previewData ?? null,
        previewLoading: state.previewLoading ?? false,
        metadata: state.viewerMetadata ?? null,
        metadataStatus: state.viewerMetadataStatus ?? "idle",
        requestId: 1,
      },
    ],
  });
}

describe("PreviewPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn() },
      configurable: true,
    });
    vi.stubGlobal("open", vi.fn());
    setViewerState({
      selectedMatch: null,
      previewData: null,
      previewLoading: false,
      viewerMetadata: null,
      viewerMetadataStatus: "idle",
    });
    useBookmarksStore.setState({
      bookmarks: [],
    });
    (api.relatedDocuments as any).mockResolvedValue([]);
    useSettingsStore.setState({ directory: "/docs" });
    useSemanticStore.setState({
      readyForCurrentRoot: false,
      indexStatus: null,
    } as any);
  });

  it("renders empty state when no match is selected", () => {
    render(<PreviewPane />);
    const logo = screen.getByAltText("Wilkes");
    expect(logo).toBeInTheDocument();
    expect(logo).toHaveClass("w-[clamp(10rem,20vw,18rem)]", "max-w-[80vw]", "h-auto");
    expect(logo).toHaveClass("opacity-25");
    expect(logo).not.toHaveClass("transition-all", "hover:opacity-25");
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

    setViewerState({
      selectedMatch: mockMatch,
      previewData: mockPreviewData,
    });

    render(<PreviewPane />);
    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "test.txt" })).toBeInTheDocument();
    expect(screen.getAllByText("test.txt")).toHaveLength(1);
  });

  it("defaults Markdown files to rendered and toggles to source with an icon button", () => {
    setViewerState({
      selectedMatch: { path: "markdown-toggle.md", origin: { TextFile: { line: 1, col: 0 } } },
      previewData: {
        Text: {
          content: "# Notes\n\n| A | B |\n| - | - |\n| 1 | 2 |",
          language: "markdown",
          highlight_line: 1,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });
    useBookmarksStore.setState({
      bookmarks: [{
        id: "rendered-bookmark",
        path: "markdown-toggle.md",
        origin: { TextFile: { line: 1, col: 2 } },
        text_range: { start: 2, end: 7 },
        quote: "Notes",
        created_at: "2026-01-01T00:00:00Z",
        rects: [],
      }],
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("markdown-viewer")).toBeInTheDocument();
    const toggle = screen.getByRole("button", { name: "View Markdown source" });
    expect(toggle.querySelector("svg")).toBeInTheDocument();
    expect(mockMarkdownViewer.mock.calls.at(-1)?.[0]).toEqual(
      expect.objectContaining({
        content: expect.stringContaining("# Notes"),
        highlightRange: { start: 0, end: 0 },
        bookmarkHighlights: [{ id: "rendered-bookmark", range: { start: 2, end: 7 } }],
        onAddBookmark: expect.any(Function),
        onExplainSelection: expect.any(Function),
        onAskSelection: expect.any(Function),
      }),
    );

    fireEvent.click(toggle);

    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "View rendered Markdown" }).querySelector("svg"))
      .toBeInTheDocument();
  });

  it("restores the Markdown view selected for a previously opened document", () => {
    saveMarkdownViewMode("markdown-restored.md", "source");
    setViewerState({
      selectedMatch: { path: "markdown-restored.md", origin: { TextFile: { line: 0, col: 0 } } },
      previewData: {
        Text: {
          content: "# Notes",
          language: "markdown",
          highlight_line: 0,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "View rendered Markdown" })).toBeInTheDocument();
  });

  it("uses the bookmark's UTF-8 range for the rendered navigation target", () => {
    const path = "markdown-unicode-bookmark.md";
    saveMarkdownViewMode(path, "rendered");
    setViewerState({
      selectedMatch: {
        path,
        origin: { TextFile: { line: 1, col: 14 } },
        text_range: { start: 14, end: 20 },
      },
      previewData: {
        Text: {
          content: "é🙂 before target",
          language: "markdown",
          highlight_line: 1,
          highlight_range: { start: 14, end: 20 },
        },
      },
    });

    render(<PreviewPane />);

    expect(mockMarkdownViewer.mock.calls.at(-1)?.[0]).toEqual(
      expect.objectContaining({ highlightRange: { start: 14, end: 20 } }),
    );
  });

  it("passes text bookmark ranges to CodeViewer and persists its normalized selection", async () => {
    const add = vi.fn().mockResolvedValue(undefined);
    setViewerState({
      selectedMatch: { path: "test.txt", origin: { TextFile: { line: 1, col: 0 } } },
      previewData: {
        Text: {
          content: "hello world",
          language: "text",
          highlight_line: 1,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });
    useBookmarksStore.setState({
      add,
      bookmarks: [
        {
          id: "text-one",
          path: "test.txt",
          origin: { TextFile: { line: 1, col: 6 } },
          text_range: { start: 6, end: 11 },
          quote: "world",
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
      ],
    });

    render(<PreviewPane />);
    const props = mockCodeViewer.mock.calls.at(-1)![0];
    expect(props.bookmarkHighlights).toEqual([
      { id: "text-one", range: { start: 6, end: 11 } },
    ]);

    props.onAddBookmark({
      quote: "hello",
      origin: { TextFile: { line: 1, col: 0 } },
      text_range: { start: 0, end: 5 },
      rects: [],
    });

    await waitFor(() =>
      expect(add).toHaveBeenCalledWith({
        path: "test.txt",
        quote: "hello",
        origin: { TextFile: { line: 1, col: 0 } },
        text_range: { start: 0, end: 5 },
        rects: [],
      }),
    );
  });

  it("shows a bookmark note from the viewer highlight and deletes the bookmark", async () => {
    const remove = vi.fn().mockResolvedValue(undefined);
    setViewerState({
      selectedMatch: { path: "test.txt", origin: { TextFile: { line: 1, col: 0 } } },
      previewData: {
        Text: {
          content: "hello world",
          language: "text",
          highlight_line: 1,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });
    useBookmarksStore.setState({
      remove,
      bookmarks: [{
        id: "noted-bookmark",
        path: "test.txt",
        origin: { TextFile: { line: 1, col: 6 } },
        text_range: { start: 6, end: 11 },
        quote: "world",
        created_at: "2026-01-01T00:00:00Z",
        note: "Important context",
        rects: [],
      }],
    });

    render(<PreviewPane />);
    act(() => {
      mockCodeViewer.mock.calls.at(-1)![0].onBookmarkOpen("noted-bookmark", {
        left: 100,
        top: 100,
        right: 140,
        bottom: 120,
      });
    });

    const details = await screen.findByRole("complementary", { name: "Bookmark details" });
    expect(details).toBeInTheDocument();
    expect(details).toHaveStyle({ left: "100px", top: "128px" });
    expect(screen.getByText("Important context")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Delete bookmark" }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("noted-bookmark"));
    await waitFor(() =>
      expect(screen.queryByRole("complementary", { name: "Bookmark details" }))
        .not.toBeInTheDocument(),
    );
  });

  it("dismisses bookmark details when clicking outside the card", async () => {
    setViewerState({
      selectedMatch: { path: "test.txt", origin: { TextFile: { line: 1, col: 0 } } },
      previewData: {
        Text: {
          content: "hello world",
          language: "text",
          highlight_line: 1,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });
    useBookmarksStore.setState({
      bookmarks: [{
        id: "outside-dismiss",
        path: "test.txt",
        origin: { TextFile: { line: 1, col: 6 } },
        text_range: { start: 6, end: 11 },
        quote: "world",
        created_at: "2026-01-01T00:00:00Z",
        note: "Visible note",
        rects: [],
      }],
    });

    render(<PreviewPane />);
    act(() => {
      mockCodeViewer.mock.calls.at(-1)![0].onBookmarkOpen("outside-dismiss", {
        left: 100,
        top: 100,
        right: 140,
        bottom: 120,
      });
    });
    expect(await screen.findByRole("complementary", { name: "Bookmark details" }))
      .toBeInTheDocument();

    fireEvent.pointerDown(screen.getByTestId("code-viewer"));
    expect(screen.queryByRole("complementary", { name: "Bookmark details" }))
      .not.toBeInTheDocument();
  });

  it("renders metadata title and author when available", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "A Better Title", author: "Test Author", doi: null, created_at: null },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);
    expect(screen.getByText("A Better Title")).toBeInTheDocument();
    expect(screen.getByText("Test Author")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "test.pdf" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy path" })).toBeInTheDocument();
  });

  it("shortens the header author to 30 characters", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: {
        title: "Paper",
        author: "A Very Long Author Name That Exceeds Thirty Characters",
        doi: null,
        created_at: null,
      },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);
    expect(screen.getByText("A Very Long Author Name That …")).toBeInTheDocument();
    expect(screen.queryByText("A Very Long Author Name That Exceeds Thirty Characters")).not.toBeInTheDocument();
  });

  it("formats a Zotero MM/YYYY publication date in the header", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: "Tambon et al.", doi: null, created_at: "05/2025" },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);
    expect(screen.getByText("May 2025")).toBeInTheDocument();
    expect(screen.getByText("Tambon et al.")).toBeInTheDocument();
  });

  it("renders metadata loading placeholder while preserving the path", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: null,
      viewerMetadataStatus: "loading",
    });

    render(<PreviewPane />);
    expect(screen.getByText("Loading metadata…")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "test.pdf" })).toBeInTheDocument();
    expect(screen.getAllByText("test.pdf")).toHaveLength(1);
  });

  it("replaces the displayed path with a copy path action", () => {
    setViewerState({
      selectedMatch: { path: "/docs/paper.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: null, doi: null, created_at: null },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);

    expect(screen.queryByText("/docs/paper.pdf")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy path" }));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("/docs/paper.pdf");
  });

  it("renders DOI open and copy actions when DOI is available", () => {
    const mockMatch = { path: "paper.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: "Author", doi: "10.1000/xyz123", created_at: null },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);
    expect(screen.getByRole("button", { name: "Open DOI 10.1000/xyz123" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open Google Scholar" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy DOI 10.1000/xyz123" })).toBeInTheDocument();
    expect(screen.getByText("DOI: 10.1000/xyz123")).toBeInTheDocument();
    expect(screen.getByText("Scholar")).toBeInTheDocument();
  });

  it("opens DOI and Google Scholar URLs and copies DOI from header actions", () => {
    const mockMatch = { path: "paper.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: "Author", doi: "10.1000/xyz123", created_at: null },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);

    fireEvent.click(screen.getByRole("button", { name: "Open DOI 10.1000/xyz123" }));
    expect(window.open).toHaveBeenCalledWith(
      "https://doi.org/10.1000/xyz123",
      "_blank",
      "noopener,noreferrer",
    );

    fireEvent.click(screen.getByRole("button", { name: "Open Google Scholar" }));
    expect(window.open).toHaveBeenCalledWith(
      "https://scholar.google.com/scholar?q=10.1000%2Fxyz123",
      "_blank",
      "noopener,noreferrer",
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy DOI 10.1000/xyz123" }));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("10.1000/xyz123");
  });

  it("renders Google Scholar action using title when DOI is unavailable", () => {
    const mockMatch = { path: "paper.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "A Title Without DOI", author: "Author", doi: null, created_at: null },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);

    expect(screen.queryByRole("button", { name: /^Open DOI / })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open Google Scholar" }));
    expect(window.open).toHaveBeenCalledWith(
      "https://scholar.google.com/scholar?q=A%20Title%20Without%20DOI",
      "_blank",
      "noopener,noreferrer",
    );
  });

  it("renders PdfViewer for pdf data", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    const mockPreviewData = {
      Pdf: {
        page: 1,
        highlight_bbox: null,
      },
    };

    setViewerState({
      selectedMatch: mockMatch,
      previewData: mockPreviewData,
    });

    render(<PreviewPane />);
    expect(screen.getByTestId("pdf-viewer")).toBeInTheDocument();
  });

  it("renders created-at month and year in the metadata summary", () => {
    const mockMatch = { path: "test.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: "Author", doi: null, created_at: "2025-04" },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane />);
    expect(screen.getByText("Apr 2025")).toBeInTheDocument();
    expect(screen.getByText("Author")).toBeInTheDocument();
  });

  it("renders PdfViewer using selectedMatch.origin even when previewData is stale", () => {
    // Regression: page/bbox were read from displayData (which could be stale
    // data from a previously viewed file) instead of selectedMatch.origin.
    // This meant PdfViewer could mount with the wrong target page.
    setViewerState({
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

  it("passes only current-file PDF bookmarks to PdfViewer", () => {
    setViewerState({
      selectedMatch: {
        path: "current.pdf",
        origin: { PdfPage: { page: 1, bbox: null } },
      } as any,
      previewData: { Pdf: { page: 1, highlight_bbox: null } },
      previewLoading: false,
    });
    useBookmarksStore.setState({
      bookmarks: [
        {
          id: "current",
          path: "current.pdf",
          origin: { PdfPage: { page: 4, bbox: { x: 1, y: 2, width: 3, height: 4 } } },
          quote: "current quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
          rects: [{ x: 1, y: 2, width: 3, height: 4 }],
        },
        {
          id: "other",
          path: "other.pdf",
          origin: { PdfPage: { page: 5, bbox: { x: 10, y: 20, width: 30, height: 40 } } },
          quote: "other quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
          rects: [{ x: 10, y: 20, width: 30, height: 40 }],
        },
        {
          id: "text",
          path: "current.pdf",
          origin: { TextFile: { line: 1, col: 1 } },
          quote: "text quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
          rects: [],
        },
      ],
    });

    render(<PreviewPane />);

    const call = mockPdfViewer.mock.calls[mockPdfViewer.mock.calls.length - 1][0];
    expect(call.bookmarkHighlights).toEqual([
      { id: "current", page: 4, rects: [{ x: 1, y: 2, width: 3, height: 4 }] },
    ]);
    // The text-file bookmark carries no rects and must not produce a highlight.
  });

  it("renders PdfViewer when selectedMatch is PDF but previewData is stale Text data", () => {
    // Regression: viewer type was determined by displayData ("Text" in displayData),
    // not by selectedMatch.origin. When coming from a text file, the stale
    // displayData would show CodeViewer instead of PdfViewer.
    setViewerState({
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

  it("closes the document from its tab", () => {
    setViewerState({
      selectedMatch: { path: "test.txt", origin: { TextFile: { line: 1, col: 1 } } } as any,
      previewData: { Text: { content: "", language: null, highlight_line: 1, highlight_range: { start: 0, end: 0 } } } as any,
    });

    render(<PreviewPane />);
    const closeButton = screen.getByRole("button", { name: "Close test.txt" });
    fireEvent.click(closeButton);

    expect(useViewerStore.getState().tabs).toEqual([]);
    expect(screen.getByAltText("Wilkes")).toBeInTheDocument();
  });

  it("renders related documents and opens them in the viewer", async () => {
    (api.relatedDocuments as any).mockResolvedValueOnce([
      {
        path: "/docs/lower-score.txt",
        file_type: "PlainText",
        size_bytes: 5,
        extension: "txt",
        score: 0.42,
      },
      {
        path: "/docs/related.txt",
        file_type: "PlainText",
        size_bytes: 7,
        extension: "txt",
        score: 0.88,
      },
    ]);
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      indexStatus: {
        indexed_files: 2,
        total_chunks: 4,
        built_at: 123,
        build_duration_ms: 10,
        engine: "Candle",
        model_id: "model",
        dimension: 2,
        root_path: "/docs",
        db_size_bytes: 100,
      },
    } as any);
    setViewerState({
      selectedMatch: { path: "/docs/source.txt", origin: { TextFile: { line: 1, col: 1 } } } as any,
      previewData: {
        Text: {
          content: "source",
          language: "text",
          highlight_line: 1,
          highlight_range: { start: 0, end: 6 },
        },
      },
    });

    render(<PreviewPane />);

    fireEvent.click(screen.getByRole("button", { name: "Show related documents" }));
    await waitFor(() => expect(screen.getByText("related.txt")).toBeInTheDocument());
    expect(screen.getByText("88%")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Filter files...")).toBeInTheDocument();
    const relatedRows = screen.getAllByRole("button").filter((button) =>
      /(?:related|lower-score)\.txt/.test(button.textContent ?? ""),
    );
    expect(relatedRows.map((button) => button.textContent)).toEqual([
      expect.stringContaining("related.txt"),
      expect.stringContaining("lower-score.txt"),
    ]);
    expect(api.relatedDocuments).toHaveBeenCalledWith({
      root: "/docs",
      path: "/docs/source.txt",
      scope: { type: "corpus" },
      limit: 8,
    });

    fireEvent.click(screen.getByText("related.txt"));
    expect(useViewerStore.getState().tabs).toHaveLength(2);
    expect(useViewerStore.getState().tabs.find(
      (tab) => tab.id === useViewerStore.getState().activeTabId,
    )?.path).toBe("/docs/related.txt");

    setViewerState({
      selectedMatch: {
        path: "/docs/related.txt",
        origin: { TextFile: { line: 1, col: 1 } },
      },
    });
    expect(await screen.findByText("Related to source.txt")).toBeInTheDocument();
    expect(api.relatedDocuments).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Use current" }));
    await waitFor(() => expect(api.relatedDocuments).toHaveBeenLastCalledWith({
      root: "/docs",
      path: "/docs/related.txt",
      scope: { type: "corpus" },
      limit: 8,
    }));

    setViewerState({
      selectedMatch: {
        path: "/docs/normal-navigation.txt",
        origin: { TextFile: { line: 1, col: 1 } },
      },
    });
    await waitFor(() => expect(api.relatedDocuments).toHaveBeenLastCalledWith({
      root: "/docs",
      path: "/docs/normal-navigation.txt",
      scope: { type: "corpus" },
      limit: 8,
    }));
    expect(screen.getByText("Related to normal-navigation.txt")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use current" })).not.toBeInTheDocument();
  });

  it("toggles the related documents pane", async () => {
    (api.relatedDocuments as any).mockResolvedValueOnce([
      {
        path: "/docs/related.txt",
        file_type: "PlainText",
        size_bytes: 7,
        extension: "txt",
        score: 0.88,
      },
    ]);
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      indexStatus: {
        indexed_files: 2,
        total_chunks: 4,
        built_at: 123,
        build_duration_ms: 10,
        engine: "Candle",
        model_id: "model",
        dimension: 2,
        root_path: "/docs",
        db_size_bytes: 100,
      },
    } as any);
    setViewerState({
      selectedMatch: { path: "/docs/source.txt", origin: { TextFile: { line: 1, col: 1 } } } as any,
      previewData: {
        Text: {
          content: "source",
          language: "text",
          highlight_line: 1,
          highlight_range: { start: 0, end: 6 },
        },
      },
    });

    render(<PreviewPane />);

    expect(screen.queryByText("related.txt")).not.toBeInTheDocument();
    expect(api.relatedDocuments).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Show related documents" }));
    await waitFor(() => expect(screen.getByText("related.txt")).toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: "Use whole library for related documents" }));
    await waitFor(() => expect(api.relatedDocuments).toHaveBeenLastCalledWith({
      root: "/docs",
      path: "/docs/source.txt",
      scope: { type: "all" },
      limit: 8,
    }));
    fireEvent.click(screen.getByRole("button", { name: "Use current root for related documents" }));
    expect(screen.getByText("related.txt")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Hide related documents" }));
    expect(screen.queryByText("related.txt")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show related documents" }));
    expect(screen.getByText("related.txt")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Close related documents" }));
    expect(screen.queryByText("related.txt")).not.toBeInTheDocument();
  });
});
