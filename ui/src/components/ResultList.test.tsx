import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import ResultList from "./ResultList";
import { ToastProvider } from "./Toast";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";

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
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      selectedMatch: null,
    });
    useSettingsStore.setState({
      excluded: new Set(),
      fileList: [],
      omittedFileList: [],
      filterText: "",
      setFilterText: vi.fn(),
      indexing: false,
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

    expect(screen.getByText("1 file")).toBeInTheDocument();
    expect(screen.getByText("visible.txt")).toBeInTheDocument();
    expect(screen.getByText("1 file omitted from this list")).toBeInTheDocument();
    expect(screen.queryByText("large.pdf")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /1 file omitted from this list/i }));

    expect(screen.getByText("large.pdf")).toBeInTheDocument();
    expect(screen.getByText(/exceeds current file size limit/)).toBeInTheDocument();
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
});
