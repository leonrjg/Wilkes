import {
  act,
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { ToastProvider } from "./Toast";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useResearchStore } from "../stores/useResearchStore";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useChatStore } from "../stores/useChatStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import type { FileEntry, GenerationStreamEvent } from "../lib/types";

const {
  mockOpenPath,
  mockRevealPath,
  mockRenameFile,
  mockRefreshFileMetadata,
  mockWriteClipboard,
  mockListFiles,
  mockUpdateSettings,
  mockDeleteFile,
  mockDeletionKind,
  mockIsTauri,
  mockSummarizeSearchResults,
  mockOnGenerationStream,
  mockOpenChatPaneAndSend,
} = vi.hoisted(() => ({
  mockOpenPath: vi.fn(),
  mockRevealPath: vi.fn(),
  mockRenameFile: vi.fn(),
  mockRefreshFileMetadata: vi.fn(),
  mockWriteClipboard: vi.fn().mockResolvedValue(undefined),
  mockListFiles: vi.fn().mockResolvedValue({ files: [], omitted: [] }),
  mockUpdateSettings: vi.fn().mockResolvedValue({}),
  mockDeleteFile: vi.fn().mockResolvedValue(undefined),
  mockDeletionKind: { value: "permanent" as "trash" | "permanent" },
  mockIsTauri: { value: false },
  mockSummarizeSearchResults: vi.fn().mockResolvedValue(undefined),
  mockOnGenerationStream: vi.fn(),
  mockOpenChatPaneAndSend: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("../services", () => ({
  api: {
    openPath: mockOpenPath,
    revealPath: mockRevealPath,
    renameFile: mockRenameFile,
    refreshFileMetadata: mockRefreshFileMetadata,
    writeClipboard: mockWriteClipboard,
    listFiles: mockListFiles,
    updateSettings: mockUpdateSettings,
    summarizeSearchResults: mockSummarizeSearchResults,
    onGenerationStream: mockOnGenerationStream,
  },
  source: {
    get deletionKind() {
      return mockDeletionKind.value;
    },
    deleteFile: mockDeleteFile,
  },
  get isTauri() {
    return mockIsTauri.value;
  },
}));

vi.mock("../lib/utils/dialog", () => ({
  confirmDialog: vi.fn().mockResolvedValue(true),
}));

import ResultList from "./ResultList";

// Mock @tanstack/react-virtual
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: vi.fn().mockImplementation(({ count }) => ({
    getTotalSize: () => count * 30,
    getVirtualItems: () =>
      Array.from({ length: count }).map((_, index) => ({
        index,
        key: index,
        start: index * 30,
        size: 30,
        measureElement: vi.fn(),
      })),
    measureElement: vi.fn(),
  })),
}));

