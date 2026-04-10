import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import SearchBar from "./SearchBar";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";

// Mock the components that might be passed as slots
const MockSourceSlot = () => <div data-testid="source-slot">Source Slot</div>;

describe("SearchBar", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // Reset stores to a known state
    useSearchStore.setState({
      search: vi.fn(),
      searching: false,
      setHasQuery: vi.fn(),
    });
    useSettingsStore.setState({
      directory: "/test/dir",
      respectGitignore: true,
      maxFileSize: 1000,
      contextLines: 2,
      supportedExtensions: [],
      fileList: [],
      excluded: new Set(),
      preferSemantic: false,
      setPreferSemantic: vi.fn(),
    });
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      status: "ready",
      buildRoot: null,
      indexStatus: null,
      error: null,
    } as any);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders correctly", () => {
    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    expect(screen.getByPlaceholderText("Search…")).toBeInTheDocument();
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

  it("toggles regex option", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const regexToggle = screen.getByTitle("Regular expression");

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

  it("toggles semantic mode", () => {
    const setPreferSemanticMock = vi.fn();
    useSettingsStore.setState({ setPreferSemantic: setPreferSemanticMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const semanticToggle = screen.getByTitle("Semantic search");

    fireEvent.click(semanticToggle);

    expect(setPreferSemanticMock).toHaveBeenCalledWith(true);
  });

  it("toggles case sensitivity", () => {
    const searchMock = vi.fn();
    useSearchStore.setState({ search: searchMock });

    render(<SearchBar sourceSlot={<MockSourceSlot />} />);
    const caseToggle = screen.getByTitle("Case sensitive");

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
});
