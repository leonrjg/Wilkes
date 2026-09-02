import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import PreviewPane from "./PreviewPane";
import { activeViewerTab, useViewerStore } from "../stores/useViewerStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import { useSearchStore } from "../stores/useSearchStore";
import { api } from "../services";
import { useReaderHost, type ReaderHostServices } from "@leonrjg/wilkes-reader";
import { saveTextViewMode } from "./textViewMode";

/** Invoke a reader's `selectionActions` slot and read back the chrome it
 *  produced, so tests can drive Wilkes' handlers without a real reader. */
const selectionChrome = (props: any) => {
  const api = { dismiss: vi.fn(), clear: vi.fn(), setPinned: vi.fn() };
  return props.slots.selectionActions(
    { quote: "", origin: { TextFile: { line: 1, col: 0 } }, rects: [] },
    api,
  ).props;
};

/** The decoration a reader would have rendered for one bookmark. */
const decorationFor = (props: any, id: string) =>
  props.decorations.find((decoration: any) => decoration.id === id);

const mockCodeViewer = vi.fn(() => <div data-testid="code-viewer">CodeViewer</div>);
const mockMarkdownViewer = vi.fn(() => <div data-testid="markdown-viewer">MarkdownViewer</div>);
// Reads the host the pane provides from inside the tree, which is the only
// place a reader ever sees it.
let capturedReaderHost: ReaderHostServices | null = null;
const readerHostValue = () => capturedReaderHost!;
const mockHtmlViewer = vi.fn(() => {
  capturedReaderHost = useReaderHost();
  return <div data-testid="html-viewer">HtmlViewer</div>;
});
const mockPdfViewer = vi.fn(() => <div data-testid="pdf-viewer">PdfViewer</div>);
// The readers are stood in for so this pane is tested on what it hands them.
// Everything else the package exports stays real: the pane is built out of it.
vi.mock("@leonrjg/wilkes-reader", async (importOriginal) => ({
  ...(await importOriginal<object>()),
  CodeViewer: (props: any) => mockCodeViewer(props),
  MarkdownViewer: (props: any) => mockMarkdownViewer(props),
  HtmlViewer: (props: any) => mockHtmlViewer(props),
  PdfViewer: (props: any) => mockPdfViewer(props),
}));
vi.mock("./DocumentEditor", () => ({ default: () => <div data-testid="document-editor">DocumentEditor</div> }));
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
    resolveAssetUrl: vi.fn((path: string) => path),
    relatedDocuments: vi.fn(() => Promise.resolve([])),
    chunkTopics: vi.fn(() => Promise.resolve({ topics: [] })),
    cancelChunkTopics: vi.fn(() => Promise.resolve()),
    cancelSearch: vi.fn(() => Promise.resolve()),
    updateSettings: vi.fn(() => Promise.resolve()),
    citationLinks: vi.fn(() =>
      Promise.resolve({ references: [], cited_by: [], all_references: [] }),
    ),
    onFileMetadataUpdated: vi.fn(() => Promise.resolve(vi.fn())),
    explainRelatedDocument: vi.fn(() => Promise.resolve()),
    summarizeDocument: vi.fn(() => Promise.resolve()),
    onGenerationStream: vi.fn(() => Promise.resolve(vi.fn())),
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
    setActiveDocument: vi.fn(() => Promise.resolve()),
  },
}));