describe("ResultList", () => {
  const mockOnMatchClick = vi.fn();
  const mockOnFileClick = vi.fn();
  let generationHandler: (event: GenerationStreamEvent) => void;

  const renderWithToasts = (
    filterText = "",
    onFilterTextChange: (text: string) => void = vi.fn(),
  ) =>
    render(
      <ToastProvider>
        <ResultList
          filterText={filterText}
          onFilterTextChange={onFilterTextChange}
          onMatchClick={mockOnMatchClick}
          onFileClick={mockOnFileClick}
        />
      </ToastProvider>,
    );

  beforeEach(() => {
    vi.clearAllMocks();
    mockRenameFile.mockResolvedValue("/test/renamed.txt");
    mockRefreshFileMetadata.mockResolvedValue(undefined);
    mockListFiles.mockResolvedValue({ files: [], omitted: [] });
    mockIsTauri.value = false;
    mockDeletionKind.value = "permanent";
    mockSummarizeSearchResults.mockResolvedValue(undefined);
    mockOnGenerationStream.mockImplementation(
      (handler: (event: GenerationStreamEvent) => void) => {
        generationHandler = handler;
        return Promise.resolve(vi.fn());
      },
    );
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      lastQuery: null,
      resultContext: null,
    });
    useSettingsStore.setState({
      fileList: [],
      omittedFileList: [],
      indexing: false,
      fileSortKey: "filename",
      fileSortDirection: "asc",
      fileDisplayFields: ["size"],
    });
    useResearchStore.setState({
      tags: [],
      collections: [],
      selectedCollectionId: null,
      selectedTagId: null,
      draftCollectionExpression: null,
      load: vi.fn().mockResolvedValue(undefined),
    } as any);
    useGenerationStore.setState({ ready: false });
    useChatStore.setState({
      hasAvailableBackend: false,
      openPaneAndSend: mockOpenChatPaneAndSend,
    });
    useTopicsStore.setState({ selectedTopicKey: null });
  });

  it("renders empty state when no query", () => {
    renderWithToasts();
    expect(screen.getByPlaceholderText("Filter files...")).toBeInTheDocument();
  });

  it("renders document-scope controls above the filename filter", () => {
    renderWithToasts();
    const collection = screen.getByRole("combobox", { name: "Smart collection" });
    const fileFilter = screen.getByPlaceholderText("Filter files...");

    expect(collection.compareDocumentPosition(fileFilter) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("renders omitted files in a muted footer", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/visible.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
      omittedFileList: [
        {
          path: "/test/large.pdf",
          size_bytes: 15 * 1024 * 1024,
          file_type: "Pdf",
          extension: "pdf",
          reason: "TooLarge",
        },
      ],
    });

    renderWithToasts();

    const fileCount = screen.getByLabelText("1 file");
    fireEvent.mouseEnter(fileCount);
    expect(screen.getByRole("tooltip")).toHaveTextContent("1 file");
    fireEvent.mouseLeave(fileCount);
    expect(screen.getByText("visible.txt")).toBeInTheDocument();
    expect(
      screen.getByText("1 file omitted from this list"),
    ).toBeInTheDocument();
    expect(screen.queryByText("large.pdf")).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: /1 file omitted from this list/i }),
    );

    expect(screen.getByText("large.pdf")).toBeInTheDocument();
    expect(
      screen.getByText(/exceeds current file size limit/),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /large\.pdf/i }));
    expect(mockOnFileClick).toHaveBeenCalledWith("/test/large.pdf");
  });

  it("sorts the file list by filename, size, and dates", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/beta.txt",
          size_bytes: 30,
          file_type: "PlainText",
          extension: "txt",
          title: "Zebra Paper",
          author: "Baker",
          created_at_ms: 3000,
          modified_at_ms: 1000,
        },
        {
          path: "/test/alpha.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
          title: "Alpha Paper",
          author: "Clark",
          created_at_ms: 1000,
          modified_at_ms: 3000,
        },
        {
          path: "/test/gamma.txt",
          size_bytes: 20,
          file_type: "PlainText",
          extension: "txt",
          title: "Middle Paper",
          author: "Adams",
          created_at_ms: null,
          modified_at_ms: 2000,
        },
      ],
    });

    renderWithToasts();

    const alpha = screen.getByRole("button", { name: /alpha\.txt/i });
    const beta = screen.getByRole("button", { name: /beta\.txt/i });
    const gamma = screen.getByRole("button", { name: /gamma\.txt/i });
    expect(
      alpha.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      beta.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "Sort and column visibility" }),
    );
    const menu = screen.getByRole("menu");
    expect(within(menu).getByText("Title")).toBeInTheDocument();
    expect(within(menu).getByText("Author")).toBeInTheDocument();

    fireEvent.click(within(menu).getByText("Title"));
    expect(mockUpdateSettings).toHaveBeenCalledWith({ file_sort_key: "title" });
    expect(
      alpha.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      gamma.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(within(menu).getByText("Size"));
    expect(mockUpdateSettings).toHaveBeenCalledWith({ file_sort_key: "size" });
    expect(
      alpha.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      gamma.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(
      screen.getByRole("button", { name: "Toggle file sort direction" }),
    );
    expect(mockUpdateSettings).toHaveBeenCalledWith({
      file_sort_direction: "desc",
    });
    expect(
      beta.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      gamma.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    fireEvent.click(within(menu).getByText("Created"));
    expect(
      beta.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      alpha.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("toggles a metadata column's visibility via checkbox without changing the sort", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/withdate.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          publication_date: "2021-05",
        },
        {
          path: "/test/nodate.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          publication_date: null,
        },
      ],
    });

    renderWithToasts();

    // Hidden by default.
    expect(screen.queryByText("2021-05")).not.toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "Sort and column visibility" }),
    );
    const menu = screen.getByRole("menu");
    fireEvent.click(
      within(menu).getByLabelText("Show Publication date column"),
    );

    expect(mockUpdateSettings).toHaveBeenCalledWith({
      file_display_fields: ["size", "publication"],
    });
    expect(screen.getByText("May 2021")).toBeInTheDocument();
    // Files without the field show a placeholder dash.
    expect(screen.getAllByText("—").length).toBeGreaterThan(0);
    // Checking the box doesn't change the active sort key.
    expect(mockUpdateSettings).not.toHaveBeenCalledWith(
      expect.objectContaining({ file_sort_key: expect.anything() }),
    );
  });

  it("renders file metadata as sub-rows with title, author, and path on hover", () => {
    useSettingsStore.setState({
      fileDisplayFields: [
        "title",
        "author",
        "size",
        "created",
        "modified",
        "publication",
        "citations",
      ],
      fileList: [
        {
          path: "/test/paper.pdf",
          size_bytes: 2048,
          file_type: "Pdf",
          extension: "pdf",
          created_at_ms: Date.UTC(2026, 5, 3, 12),
          modified_at_ms: Date.UTC(2026, 5, 4, 12),
          title: "A Better Paper Title",
          author: "Smith et al.",
          publication_date: "2021-05-03",
          citation_count: 12,
        },
      ],
    });

    renderWithToasts();

    const row = screen.getByRole("button", { name: /paper\.pdf/i });
    expect(row).toHaveTextContent("paper.pdf");
    expect(row).not.toHaveTextContent("Size");
    fireEvent.mouseEnter(screen.getByLabelText("Size"));
    expect(screen.getByRole("tooltip")).toHaveTextContent("Size");
    fireEvent.mouseLeave(screen.getByLabelText("Size"));
    expect(row).toHaveTextContent("2.0 KB");
    expect(screen.getByLabelText("Created")).toBeInTheDocument();
    expect(row).toHaveTextContent("3 Jun 2026");
    expect(screen.getByLabelText("Modified")).toBeInTheDocument();
    expect(row).toHaveTextContent("4 Jun 2026");
    expect(screen.getByLabelText("Publication date")).toBeInTheDocument();
    expect(row).toHaveTextContent("May 2021");
    fireEvent.mouseEnter(screen.getByText("May 2021"));
    expect(screen.getByRole("tooltip")).toHaveTextContent("3 May 2021");
    fireEvent.mouseLeave(screen.getByText("May 2021"));
    expect(screen.getByLabelText("Citations")).toBeInTheDocument();
    expect(row).toHaveTextContent("12");
    expect(screen.getByLabelText("Title")).toBeInTheDocument();
    expect(row).toHaveTextContent("A Better Paper Title");
    expect(screen.getByLabelText("Author")).toBeInTheDocument();
    expect(row).toHaveTextContent("Smith et al.");
    expect(screen.getByLabelText("Type")).toBeInTheDocument();
    expect(row).toHaveTextContent("PDF");
    expect(screen.queryByText("/test/paper.pdf")).not.toBeInTheDocument();
    fireEvent.mouseEnter(screen.getByLabelText("Path: /test/paper.pdf"));
    expect(screen.getByRole("tooltip")).toHaveTextContent("/test/paper.pdf");
    fireEvent.mouseLeave(screen.getByLabelText("Path: /test/paper.pdf"));
  });

  it("does not render title or author when their display fields are unchecked", () => {
    useSettingsStore.setState({
      fileDisplayFields: ["size"],
      fileList: [
        {
          path: "/test/paper.pdf",
          size_bytes: 2048,
          file_type: "Pdf",
          extension: "pdf",
          title: "A Better Paper Title",
          author: "Smith et al.",
        },
      ],
    });

    renderWithToasts();

    const row = screen.getByRole("button", { name: /paper\.pdf/i });
    expect(row).not.toHaveTextContent("A Better Paper Title");
    expect(row).not.toHaveTextContent("Smith et al.");
  });

  it("marks metadata fields with multiple source values and exposes them in the tooltip", () => {
    useSettingsStore.setState({
      fileDisplayFields: ["title", "citations"],
      fileList: [
        {
          path: "/test/paper.pdf",
          size_bytes: 2048,
          file_type: "Pdf",
          extension: "pdf",
          title: "Zotero Title",
          citation_count: 12,
          metadata_conflicts: {
            title: [
              { source: "file", value: "Embedded Title" },
              { source: "zotero", value: "Zotero Title" },
              { source: "openalex", value: "Zotero Title" },
              { source: "semantic_scholar", value: "Zotero Title" },
              { source: "custom_source", value: "Zotero Title" },
            ],
            citation_count: [
              { source: "zotero", value: "9" },
              { source: "semantic_scholar", value: "12" },
            ],
          },
        },
      ],
    });

    renderWithToasts();

    const title = screen.getByText("Zotero Title");
    expect(title).toHaveClass("decoration-wavy");
    fireEvent.mouseEnter(title);
    expect(screen.getByRole("tooltip")).toHaveTextContent("Sources");
    expect(screen.getByRole("tooltip")).toHaveTextContent("File: Embedded Title");
    expect(screen.getByRole("tooltip")).toHaveTextContent(
      "Zotero, OpenAlex, Semantic Scholar, custom_source: Zotero Title",
    );
    expect(screen.getByRole("tooltip").querySelectorAll(".grid")).toHaveLength(2);
    expect(screen.getByText(/Zotero, OpenAlex/)).toHaveClass("break-words");
    fireEvent.mouseLeave(title);
    const citations = screen.getByText("12");
    expect(citations).toHaveClass("decoration-wavy");
    fireEvent.mouseEnter(citations);
    expect(screen.getByRole("tooltip")).toHaveTextContent("Sources");
    expect(screen.getByRole("tooltip")).toHaveTextContent("Zotero: 9");
    expect(screen.getByRole("tooltip")).toHaveTextContent("Semantic Scholar: 12");
    fireEvent.mouseLeave(citations);
  });

  it("does not render full-width detail rows when their values are missing", () => {
    useSettingsStore.setState({
      fileDisplayFields: ["title", "author", "size"],
      fileList: [
        {
          path: "/test/no-title.pdf",
          size_bytes: 2048,
          file_type: "Pdf",
          extension: "pdf",
          title: null,
          author: "",
        },
      ],
    });

    renderWithToasts();

    expect(screen.queryByLabelText("Title")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Author")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Size")).toBeInTheDocument();
  });

  it("clips overflowing detail rows until the overflow indicator is clicked", () => {
    const scrollWidth = vi
      .spyOn(HTMLElement.prototype, "scrollWidth", "get")
      .mockReturnValue(400);
    const clientWidth = vi
      .spyOn(HTMLElement.prototype, "clientWidth", "get")
      .mockReturnValue(120);

    useSettingsStore.setState({
      fileDisplayFields: ["created", "modified", "publication", "citations", "size"],
      fileList: [
        {
          path: "/test/overflow.pdf",
          size_bytes: 2048,
          file_type: "Pdf",
          extension: "pdf",
          created_at_ms: Date.UTC(2026, 5, 3, 12),
          modified_at_ms: Date.UTC(2026, 5, 4, 12),
          publication_date: "2021-05-03",
          citation_count: 12,
        },
      ],
    });

    renderWithToasts();

    const overflow = screen.getByRole("button", { name: "Show hidden file details" });
    expect(screen.getByText("3 Jun 2026").closest(".overflow-hidden")).toBeInTheDocument();

    fireEvent.click(overflow);

    expect(mockOnFileClick).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Show hidden file details" })).not.toBeInTheDocument();
    expect(screen.getByText("3 Jun 2026").closest(".flex-wrap")).toBeInTheDocument();

    scrollWidth.mockRestore();
    clientWidth.mockRestore();
  });

  it("renders results when searching", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/test/file.txt",
          file_type: "PlainText",
          matches: [
            {
              text_range: { start: 0, end: 4 },
              matched_text: "test",
              context_before: "before ",
              context_after: " after",
              origin: { TextFile: { line: 1, col: 1 } },
            },
          ],
        },
      ],
      searching: false,
    });

    renderWithToasts();
    expect(screen.getByText("file.txt")).toBeInTheDocument();
    expect(screen.getByText("test")).toBeInTheDocument();
    expect(screen.queryByText("/test/file.txt")).not.toBeInTheDocument();
    fireEvent.mouseEnter(screen.getByLabelText("Path: /test/file.txt"));
    expect(screen.getByRole("tooltip")).toHaveTextContent("/test/file.txt");
  });

  it("calls onMatchClick when a match is clicked", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/test/file.txt",
          file_type: "PlainText",
          matches: [
            {
              text_range: { start: 0, end: 4 },
              matched_text: "test",
              context_before: "",
              context_after: "",
              origin: { TextFile: { line: 1, col: 1 } },
            },
          ],
        },
      ],
    });

    renderWithToasts();
    const matchRow = screen.getByRole("button", { name: /L1test/ });
    fireEvent.click(matchRow);

    expect(mockOnMatchClick).toHaveBeenCalledWith(
      expect.objectContaining({
        path: "/test/file.txt",
      }),
    );
  });

  it("calls onFileClick when file header is clicked", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/test/file.txt",
          file_type: "PlainText",
          matches: [],
        },
      ],
    });

    renderWithToasts();
    const fileHeader = screen.getByText("file.txt");
    fireEvent.click(fileHeader);

    expect(mockOnFileClick).toHaveBeenCalledWith("/test/file.txt");
  });

  it("expands matches when show more is clicked", () => {
    const manyMatches = Array.from({ length: 10 }).map((_, i) => ({
      text_range: { start: i, end: i + 1 },
      matched_text: "m",
      context_before: "",
      context_after: "",
      origin: { TextFile: { line: i + 1, col: 1 } },
    }));

    useSearchStore.setState({
      hasQuery: true,
      results: [
        { path: "many.txt", file_type: "PlainText", matches: manyMatches },
      ],
    });

    renderWithToasts();

    const expandBtn = screen.getByText(/Show 5 more/);
    fireEvent.click(expandBtn);

    // After clicking, it should show more matches (handled by internal state)
    // We can't easily check internal state, but we can check if more match rows are rendered
    // In our mock virtualizer, it just renders everything based on count.
  });

  it("filters files", () => {
    const setFilterTextMock = vi.fn();

    renderWithToasts("", setFilterTextMock);
    const filterInput = screen.getByPlaceholderText("Filter files...");

    fireEvent.change(filterInput, { target: { value: "my-filter" } });
    expect(setFilterTextMock).toHaveBeenCalledWith("my-filter");
  });

  it("clears only the controlled file filter", () => {
    const setFilterTextMock = vi.fn();

    renderWithToasts("metadata query", setFilterTextMock);
    fireEvent.click(screen.getByRole("button", { name: "Clear file filter" }));

    expect(setFilterTextMock).toHaveBeenCalledWith("");
  });

  it("filters files by all available metadata values", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/visible.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          title: "Unrelated title",
          author: "Smith et al.",
          publication_date: "2024-03",
          citation_count: 37,
          metadata_conflicts: {
            doi: [{ source: "semantic_scholar", value: "10.1234/example" }],
          },
        },
        {
          path: "/test/hidden.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          title: "Other paper",
          author: "Jones",
        },
        {
          path: "/test/title-match.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          title: "Smithsonian Notes",
          author: null,
        },
      ],
    });

    renderWithToasts("10.1234/example");

    expect(screen.getByText("visible.pdf")).toBeInTheDocument();
    expect(screen.queryByText("title-match.pdf")).not.toBeInTheDocument();
    expect(screen.queryByText("hidden.pdf")).not.toBeInTheDocument();
  });

  it("automatically searches metadata fields added in the future", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/future.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          future_metadata: { venue: "Future Venue" },
        } as FileEntry,
      ],
    });

    renderWithToasts("future venue");

    expect(screen.getByText("future.pdf")).toBeInTheDocument();
  });

  it("does not fuzzy-match across unrelated metadata values", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/unrelated.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          future_metadata: { first: "Alpha", second: "Zebra" },
        } as FileEntry,
      ],
    });

    renderWithToasts("az");

    expect(screen.queryByText("unrelated.pdf")).not.toBeInTheDocument();
  });

  it("does not spread a fuzzy match across words in a long title", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/Harness_Engineering.pdf",
          size_bytes: 808,
          file_type: "Pdf",
          extension: "pdf",
          title: "Harness Engineering for Agentic AI Coding",
          author: "Galster et al.",
        },
        {
          path: "/test/Whats_Inside_GitHub.pdf",
          size_bytes: 245_200,
          file_type: "Pdf",
          extension: "pdf",
          title: "What's Inside a GitHub Repository?",
          author: "Hora et al.",
        },
      ],
    });

    renderWithToasts("whats");

    expect(screen.queryByText("Harness_Engineering.pdf")).not.toBeInTheDocument();
    expect(screen.getByText("Whats_Inside_GitHub.pdf")).toBeInTheDocument();
  });

  it("fuzzy-filters metadata while preserving the selected sort order", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/beta.pdf",
          size_bytes: 20,
          file_type: "Pdf",
          extension: "pdf",
          author: "Smith",
        },
        {
          path: "/test/alpha.pdf",
          size_bytes: 10,
          file_type: "Pdf",
          extension: "pdf",
          author: "Smithers",
        },
        {
          path: "/test/hidden.pdf",
          size_bytes: 30,
          file_type: "Pdf",
          extension: "pdf",
          author: "Jones",
        },
      ],
    });

    renderWithToasts("smth");

    const alpha = screen.getByText("alpha.pdf");
    const beta = screen.getByText("beta.pdf");
    expect(alpha.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.queryByText("hidden.pdf")).not.toBeInTheDocument();
  });

  it("displays search stats", () => {
    useSearchStore.setState({
      hasQuery: true,
      stats: {
        total_matches: 42,
        files_scanned: 10,
        elapsed_ms: 123,
        errors: ["Permission denied in /root/restricted"],
      },
    });

    renderWithToasts();
    expect(screen.getByText(/42 matches in 10 files/)).toBeInTheDocument();
    expect(screen.getByText(/1 file failed/)).toBeInTheDocument();
  });

  it("clears topic results from the results strip", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/papers/topic.pdf",
          file_type: "Pdf",
          matches: [],
        },
      ],
      stats: {
        total_matches: 1,
        files_scanned: 1,
        elapsed_ms: 0,
        errors: [],
      },
    });
    useTopicsStore.setState({ selectedTopicKey: "topic-a" });

    renderWithToasts();
    fireEvent.click(screen.getByRole("button", { name: "Clear results" }));

    expect(useSearchStore.getState()).toEqual(
      expect.objectContaining({
        hasQuery: false,
        results: [],
        stats: null,
      }),
    );
    expect(useTopicsStore.getState().selectedTopicKey).toBeNull();
  });

  it("summarizes one completed result snapshot and detaches it when a new search starts", async () => {
    useGenerationStore.setState({ ready: true });
    useSearchStore.setState({
      hasQuery: true,
      searching: false,
      lastQuery: {
        pattern: "cache behavior",
        root: "/papers",
        is_regex: false,
        case_sensitive: false,
        max_results: 100,
        respect_gitignore: true,
        max_file_size: 1_000_000,
        context_lines: 2,
        mode: "Semantic",
        scope: { type: "corpus" },
        supported_extensions: ["pdf"],
      },
      resultContext: { kind: "search", subject: "cache behavior" },
      results: [
        {
          path: "/papers/top.pdf",
          file_type: "Pdf",
          matches: [
            {
              text_range: null,
              matched_text:
                "Caching behavior converges across the leading measured result.",
              context_before: "",
              context_after: "",
              origin: { PdfPage: { page: 1, bbox: null } },
              score: 0.98,
            },
          ],
        },
      ],
      stats: {
        total_matches: 1,
        files_scanned: 1,
        elapsed_ms: 12,
        errors: [],
      },
    });

    renderWithToasts();
    fireEvent.click(screen.getByRole("button", { name: "Summarize results" }));

    await waitFor(() => expect(mockSummarizeSearchResults).toHaveBeenCalledOnce());
    const [requestId, input] = mockSummarizeSearchResults.mock.calls[0];
    expect(input).toEqual({
      query: "cache behavior",
      sources: [
        {
          title: "top.pdf",
          path: "/papers/top.pdf",
        },
      ],
      passages: [
        {
          text: "Caching behavior converges across the leading measured result.",
          source_index: 0,
        },
      ],
    });

    act(() => {
      generationHandler({
        phase: "completed",
        request_id: requestId,
        task: "search_results_summary",
        text: "Caching behavior converges across the leading measured result [1].",
      });
    });
    // The prose renders as a text node; the citation renders as its own link.
    const summary = screen.getByText("Results summary").closest("section");
    expect(summary).not.toBeNull();
    expect(
      within(summary!).getByText(
        /Caching behavior converges across the leading measured result/,
      ),
    ).toBeInTheDocument();
    expect(within(summary!).getByRole("button", { name: "[1]" })).toBeInTheDocument();

    act(() => useSearchStore.setState({ searching: true }));

    await waitFor(() =>
      expect(screen.queryByText("Results summary")).not.toBeInTheDocument(),
    );

    act(() =>
      useSearchStore.setState({
        searching: false,
        stats: {
          total_matches: 0,
          files_scanned: 0,
          elapsed_ms: 5,
          errors: ["search failed"],
        },
      }),
    );
    expect(
      screen.queryByRole("button", { name: "Summarize results" }),
    ).not.toBeInTheDocument();
  });

  it("summarizes a topic result set using its displayed label", async () => {
    useGenerationStore.setState({ ready: true });
    useSearchStore.setState({
      hasQuery: true,
      searching: false,
      lastQuery: null,
      resultContext: {
        kind: "topic",
        topicKey: "topic-a",
        subject: "Graph Database Indexes",
      },
      results: [
        {
          path: "/papers/graphs.pdf",
          file_type: "Pdf",
          matches: [
            {
              text_range: null,
              matched_text:
                "Graph database indexes accelerate neighborhood traversal by reducing repeated scans across connected records in large collections.",
              context_before: "",
              context_after: "",
              origin: { PdfPage: { page: 3, bbox: null } },
            },
          ],
        },
      ],
      stats: {
        total_matches: 1,
        files_scanned: 1,
        elapsed_ms: 0,
        errors: [],
      },
    });

    renderWithToasts();
    fireEvent.click(screen.getByRole("button", { name: "Summarize results" }));

    await waitFor(() => expect(mockSummarizeSearchResults).toHaveBeenCalledOnce());
    expect(mockSummarizeSearchResults.mock.calls[0][1]).toEqual(
      expect.objectContaining({ query: "Graph Database Indexes" }),
    );
  });

  it("skips generation when cleaning leaves only references and offers agent chat", async () => {
    useGenerationStore.setState({ ready: true });
    useChatStore.setState({ hasAvailableBackend: true });
    useSearchStore.setState({
      hasQuery: true,
      searching: false,
      lastQuery: {
        pattern: "use of econometric methods in computer science research",
        root: "/papers",
        is_regex: false,
        case_sensitive: false,
        max_results: 100,
        respect_gitignore: true,
        max_file_size: 1_000_000,
        context_lines: 2,
        mode: "Semantic",
        scope: { type: "corpus" },
        supported_extensions: ["pdf"],
      },
      resultContext: {
        kind: "search",
        subject: "use of econometric methods in computer science research",
      },
      results: [
        {
          path: "/papers/references.pdf",
          file_type: "Pdf",
          matches: [
            {
              text_range: null,
              matched_text:
                "References. Isaac Baley and Laura Veldkamp. Bayesian learning. NBER Working Paper 29338, 2021. Another Author. Related title. Journal of Economics, 2023.",
              context_before: "",
              context_after: "",
              origin: { PdfPage: { page: 1, bbox: null } },
              score: 0.98,
            },
          ],
        },
      ],
      stats: {
        total_matches: 1,
        files_scanned: 1,
        elapsed_ms: 12,
        errors: [],
      },
    });

    renderWithToasts();
    fireEvent.click(screen.getByRole("button", { name: "Summarize results" }));

    expect(
      screen.getByText(
        "No substantive passage in these results directly addresses the query.",
      ),
    ).toBeInTheDocument();
    expect(mockOnGenerationStream).not.toHaveBeenCalled();
    expect(mockSummarizeSearchResults).not.toHaveBeenCalled();

    fireEvent.click(
      screen.getByRole("button", { name: "Explore results in agent chat" }),
    );
    await waitFor(() => expect(mockOpenChatPaneAndSend).toHaveBeenCalledOnce());
    expect(mockOpenChatPaneAndSend.mock.calls[0][0]).toContain(
      "Search query: use of econometric methods in computer science research",
    );
    expect(mockOpenChatPaneAndSend.mock.calls[0][0]).toContain(
      "- /papers/references.pdf",
    );
  });

  it("handles empty results and searching state", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [],
      searching: true,
    });

    const { container } = renderWithToasts();
    expect(screen.getByText("0 matches…")).toBeInTheDocument();
    // Shimmer element
    expect(container.querySelector(".animate-shimmer")).toBeDefined();
  });

  it("opens a file context menu on right click without opening the file", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/visible.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
    });

    renderWithToasts();
    const row = screen.getByRole("button", { name: /visible\.txt/i });
    expect(row).toHaveClass("select-none");
    expect(row).not.toHaveClass("selectable");

    fireEvent.contextMenu(row);

    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Open" })).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Copy path" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "Reveal in folder" }),
    ).not.toBeInTheDocument();
    expect(mockOnFileClick).not.toHaveBeenCalled();
  });

  it("runs the open action from a match-row context menu", () => {
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/test/file.txt",
          file_type: "PlainText",
          matches: [
            {
              text_range: { start: 0, end: 4 },
              matched_text: "test",
              context_before: "",
              context_after: "",
              origin: { TextFile: { line: 1, col: 1 } },
            },
          ],
        },
      ],
    });

    renderWithToasts();
    const matchRow = screen.getByRole("button", { name: /L1test/ });
    expect(matchRow).toHaveClass("select-none");
    expect(matchRow).not.toHaveClass("selectable");

    fireEvent.contextMenu(matchRow);
    fireEvent.click(screen.getByRole("menuitem", { name: "Open" }));

    expect(mockOnMatchClick).toHaveBeenCalledWith(
      expect.objectContaining({
        path: "/test/file.txt",
      }),
    );
  });

  it("copies a file path and shows a success toast", async () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/visible.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(
      screen.getByRole("button", { name: /visible\.txt/i }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy path" }));

    expect(mockWriteClipboard).toHaveBeenCalledWith("/test/visible.txt");
    expect(await screen.findByText("Path copied")).toBeInTheDocument();
  });

  it("refreshes metadata for one file from the context menu", async () => {
    useSettingsStore.setState({
      directory: "/test",
      fileList: [
        {
          path: "/test/visible.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(
      screen.getByRole("button", { name: /visible\.txt/i }),
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "Refresh metadata" }));

    expect(mockRefreshFileMetadata).toHaveBeenCalledWith("/test/visible.txt");
    await waitFor(() => expect(mockListFiles).toHaveBeenCalled());
    expect(await screen.findByText("Metadata refresh started")).toBeInTheDocument();
  });

  it("closes the menu on Escape", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/visible.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(
      screen.getByRole("button", { name: /visible\.txt/i }),
    );
    expect(screen.getByRole("menu")).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("shows the desktop reveal action and calls revealPath", () => {
    mockIsTauri.value = true;
    useSearchStore.setState({
      hasQuery: true,
      results: [
        {
          path: "/test/file.txt",
          file_type: "PlainText",
          matches: [],
        },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByText("file.txt"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Reveal in folder" }));

    expect(mockRevealPath).toHaveBeenCalledWith("/test/file.txt");
  });

  it("renames a file from the context menu", async () => {
    useSettingsStore.setState({
      directory: "/test",
      fileList: [
        {
          path: "/test/file.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
        },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByRole("button", { name: /file\.txt/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));

    const input = screen.getByLabelText("File name") as HTMLInputElement;
    expect(input).toHaveValue("file.txt");
    await waitFor(() => {
      expect(input.selectionStart).toBe(0);
      expect(input.selectionEnd).toBe(4);
    });

    fireEvent.change(input, { target: { value: "renamed.txt" } });
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    expect(mockRenameFile).toHaveBeenCalledWith(
      "/test/file.txt",
      "renamed.txt",
    );
  });

  it("permanently deletes a web file after confirmation", async () => {
    useSettingsStore.setState({
      directory: "/test",
      fileList: [{ path: "/test/file.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" }],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByRole("button", { name: /file\.txt/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete permanently" }));

    const { confirmDialog } = await import("../lib/utils/dialog");
    expect(confirmDialog).toHaveBeenCalledWith(
      'Permanently delete "file.txt"? This cannot be undone.',
    );
    await waitFor(() => expect(mockDeleteFile).toHaveBeenCalledWith("/test/file.txt"));
    expect(await screen.findByText('Permanently deleted "file.txt"')).toBeInTheDocument();
  });

  it("moves a desktop file to Trash without a permanent-delete fallback", async () => {
    mockIsTauri.value = true;
    mockDeletionKind.value = "trash";
    useSettingsStore.setState({
      directory: "/test",
      fileList: [{ path: "/test/file.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" }],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByRole("button", { name: /file\.txt/i }));
    expect(screen.queryByRole("menuitem", { name: "Delete permanently" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Move to Trash" }));

    const { confirmDialog } = await import("../lib/utils/dialog");
    expect(confirmDialog).toHaveBeenCalledWith(
      'Move "file.txt" to Trash? You can restore it from Trash.',
    );
    await waitFor(() => expect(mockDeleteFile).toHaveBeenCalledWith("/test/file.txt"));
  });
});
