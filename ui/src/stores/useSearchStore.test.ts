import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import type { FileMatches, SearchQuery, SearchStats } from "../lib/types";
import { useSearchStore } from "./useSearchStore";

vi.mock("../services", () => ({
  api: {
    search: vi.fn(),
    cancelSearch: vi.fn(),
    getIndexStatus: vi.fn(),
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
      currentSearchId: null,
      lastQuery: null,
    });
  });

  it("has an empty initial search state", () => {
    expect(useSearchStore.getState()).toEqual(
      expect.objectContaining({ results: [], searching: false, hasQuery: false }),
    );
  });

  it("sets query presence", () => {
    useSearchStore.getState().setHasQuery(true);
    expect(useSearchStore.getState().hasQuery).toBe(true);
  });

  it("performs a search and updates results", async () => {
    const query: SearchQuery = {
      pattern: "test",
      root: "/root",
      is_regex: false,
      case_sensitive: false,
      max_results: 100,
      respect_gitignore: true,
      max_file_size: 1000,
      context_lines: 2,
      mode: "Grep",
      scope: { type: "corpus" },
      supported_extensions: [],
    };
    const fileMatches: FileMatches = {
      path: "/root/file.txt",
      file_type: "PlainText",
      matches: [],
    };
    const stats: SearchStats = {
      files_scanned: 1,
      total_matches: 0,
      elapsed_ms: 10,
      errors: [],
    };
    vi.mocked(api.search).mockImplementation(async (_query, onResult, onDone) => {
      onResult(fileMatches);
      onDone(stats);
      return "search-id-123";
    });

    await useSearchStore.getState().search(query);

    expect(useSearchStore.getState()).toEqual(
      expect.objectContaining({
        results: [fileMatches],
        stats,
        searching: false,
        lastQuery: query,
      }),
    );
  });

  it("clears stale results when a new search returns none", async () => {
    useSearchStore.setState({
      results: [{ path: "/old.ts", file_type: "PlainText", matches: [] }],
    });
    vi.mocked(api.search).mockImplementation(async (_query, _onResult, onDone) => {
      onDone({ files_scanned: 5, total_matches: 0, elapsed_ms: 10, errors: [] });
      return "search-id-456";
    });

    await useSearchStore.getState().search({ pattern: "nomatch" } as SearchQuery);

    expect(useSearchStore.getState().results).toEqual([]);
  });

  it("records search errors", async () => {
    vi.mocked(api.search).mockRejectedValue(new Error("Network Error"));

    await useSearchStore.getState().search({} as SearchQuery);

    expect(useSearchStore.getState().searching).toBe(false);
    expect(useSearchStore.getState().stats?.errors).toContain("Error: Network Error");
  });

  it("replays the last search", async () => {
    const query = { pattern: "replay", mode: "Grep" } as SearchQuery;
    const search = vi.fn();
    useSearchStore.setState({ lastQuery: query, search });

    await useSearchStore.getState().replaySearch();

    expect(search).toHaveBeenCalledWith(query);
  });

  it("only replays semantic searches against a usable index", async () => {
    const query = {
      pattern: "replay",
      mode: "Semantic",
      root: "/indexed",
    } as SearchQuery;
    const search = vi.fn();
    useSearchStore.setState({ lastQuery: query, search });
    vi.mocked(api.getIndexStatus).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      root_path: "/indexed",
    } as never);

    await useSearchStore.getState().replaySearch();
    expect(search).not.toHaveBeenCalled();

    vi.mocked(api.getIndexStatus).mockResolvedValue({
      indexed_files: 10,
      total_chunks: 20,
      root_path: "/indexed",
    } as never);
    await useSearchStore.getState().replaySearch();
    expect(search).toHaveBeenCalledWith(query);
  });

  it("defers semantic search without clearing visible results", () => {
    const results: FileMatches[] = [
      { path: "/f.ts", file_type: "PlainText", matches: [] },
    ];
    useSearchStore.setState({
      results,
      stats: { files_scanned: 1, total_matches: 1, elapsed_ms: 10, errors: [] },
    });

    useSearchStore
      .getState()
      .deferSemanticSearch({
        pattern: "queued",
        mode: "Semantic",
        root: "/root",
      } as SearchQuery);

    expect(useSearchStore.getState()).toEqual(
      expect.objectContaining({
        results,
        stats: null,
        lastQuery: expect.objectContaining({ pattern: "queued", mode: "Semantic" }),
      }),
    );
  });

  it("invalidates semantic results only for the matching root", () => {
    const results: FileMatches[] = [
      { path: "/f.ts", file_type: "PlainText", matches: [] },
    ];
    useSearchStore.setState({
      lastQuery: {
        pattern: "queued",
        mode: "Semantic",
        root: "/root",
      } as SearchQuery,
      results,
    });

    useSearchStore.getState().invalidateSemanticResultsForRoot("/other");
    expect(useSearchStore.getState().results).toEqual(results);

    useSearchStore.getState().invalidateSemanticResultsForRoot("/root");
    expect(useSearchStore.getState().results).toEqual([]);
  });

  it("clears only search results", () => {
    useSearchStore.setState({
      results: [{ path: "/f.ts", file_type: "PlainText", matches: [] }],
      stats: { files_scanned: 1, total_matches: 1, elapsed_ms: 10, errors: [] },
    });

    useSearchStore.getState().clearResults();

    expect(useSearchStore.getState().results).toEqual([]);
    expect(useSearchStore.getState().stats).toBeNull();
  });

  it("handles search cancellation errors", async () => {
    vi.mocked(api.search).mockRejectedValue(
      Object.assign(new Error("AbortError"), { name: "AbortError" }),
    );

    await useSearchStore.getState().search({ pattern: "test" } as SearchQuery);

    expect(useSearchStore.getState().searching).toBe(false);
  });
});
