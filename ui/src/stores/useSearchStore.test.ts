import { describe, it, expect, vi, beforeEach } from "vitest";
import { useSearchStore } from "./useSearchStore";
import { api } from "../services";
import type { SearchQuery, FileMatches, SearchStats, MatchRef, PreviewData } from "../lib/types";

vi.mock("../services", () => ({
  api: {
    search: vi.fn(),
    cancelSearch: vi.fn(),
    preview: vi.fn(),
  },
}));

describe("useSearchStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
      selectedMatch: null,
      previewData: null,
      previewLoading: false,
      currentSearchId: null,
      lastQuery: null,
    });
  });

  it("should have initial state", () => {
    const state = useSearchStore.getState();
    expect(state.results).toEqual([]);
    expect(state.searching).toBe(false);
  });

  it("should set hasQuery", () => {
    useSearchStore.getState().setHasQuery(true);
    expect(useSearchStore.getState().hasQuery).toBe(true);
  });

  it("should perform a search and update results", async () => {
    const mockQuery: SearchQuery = {
      pattern: "test",
      root: "/root",
      is_regex: false,
      case_sensitive: false,
      file_type_filters: [],
      max_results: 100,
      respect_gitignore: true,
      max_file_size: 1000,
      context_lines: 2,
      mode: "Grep",
      supported_extensions: [],
    };

    const mockFileMatch: FileMatches = {
      path: "/root/file.txt",
      file_type: "PlainText",
      matches: [],
    };

    const mockStats: SearchStats = {
      files_scanned: 1,
      total_matches: 0,
      elapsed_ms: 10,
      errors: [],
    };

    (api.search as any).mockImplementation((query: any, onResult: any, onDone: any) => {
      onResult(mockFileMatch);
      onDone(mockStats);
      return Promise.resolve("search-id-123");
    });

    await useSearchStore.getState().search(mockQuery);

    const state = useSearchStore.getState();
    expect(state.results).toEqual([mockFileMatch]);
    expect(state.stats).toEqual(mockStats);
    expect(state.searching).toBe(false);
    expect(state.lastQuery).toEqual(mockQuery);
  });

  it("should handle search errors", async () => {
    (api.search as any).mockRejectedValue(new Error("Network Error"));

    await useSearchStore.getState().search({} as any);

    const state = useSearchStore.getState();
    expect(state.searching).toBe(false);
    expect(state.stats?.errors).toContain("Error: Network Error");
  });

  it("should select a match and load preview", async () => {
    const mockMatchRef: MatchRef = {
      path: "/root/file.txt",
      origin: { TextFile: { line: 1, col: 1 } },
    };

    const mockPreviewData: PreviewData = {
      Text: {
        content: "test content",
        language: "text",
        highlight_line: 1,
        highlight_range: { start: 0, end: 4 },
      },
    };

    (api.preview as any).mockResolvedValue(mockPreviewData);

    await useSearchStore.getState().selectMatch(mockMatchRef);

    const state = useSearchStore.getState();
    expect(state.selectedMatch).toEqual(mockMatchRef);
    expect(state.previewData).toEqual(mockPreviewData);
    expect(state.previewLoading).toBe(false);
  });

  it("should clear preview", () => {
    useSearchStore.setState({
      selectedMatch: {} as any,
      previewData: {} as any,
    });

    useSearchStore.getState().clearPreview();

    const state = useSearchStore.getState();
    expect(state.selectedMatch).toBeNull();
    expect(state.previewData).toBeNull();
  });

  it("should replay search", async () => {
    const mockQuery: SearchQuery = { pattern: "replay" } as any;
    useSearchStore.setState({ lastQuery: mockQuery });
    
    const searchMock = vi.fn();
    // We need to mock the search function on the store itself because replaySearch calls get().search
    useSearchStore.setState({ search: searchMock });

    await useSearchStore.getState().replaySearch();
    expect(searchMock).toHaveBeenCalledWith(mockQuery);
  });

  it("should handle search cancellation by user", async () => {
    (api.search as any).mockImplementation(() => {
      return new Promise((_, reject) => {
        const err = new Error("AbortError");
        err.name = "AbortError";
        reject(err);
      });
    });

    await useSearchStore.getState().search({ pattern: "test" } as any);
    expect(useSearchStore.getState().searching).toBe(false);
  });
});
