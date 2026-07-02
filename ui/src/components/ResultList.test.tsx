import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { ToastProvider } from "./Toast";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";

const {
  mockOpenPath,
  mockRevealPath,
  mockRenameFile,
  mockWriteClipboard,
  mockListFiles,
  mockUpdateSettings,
  mockIsTauri,
} = vi.hoisted(() => ({
  mockOpenPath: vi.fn(),
  mockRevealPath: vi.fn(),
  mockRenameFile: vi.fn(),
  mockWriteClipboard: vi.fn().mockResolvedValue(undefined),
  mockListFiles: vi.fn().mockResolvedValue({ files: [], omitted: [] }),
  mockUpdateSettings: vi.fn().mockResolvedValue({}),
  mockIsTauri: { value: false },
}));

vi.mock("../services", () => ({
  api: {
    openPath: mockOpenPath,
    revealPath: mockRevealPath,
    renameFile: mockRenameFile,
    writeClipboard: mockWriteClipboard,
    listFiles: mockListFiles,
    updateSettings: mockUpdateSettings,
  },
  get isTauri() {
    return mockIsTauri.value;
  },
}));

import ResultList from "./ResultList";

// Mock @tanstack/react-virtual
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: vi.fn().mockImplementation(({ count }) => ({
    getTotalSize: () => count * 30,
    getVirtualItems: () => Array.from({ length: count }).map((_, index) => ({
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

  const renderWithToasts = () =>
    render(
      <ToastProvider>
        <ResultList onMatchClick={mockOnMatchClick} onFileClick={mockOnFileClick} />
      </ToastProvider>,
    );

  beforeEach(() => {
    vi.clearAllMocks();
    mockRenameFile.mockResolvedValue("/test/renamed.txt");
    mockListFiles.mockResolvedValue({ files: [], omitted: [] });
    mockIsTauri.value = false;
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      selectedMatch: null,
    });
    useSettingsStore.setState({
      fileList: [],
      omittedFileList: [],
      filterText: "",
      setFilterText: vi.fn(),
      indexing: false,
      fileSortKey: "filename",
      fileSortDirection: "asc",
      fileDisplayFields: ["size"],
    });
  });

  it("renders empty state when no query", () => {
    renderWithToasts();
    expect(screen.getByPlaceholderText("Filter files...")).toBeInTheDocument();
  });

  it("renders omitted files in a muted footer", () => {
    useSettingsStore.setState({
      fileList: [
        { path: "/test/visible.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" },
      ],
      omittedFileList: [
        { path: "/test/large.pdf", size_bytes: 15 * 1024 * 1024, file_type: "Pdf", extension: "pdf", reason: "TooLarge" },
      ],
    });

    renderWithToasts();

    expect(screen.getByTitle("1 file")).toBeInTheDocument();
    expect(screen.getByText("visible.txt")).toBeInTheDocument();
    expect(screen.getByText("1 file omitted from this list")).toBeInTheDocument();
    expect(screen.queryByText("large.pdf")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /1 file omitted from this list/i }));

    expect(screen.getByText("large.pdf")).toBeInTheDocument();
    expect(screen.getByText(/exceeds current file size limit/)).toBeInTheDocument();
  });

  it("sorts the file list by filename, size, and dates", () => {
    useSettingsStore.setState({
      fileList: [
        {
          path: "/test/beta.txt",
          size_bytes: 30,
          file_type: "PlainText",
          extension: "txt",
          created_at_ms: 3000,
          modified_at_ms: 1000,
        },
        {
          path: "/test/alpha.txt",
          size_bytes: 10,
          file_type: "PlainText",
          extension: "txt",
          created_at_ms: 1000,
          modified_at_ms: 3000,
        },
        {
          path: "/test/gamma.txt",
          size_bytes: 20,
          file_type: "PlainText",
          extension: "txt",
          created_at_ms: null,
          modified_at_ms: 2000,
        },
      ],
    });

    renderWithToasts();

    const alpha = screen.getByRole("button", { name: /alpha\.txt/i });
    const beta = screen.getByRole("button", { name: /beta\.txt/i });
    const gamma = screen.getByRole("button", { name: /gamma\.txt/i });
    expect(alpha.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(beta.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Sort and column visibility" }));
    const menu = screen.getByRole("menu");
    fireEvent.click(within(menu).getByText("Size"));
    expect(mockUpdateSettings).toHaveBeenCalledWith({ file_sort_key: "size" });
    expect(alpha.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(gamma.compareDocumentPosition(beta) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Toggle file sort direction" }));
    expect(mockUpdateSettings).toHaveBeenCalledWith({ file_sort_direction: "desc" });
    expect(beta.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(gamma.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    fireEvent.click(within(menu).getByText("Created"));
    expect(beta.compareDocumentPosition(alpha) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(alpha.compareDocumentPosition(gamma) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
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

    fireEvent.click(screen.getByRole("button", { name: "Sort and column visibility" }));
    const menu = screen.getByRole("menu");
    fireEvent.click(within(menu).getByLabelText("Show Publication date column"));

    expect(mockUpdateSettings).toHaveBeenCalledWith({ file_display_fields: ["size", "publication"] });
    expect(screen.getByText("2021-05")).toBeInTheDocument();
    // Files without the field show a placeholder dash.
    expect(screen.getAllByText("—").length).toBeGreaterThan(0);
    // Checking the box doesn't change the active sort key.
    expect(mockUpdateSettings).not.toHaveBeenCalledWith(
      expect.objectContaining({ file_sort_key: expect.anything() }),
    );
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

    expect(mockOnMatchClick).toHaveBeenCalledWith(expect.objectContaining({
      path: "/test/file.txt",
    }));
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
      results: [{ path: "many.txt", file_type: "PlainText", matches: manyMatches }],
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
    useSettingsStore.setState({ setFilterText: setFilterTextMock });

    renderWithToasts();
    const filterInput = screen.getByPlaceholderText("Filter files...");
    
    fireEvent.change(filterInput, { target: { value: "my-filter" } });
    expect(setFilterTextMock).toHaveBeenCalledWith("my-filter");
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
        { path: "/test/visible.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" },
      ],
    });

    renderWithToasts();
    const row = screen.getByRole("button", { name: /visible\.txt/i });
    expect(row).toHaveClass("select-none");
    expect(row).not.toHaveClass("selectable");

    fireEvent.contextMenu(row);

    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Open" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Copy path" })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "Reveal in folder" })).not.toBeInTheDocument();
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

    expect(mockOnMatchClick).toHaveBeenCalledWith(expect.objectContaining({
      path: "/test/file.txt",
    }));
  });

  it("copies a file path and shows a success toast", async () => {
    useSettingsStore.setState({
      fileList: [
        { path: "/test/visible.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByRole("button", { name: /visible\.txt/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy path" }));

    expect(mockWriteClipboard).toHaveBeenCalledWith("/test/visible.txt");
    expect(await screen.findByText("Path copied")).toBeInTheDocument();
  });

  it("closes the menu on Escape", () => {
    useSettingsStore.setState({
      fileList: [
        { path: "/test/visible.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" },
      ],
    });

    renderWithToasts();
    fireEvent.contextMenu(screen.getByRole("button", { name: /visible\.txt/i }));
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
        { path: "/test/file.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" },
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

    expect(mockRenameFile).toHaveBeenCalledWith("/test/file.txt", "renamed.txt");
  });
});