function setViewerState(state: {
  selectedMatch?: any;
  previewData?: any;
  previewLoading?: boolean;
  previewError?: string | null;
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
        previewError: state.previewError ?? null,
        pdfLoadAttempt: 0,
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
    useSettingsStore.setState({ directory: "/docs", fileList: [] });
    useSemanticStore.setState({
      readyForCurrentRoot: false,
      indexStatus: null,
    } as any);
    useGenerationStore.setState({ ready: false });
    useTopicsStore.setState({
      document: {
        loading: false,
        requestId: null,
        result: null,
        root: null,
        path: null,
        granularity: "much_fewer",
        selectedTopicKey: null,
      },
    });
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      currentSearchId: null,
      lastQuery: null,
      resultContext: null,
    });
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

  it("shows a recoverable error when a restored document cannot be loaded", () => {
    setViewerState({
      selectedMatch: {
        path: "/missing.txt",
        origin: { TextFile: { line: 0, col: 0 } },
      },
      previewError: "file no longer exists",
    });

    render(<PreviewPane />);

    expect(screen.getByText("Could not load this document")).toBeInTheDocument();
    expect(screen.getByText("file no longer exists")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    expect(api.preview).toHaveBeenCalledWith({
      path: "/missing.txt",
      origin: { TextFile: { line: 0, col: 0 } },
    });
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
        decorations: [
          expect.objectContaining({
            id: "rendered-bookmark",
            anchor: { kind: "range", range: { start: 2, end: 7 } },
            className: "rendered-bookmark-highlight",
            onActivate: expect.any(Function),
          }),
        ],
        slots: expect.objectContaining({ selectionActions: expect.any(Function) }),
      }),
    );

    fireEvent.click(toggle);

    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "View rendered Markdown" }).querySelector("svg"))
      .toBeInTheDocument();
  });

  it("defaults HTML files to rendered and toggles to source with an icon button", () => {
    setViewerState({
      selectedMatch: { path: "page.html", origin: { TextFile: { line: 1, col: 0 } } },
      previewData: {
        Text: {
          content: "<html><body><h1>Findings</h1></body></html>",
          language: "html",
          highlight_line: 1,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });
    useBookmarksStore.setState({
      bookmarks: [{
        id: "html-bookmark",
        path: "page.html",
        origin: { TextFile: { line: 1, col: 16 } },
        text_range: { start: 16, end: 24 },
        quote: "Findings",
        created_at: "2026-01-01T00:00:00Z",
        rects: [],
      }],
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("html-viewer")).toBeInTheDocument();
    expect(mockHtmlViewer.mock.calls.at(-1)?.[0]).toEqual(
      expect.objectContaining({
        content: expect.stringContaining("<h1>Findings</h1>"),
        documentPath: "page.html",
        highlightRange: { start: 0, end: 0 },
        // The same anchor and the same palette as rendered Markdown: what a
        // bookmark is does not change with the document it is in.
        decorations: [
          expect.objectContaining({
            id: "html-bookmark",
            anchor: { kind: "range", range: { start: 16, end: 24 } },
            className: "rendered-bookmark-highlight",
            onActivate: expect.any(Function),
          }),
        ],
        slots: expect.objectContaining({ selectionActions: expect.any(Function) }),
      }),
    );

    fireEvent.click(screen.getByRole("button", { name: "View HTML source" }));

    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(decorationFor(mockCodeViewer.mock.calls.at(-1)?.[0], "html-bookmark").className)
      .toBe("cm-bookmark-highlight");
    expect(screen.getByRole("button", { name: "View rendered HTML" })).toBeInTheDocument();
  });

  it("remembers the view a document was last read in, whichever kind it is", () => {
    saveTextViewMode("remembered.html", "source");
    setViewerState({
      selectedMatch: { path: "remembered.html", origin: { TextFile: { line: 0, col: 0 } } },
      previewData: {
        Text: {
          content: "<p>Body</p>",
          language: "html",
          highlight_line: 0,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });

    render(<PreviewPane />);

    expect(screen.getByTestId("code-viewer")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "View rendered HTML" })).toBeInTheDocument();
  });

  it("offers the readers a URL for the files a document sits beside", () => {
    setViewerState({
      selectedMatch: { path: "/corpus/page.html", origin: { TextFile: { line: 0, col: 0 } } },
      previewData: {
        Text: {
          content: "<p>Body</p>",
          language: "html",
          highlight_line: 0,
          highlight_range: { start: 0, end: 0 },
        },
      },
    });

    render(<PreviewPane />);

    expect(readerHostValue().resolveLocalAsset?.("/corpus/figures/one.png"))
      .toBe("/corpus/figures/one.png");
    expect(api.resolveAssetUrl).toHaveBeenCalledWith("/corpus/figures/one.png");
  });

  it("restores the Markdown view selected for a previously opened document", () => {
    saveTextViewMode("markdown-restored.md", "source");
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
    saveTextViewMode(path, "rendered");
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
    expect(props.decorations).toEqual([
      expect.objectContaining({
        id: "text-one",
        anchor: { kind: "range", range: { start: 6, end: 11 } },
        className: "cm-bookmark-highlight",
      }),
    ]);

    selectionChrome(props).onAddBookmark({
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
      decorationFor(mockCodeViewer.mock.calls.at(-1)![0], "noted-bookmark").onActivate("noted-bookmark", {
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
      decorationFor(mockCodeViewer.mock.calls.at(-1)![0], "outside-dismiss").onActivate("outside-dismiss", {
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

  it("shows the citation graph button only for a document with a DOI", async () => {
    const match = { path: "/docs/paper.pdf", origin: { PdfPage: { page: 1, bbox: null } } } as any;
    setViewerState({
      selectedMatch: match,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: null, doi: null, created_at: null },
      viewerMetadataStatus: "ready",
    });
    const { rerender } = render(<PreviewPane />);

    expect(screen.queryByRole("button", { name: "Show citation graph" })).not.toBeInTheDocument();

    setViewerState({
      selectedMatch: match,
      previewData: { Pdf: { page: 1, highlight_bbox: null } } as any,
      viewerMetadata: { title: "Paper", author: null, doi: "10.1/paper", created_at: null },
      viewerMetadataStatus: "ready",
    });
    rerender(<PreviewPane />);

    fireEvent.click(screen.getByRole("button", { name: "Show citation graph" }));
    expect(await screen.findByRole("complementary", { name: "Citation graph" })).toBeInTheDocument();
    expect(api.citationLinks).toHaveBeenCalledWith({ root: "/docs", path: "/docs/paper.pdf" });
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

  it("publishes the active PDF and live page for external MCP without a chat session", async () => {
    setViewerState({
      selectedMatch: {
        path: "/docs/active.pdf",
        origin: { PdfPage: { page: 2, bbox: null } },
      } as any,
      previewData: { Pdf: { page: 2, highlight_bbox: null } },
    });

    render(<PreviewPane />);

    await waitFor(() =>
      expect(api.setActiveDocument).toHaveBeenCalledWith("/docs/active.pdf", 2),
    );
    act(() => {
      mockPdfViewer.mock.lastCall?.[0].onPageChange(6);
    });
    expect(api.setActiveDocument).toHaveBeenCalledWith("/docs/active.pdf", 6);
  });

  it("keeps workspace-owned actions and active-document publication out of standalone mode", () => {
    useGenerationStore.setState({ ready: true });
    useSemanticStore.setState({ readyForCurrentRoot: true });
    setViewerState({
      selectedMatch: {
        path: "/outside/every-root/paper.pdf",
        origin: { PdfPage: { page: 1, bbox: null } },
      } as any,
      previewData: { Pdf: { page: 1, highlight_bbox: null } },
      viewerMetadata: {
        title: "Outside paper",
        author: null,
        doi: "10.1000/outside",
        created_at: null,
      },
      viewerMetadataStatus: "ready",
    });

    render(<PreviewPane standalone />);

    expect(screen.queryByRole("button", { name: "Summarize document" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show related documents" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show citation graph" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show document topics" })).not.toBeInTheDocument();
    expect(api.setActiveDocument).not.toHaveBeenCalled();
  });

  it("surfaces PDF parse failures and retries with a fresh load attempt", () => {
    const mockMatch = {
      path: "/missing.pdf",
      origin: { PdfPage: { page: 1, bbox: null } },
    } as any;
    setViewerState({
      selectedMatch: mockMatch,
      previewData: { Pdf: { page: 1, highlight_bbox: null } },
    });

    render(<PreviewPane />);
    act(() => {
      mockPdfViewer.mock.lastCall?.[0].onLoadError(new Error("PDF file not found"));
    });

    expect(screen.getByText("PDF file not found")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    expect(activeViewerTab(useViewerStore.getState())?.pdfLoadAttempt).toBe(1);
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

  it("passes transient PDF search evidence to the viewer locator", () => {
    const locator = {
      matched_text: "reason-\nable by reasonable people",
      context_before: "found to be ",
      context_after: ". An effort",
    };
    setViewerState({
      selectedMatch: {
        path: "paper.pdf",
        origin: { PdfPage: { page: 30, bbox: null } },
        text_range: { start: 10, end: 45 },
      },
      previewData: { Pdf: { page: 30, highlight_bbox: null } },
    });
    useSearchStore.setState({
      results: [
        {
          path: "paper.pdf",
          file_type: "Pdf",
          matches: [
            {
              text_range: { start: 10, end: 45 },
              matched_text: locator.matched_text,
              context_before: locator.context_before,
              context_after: locator.context_after,
              origin: { PdfPage: { page: 30, bbox: null } },
            },
          ],
        },
      ],
    });

    render(<PreviewPane />);

    expect(mockPdfViewer.mock.lastCall?.[0].search_locator).toEqual(locator);
  });

  it("hands the reader the areas this document's reading owns", () => {
    const superseded = [
      {
        page: 3,
        bbox: { x: 10, y: 20, width: 300, height: 24 },
        text: "y_{B} = w^{x_{B}} \\bmod q",
      },
    ];
    setViewerState({
      selectedMatch: {
        path: "paper.pdf",
        origin: { PdfPage: { page: 3, bbox: null } },
      } as any,
      previewData: { Pdf: { page: 3, highlight_bbox: null, superseded } },
    });

    render(<PreviewPane />);

    expect(mockPdfViewer.mock.lastCall?.[0].textSubstitutions).toEqual(superseded);
  });

  it("tells the reader nothing about areas before the preview arrives", () => {
    setViewerState({
      selectedMatch: {
        path: "paper.pdf",
        origin: { PdfPage: { page: 1, bbox: null } },
      } as any,
      previewData: null,
    });

    render(<PreviewPane />);

    // Not an empty list: the document's reading has not been read yet, which
    // is not the same as knowing it owns nothing.
    expect(mockPdfViewer.mock.lastCall?.[0].textSubstitutions).toBeUndefined();
  });

  it("does not replace semantic chunk geometry with exact-result localization", () => {
    const origin = {
      PdfPage: { page: 30, bbox: { x: 1, y: 2, width: 3, height: 4 } },
    };
    setViewerState({
      selectedMatch: { path: "paper.pdf", origin },
      previewData: { Pdf: { page: 30, highlight_bbox: origin.PdfPage.bbox } },
    });
    useSearchStore.setState({
      results: [
        {
          path: "paper.pdf",
          file_type: "Pdf",
          matches: [
            {
              text_range: null,
              matched_text: "an entire semantic chunk",
              context_before: "",
              context_after: "",
              origin,
              score: 0.9,
            },
          ],
        },
      ],
    });

    render(<PreviewPane />);

    expect(mockPdfViewer.mock.lastCall?.[0].search_locator).toBeNull();
    expect(mockPdfViewer.mock.lastCall?.[0].highlight_bbox).toEqual(origin.PdfPage.bbox);
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
    expect(call.decorations).toEqual([
      expect.objectContaining({
        id: "current",
        anchor: { kind: "rects", page: 4, rects: [{ x: 1, y: 2, width: 3, height: 4 }] },
        className: "pdf-highlight--bookmark",
      }),
    ]);
    // The text-file bookmark carries no rects and must not produce a highlight.
  });

  it("marks a navigation target the reader cannot locate itself", () => {
    setViewerState({
      selectedMatch: {
        path: "current.pdf",
        origin: { PdfPage: { page: 3, bbox: { x: 1, y: 2, width: 3, height: 4 } } },
      } as any,
      previewData: { Pdf: { page: 3, highlight_bbox: null } },
      previewLoading: false,
    });
    useBookmarksStore.setState({ bookmarks: [] });

    render(<PreviewPane />);

    const call = mockPdfViewer.mock.calls.at(-1)![0];
    expect(call.decorations).toEqual([
      expect.objectContaining({
        id: "navigation-target",
        anchor: { kind: "rects", page: 3, rects: [{ x: 1, y: 2, width: 3, height: 4 }] },
      }),
    ]);
  });

  it("leaves a bookmarked target to its bookmark, so it is marked once", () => {
    const bbox = { x: 1, y: 2, width: 3, height: 4 };
    setViewerState({
      selectedMatch: {
        path: "current.pdf",
        origin: { PdfPage: { page: 3, bbox } },
      } as any,
      previewData: { Pdf: { page: 3, highlight_bbox: null } },
      previewLoading: false,
    });
    useBookmarksStore.setState({
      bookmarks: [
        {
          id: "bookmarked-target",
          path: "current.pdf",
          origin: { PdfPage: { page: 3, bbox } },
          quote: "quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
          rects: [bbox],
        },
      ],
    });

    render(<PreviewPane />);

    // A second mark under the bookmark is what showed as two stacked colours.
    const call = mockPdfViewer.mock.calls.at(-1)![0];
    expect(call.decorations.map((d: any) => d.id)).toEqual(["bookmarked-target"]);
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
    expect(api.citationLinks).not.toHaveBeenCalled();

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

  it("opens a within-document topic cloud and surfaces its chunks as search results", async () => {
    vi.mocked(api.chunkTopics).mockResolvedValueOnce({
      topics: [
        {
          cluster_key: "document-topic-a",
          chunks: [
            {
              chunk_id: 1,
              file_path: "/docs/source.txt",
              chunk_text: "First indexed passage",
              extraction_byte_range: { start: 0, end: 21 },
              origin: { TextFile: { line: 1, col: 1 } },
            },
            {
              chunk_id: 2,
              file_path: "/docs/source.txt",
              chunk_text: "Second indexed passage",
              extraction_byte_range: { start: 22, end: 44 },
              origin: { TextFile: { line: 2, col: 1 } },
            },
          ],
          representative_chunk_id: 1,
          chunk_count: 2,
          distinct_document_count: 1,
          cohesion: 0.9,
          library_coverage: {
            related_document_count: 2,
            eligible_document_count: 246,
            chunks: [
              {
                chunk_id: 21,
                file_path: "/library/outer-a.txt",
                chunk_text: "Related passage from A",
                extraction_byte_range: { start: 5, end: 27 },
                origin: { TextFile: { line: 3, col: 1 } },
              },
              {
                chunk_id: 22,
                file_path: "/library/outer-b.pdf",
                chunk_text: "Related passage from B",
                extraction_byte_range: { start: 0, end: 22 },
                origin: { PdfPage: { page: 4, bbox: null } },
              },
            ],
          },
          label: "Indexed Passage Themes",
        },
      ],
      total_chunk_count: 6,
      sampled_chunk_count: 6,
      total_document_count: 1,
      sampled_document_count: 1,
      input_cap: 6,
    });
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      indexStatus: {
        indexed_files: 1,
        total_chunks: 6,
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
      selectedMatch: {
        path: "/docs/source.txt",
        origin: { TextFile: { line: 1, col: 1 } },
      },
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
    expect(await screen.findByText(/Related to source\.txt/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show document topics" }));
    expect(await screen.findByText("Indexed Passage Themes")).toBeInTheDocument();
    expect(screen.getByText("2 / 246 docs")).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Show related passages for Indexed Passage Themes",
      }),
    ).toHaveAttribute(
      "title",
      expect.stringContaining(
        "Show 2 related passages from 2 documents",
      ),
    );
    expect(screen.queryByText(/Related to source\.txt/)).not.toBeInTheDocument();
    expect(api.chunkTopics).toHaveBeenCalledWith(
      expect.any(String),
      {
        root: "/docs",
        path: "/docs/source.txt",
        granularity: "much_fewer",
      },
    );

    fireEvent.click(screen.getByText("Indexed Passage Themes"));
    await waitFor(() =>
      expect(useSearchStore.getState().stats).toEqual(
        expect.objectContaining({ files_scanned: 1, total_matches: 2 }),
      ),
    );
    expect(
      useSearchStore.getState().results[0].matches.map((match) => match.matched_text),
    ).toEqual(["First indexed passage", "Second indexed passage"]);

    fireEvent.click(
      screen.getByRole("button", {
        name: "Show related passages for Indexed Passage Themes",
      }),
    );
    await waitFor(() =>
      expect(useSearchStore.getState().stats).toEqual(
        expect.objectContaining({ files_scanned: 2, total_matches: 2 }),
      ),
    );
    expect(useSearchStore.getState().results.map((result) => result.path)).toEqual([
      "/library/outer-a.txt",
      "/library/outer-b.pdf",
    ]);
    expect(
      useSearchStore
        .getState()
        .results.flatMap((result) =>
          result.matches.map((match) => match.matched_text),
        ),
    ).toEqual(["Related passage from A", "Related passage from B"]);

    const requestId = useTopicsStore.getState().document.requestId;
    fireEvent.click(screen.getByRole("button", { name: "Close document topics" }));
    expect(api.cancelChunkTopics).toHaveBeenCalledWith(requestId);
  });

  it("gates the summary affordance on generation readiness and keeps viewer panels exclusive", async () => {
    setViewerState({
      selectedMatch: {
        path: "/docs/source.txt",
        origin: { TextFile: { line: 1, col: 1 } },
      } as any,
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

    expect(
      screen.queryByRole("button", { name: "Summarize document" }),
    ).not.toBeInTheDocument();

    act(() => useGenerationStore.setState({ ready: true }));
    fireEvent.click(
      await screen.findByRole("button", { name: "Summarize document" }),
    );
    expect(screen.getByText("Summary")).toBeInTheDocument();
    await waitFor(() =>
      expect(api.summarizeDocument).toHaveBeenCalledWith(
        expect.stringContaining("document_summary-"),
        "/docs/source.txt",
      ),
    );

    fireEvent.click(
      screen.getByRole("button", { name: "Show related documents" }),
    );
    expect(screen.queryByText("Summary")).not.toBeInTheDocument();
    expect(screen.getByText("Semantic index unavailable")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "Hide related documents" }),
    );
    expect(screen.queryByText("Semantic index unavailable")).not.toBeInTheDocument();
  });

  it("streams related explanations through the shared request contract and caches completion", async () => {
    let generationHandler: (event: any) => void = () => {};
    (api.onGenerationStream as any).mockImplementation((handler: any) => {
      generationHandler = handler;
      return Promise.resolve(vi.fn());
    });
    (api.relatedDocuments as any).mockResolvedValue([
      {
        path: "/relation-stream-docs/related.txt",
        file_type: "PlainText",
        size_bytes: 7,
        extension: "txt",
        score: 0.88,
      },
    ]);
    useSettingsStore.setState({ directory: "/relation-stream-docs" });
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      indexStatus: {
        indexed_files: 2,
        total_chunks: 4,
        built_at: 987654,
        build_duration_ms: 10,
        engine: "Candle",
        model_id: "stream-model",
        dimension: 2,
        root_path: "/relation-stream-docs",
        db_size_bytes: 100,
      },
    } as any);
    useGenerationStore.setState({ ready: true });
    setViewerState({
      selectedMatch: {
        path: "/relation-stream-docs/source.txt",
        origin: { TextFile: { line: 1, col: 1 } },
      } as any,
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

    fireEvent.click(
      screen.getByRole("button", { name: "Show related documents" }),
    );
    await screen.findByText("related.txt");
    fireEvent.click(
      screen.getByRole("button", { name: "Explain why these are related" }),
    );
    await waitFor(() =>
      expect(api.explainRelatedDocument).toHaveBeenCalledOnce(),
    );
    const requestId = (api.explainRelatedDocument as any).mock.calls[0][0];
    expect(api.explainRelatedDocument).toHaveBeenCalledWith(
      requestId,
      "/relation-stream-docs/source.txt",
      "/relation-stream-docs/related.txt",
    );

    act(() => {
      generationHandler({
        phase: "delta",
        request_id: requestId,
        task: "relation_explanation",
        delta: "Both measure ",
      });
      generationHandler({
        phase: "completed",
        request_id: requestId,
        task: "relation_explanation",
        text: "Both measure cache behavior.",
      });
    });
    expect(screen.getByText("Both measure cache behavior.")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "Hide why these are related" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Explain why these are related" }),
    );
    expect(screen.getByText("Both measure cache behavior.")).toBeInTheDocument();
    expect(api.explainRelatedDocument).toHaveBeenCalledOnce();
  });
});
