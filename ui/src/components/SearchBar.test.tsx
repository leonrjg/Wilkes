import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import SearchBar from "./SearchBar";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { api } from "../services";
import { useResearchStore } from "../stores/useResearchStore";

// Mock the components that might be passed as slots
const MockSourceSlot = () => <div data-testid="source-slot">Source Slot</div>;

describe("SearchBar", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Reset stores to a known state
    useSearchStore.setState({
      search: vi.fn(),
      deferSemanticSearch: vi.fn(),
      searching: false,
      setHasQuery: vi.fn(),
      clearResults: vi.fn(),
    });
    useSettingsStore.setState({
      directory: "/test/dir",
      respectGitignore: true,
      maxFileSize: 1000,
      contextLines: 2,
      supportedExtensions: [],
      preferSemantic: false,
      setPreferSemantic: vi.fn(),
    });
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      readyGlobally: true,
      refreshGlobalStatus: vi.fn().mockResolvedValue(true),
      ensureCurrentRootIndexed: vi.fn().mockResolvedValue(false),
      status: "ready",
      buildRoot: null,
      blockedRoot: null,
      indexStatus: null,
      error: null,
    } as any);
    useResearchStore.setState({
      tags: [],
      collections: [],
      history: [],
      selectedCollectionId: null,
      selectedTagId: null,
      draftCollectionExpression: null,
      load: vi.fn().mockResolvedValue(undefined),
      loadHistory: vi.fn().mockResolvedValue(undefined),
    } as any);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("renders correctly", () => {
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const input = screen.getByPlaceholderText("Search…");
    expect(input).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Search all directories" })).toHaveAttribute("aria-pressed", "false");
    expect(screen.queryByText(/^All$/i)).not.toBeInTheDocument();
    expect(screen.getByTestId("source-slot")).toBeInTheDocument();
  });

  it("updates pattern and triggers search after debounce", async () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const input = screen.getByPlaceholderText("Search…");

    fireEvent.change(input, { target: { value: "test query" } });

    // Should not have called search yet due to debounce
    expect(searchMock).not.toHaveBeenCalled();

    // Fast-forward time
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({
        pattern: "test query",
      }),
    );
  });

  it("clears the query and results from the search bar", () => {
    const clearResults = vi.fn();
    useSearchStore.setState({ clearResults });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const input = screen.getByPlaceholderText("Search…");

    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();

    fireEvent.change(input, { target: { value: "test query" } });
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));

    expect(input).toHaveValue("");
    expect(input).toHaveFocus();
    expect(clearResults).toHaveBeenCalledOnce();
    expect(screen.queryByRole("button", { name: "Clear search" })).not.toBeInTheDocument();
  });

  it("keeps the clear button beside the query within a shrinkable ceiling", () => {
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const input = screen.getByPlaceholderText("Search…");
    const longQuery = "detectable coding-agent traces ".repeat(100);

    fireEvent.change(input, { target: { value: longQuery } });

    const inputSizer = input.parentElement;
    const searchField = inputSizer?.parentElement;
    const clearButton = screen.getByRole("button", { name: "Clear search" });

    expect(input).toHaveValue(longQuery);
    expect(inputSizer).toHaveClass("min-w-0", "max-w-full", "flex-shrink", "overflow-hidden");
    expect(inputSizer?.firstElementChild).toHaveClass("pr-1");
    expect(searchField).toHaveClass("min-w-0", "flex-1");
    expect(inputSizer?.nextElementSibling).toBe(clearButton);
    expect(screen.getByRole("button", { name: "Search all directories" })).toBeInTheDocument();
  });

  it("toggles regex option", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const regexToggle = screen.getByRole("button", { name: "Regular expression" });

    fireEvent.click(regexToggle);

    // It should immediately trigger search if there is a pattern, 
    // but here pattern is empty, so it might not trigger until pattern is set.
    // Wait, the component triggers search on toggle if pattern is not empty.
    
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "test" } });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({
        is_regex: true,
      }),
    );
  });

  it("toggles combined mode", () => {
    const setPreferSemanticMock = vi.fn();
    useSettingsStore.setState({ setPreferSemantic: setPreferSemanticMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const combinedToggle = screen.getByRole("button", { name: "Combined search" });

    fireEvent.click(combinedToggle);

    expect(setPreferSemanticMock).toHaveBeenCalledWith(true);
  });

  it("searches wording and meaning together in combined mode", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock } as any);
    useSemanticStore.setState({ readyForCurrentRoot: true } as any);
    useSettingsStore.setState({ preferSemantic: true } as any);

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    fireEvent.change(screen.getByPlaceholderText("Search…"), {
      target: { value: "instrumental variables weak identification" },
    });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({ mode: "Hybrid", is_regex: false }),
    );
  });

  it("restricts a combined search to the exact lane while a regular expression is in force", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock } as any);
    useSemanticStore.setState({ readyForCurrentRoot: true } as any);
    useSettingsStore.setState({ preferSemantic: true } as any);

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const combinedToggle = screen.getByRole("button", { name: "Combined search" });
    expect(combinedToggle).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "Regular expression" }));
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "weak.*ident" } });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).toHaveBeenLastCalledWith(
      expect.objectContaining({ mode: "Grep", is_regex: true }),
    );
    // The preference is untouched, so the checkbox returns on its own.
    expect(screen.getByRole("button", { name: "Combined search" })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
  });

  it("cancels an ongoing index when combined mode is unchecked", () => {
    const cancelEmbed = vi.spyOn(api, "cancelEmbed").mockResolvedValue(undefined);
    const setPreferSemanticMock = vi.fn();
    useSettingsStore.setState({
      preferSemantic: true,
      setPreferSemantic: setPreferSemanticMock,
    });
    useSemanticStore.setState({
      buildRoot: "/test/dir",
      status: "building",
    } as any);

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    fireEvent.click(screen.getByRole("button", { name: "Combined search" }));

    expect(setPreferSemanticMock).toHaveBeenCalledWith(false);
    expect(cancelEmbed).toHaveBeenCalledOnce();
  });

  it("toggles case sensitivity", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const caseToggle = screen.getByRole("button", { name: "Case sensitive" });

    fireEvent.click(caseToggle);
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "test" } });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({
        case_sensitive: true,
      }),
    );
  });

  it("sends the backend-owned all scope without directory paths", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);

    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "everywhere" } });
    act(() => vi.advanceTimersByTime(300));
    searchMock.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Search all directories" }));

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({ scope: { type: "all" }, root: "/test/dir" }),
    );
    expect(searchMock.mock.calls[0][0]).not.toHaveProperty("roots");
  });

  it("queues a semantic search and triggers indexing when no index is ready", () => {
    const deferSemanticSearch = vi.fn();
    const ensureCurrentRootIndexed = vi.fn().mockResolvedValue(false);
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock, deferSemanticSearch } as any);
    useSemanticStore.setState({
      readyForCurrentRoot: false,
      ensureCurrentRootIndexed,
    } as any);
    useSettingsStore.setState({ preferSemantic: true } as any);

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "semantic query" } });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(deferSemanticSearch).toHaveBeenCalledWith(
      expect.objectContaining({ pattern: "semantic query", mode: "Hybrid" }),
    );
    expect(ensureCurrentRootIndexed).toHaveBeenCalled();
    expect(searchMock).not.toHaveBeenCalled();
  });

  it("uses document tags as a normal search filter", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });
    useSettingsStore.setState({ refreshFileList: vi.fn().mockResolvedValue(undefined) } as any);
    useResearchStore.setState({
      tags: [{ id: "reviewed", name: "Reviewed", color: null }],
    });
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "methods" } });
    act(() => vi.advanceTimersByTime(300));
    searchMock.mockClear();

    act(() => useResearchStore.setState({ selectedTagId: "reviewed" }));

    expect(searchMock).toHaveBeenCalledWith(
      expect.objectContaining({ pattern: "methods", tag_ids: ["reviewed"] }),
    );
  });

  it("exposes search history beside the normal search input", () => {
    useResearchStore.setState({
      history: [{
        id: "history-1",
        query: {
          pattern: "prior query",
          is_regex: false,
          case_sensitive: false,
          root: "/test/dir",
          max_results: 100,
          respect_gitignore: true,
          max_file_size: 0,
          context_lines: 2,
          mode: "Grep",
          scope: { type: "corpus" },
          supported_extensions: [],
          collection_id: null,
          tag_ids: [],
        },
        initiated_by: "app",
        started_at_ms: 1,
        result_count: 2,
        status: "completed",
      }],
    } as any);
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);

    fireEvent.click(screen.getByRole("button", { name: "Search history" }));

    expect(screen.getByText("prior query")).toBeInTheDocument();
  });

  it("does not auto-trigger indexing from stale query state after semantic invalidation", () => {
    const deferSemanticSearch = vi.fn();
    const ensureCurrentRootIndexed = vi.fn().mockResolvedValue(false);
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock, deferSemanticSearch } as any);
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      blockedRoot: null,
      ensureCurrentRootIndexed,
    } as any);

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "before delete" } });
    act(() => {
      vi.advanceTimersByTime(300);
    });

    searchMock.mockClear();
    ensureCurrentRootIndexed.mockClear();
    deferSemanticSearch.mockClear();

    useSemanticStore.setState({ readyForCurrentRoot: false, blockedRoot: "/test/dir" } as any);
    act(() => {
      vi.advanceTimersByTime(300);
    });

    expect(searchMock).not.toHaveBeenCalled();
    expect(ensureCurrentRootIndexed).not.toHaveBeenCalled();
    expect(deferSemanticSearch).not.toHaveBeenCalled();
  });
});
