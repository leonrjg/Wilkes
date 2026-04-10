import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import { useSearchStore } from "./useSearchStore";
import { useSemanticStore } from "./useSemanticStore";
import { useSettingsStore } from "./useSettingsStore";

vi.mock("../services", () => ({
  api: {
    getIndexStatus: vi.fn(),
    buildIndex: vi.fn().mockResolvedValue(undefined),
    listFiles: vi.fn().mockResolvedValue([]),
  },
}));

describe("useSemanticStore", () => {
  const flushAsync = async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  };

  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({
      directory: "",
      preferSemantic: false,
      semantic: {
        enabled: true,
        selected: {
          engine: "SBERT",
          model: "intfloat/e5-small-v2",
          dimension: 384,
        },
        engine_devices: {},
        index_path: null,
        custom_models: [],
        chunk_size: 1000,
        chunk_overlap: 200,
        worker_timeout_secs: 300,
      },
      load: async () => {
        const settings = await (api as any).getSettings();
        useSettingsStore.setState({
          bookmarks: settings.bookmarked_dirs,
          recentDirs: settings.recent_dirs || [],
          directory: settings.last_directory ?? "",
          semantic: settings.semantic,
          respectGitignore: settings.respect_gitignore,
          maxFileSize: settings.max_file_size,
          supportedExtensions: settings.supported_extensions || [],
          preferSemantic: settings.search_prefer_semantic,
          theme: settings.theme,
          maxResults: settings.max_results ?? 0,
        } as any);
      },
    } as any);
    useSearchStore.setState({
      replaySearch: vi.fn().mockResolvedValue(undefined),
    } as any);
    useSemanticStore.setState({
      indexStatus: null,
      readyForCurrentRoot: false,
      status: "idle",
      buildRoot: null,
      blockedRoot: null,
      error: null,
    });
  });

  it("starts indexing when semantic is preferred for an unindexed root", async () => {
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/indexed",
      db_size_bytes: null,
    });

    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);

    await flushAsync();

    expect(api.buildIndex).toHaveBeenCalledWith(
      "/project",
      expect.objectContaining({ model: "intfloat/e5-small-v2" }),
    );
    expect(useSemanticStore.getState().status).toBe("building");
  });

  it("does not start indexing when the current root is already usable", async () => {
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 5,
      total_chunks: 10,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/project",
      db_size_bytes: null,
    });

    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);

    await flushAsync();

    expect(api.buildIndex).not.toHaveBeenCalled();
    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(true);
    expect(useSemanticStore.getState().status).toBe("ready");
  });

  it("kicks off indexing during startup load when semantic is already preferred", async () => {
    (api as any).getSettings = vi.fn().mockResolvedValue({
      bookmarked_dirs: [],
      recent_dirs: [],
      last_directory: "/startup",
      respect_gitignore: true,
      max_file_size: 1024,
      theme: "Dark",
      search_prefer_semantic: true,
      supported_extensions: ["ts"],
      max_results: 50,
      semantic: {
        enabled: true,
        selected: {
          engine: "SBERT",
          model: "intfloat/e5-small-v2",
          dimension: 384,
        },
        engine_devices: {},
        index_path: null,
        custom_models: [],
        chunk_size: 1000,
        chunk_overlap: 200,
        worker_timeout_secs: 300,
      },
    });
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/other",
      db_size_bytes: null,
    });

    await useSettingsStore.getState().load();
    await flushAsync();

    expect(api.buildIndex).toHaveBeenCalledWith(
      "/startup",
      expect.objectContaining({ model: "intfloat/e5-small-v2" }),
    );
  });

  it("rebuilds automatically when switching to a different root with semantic enabled", async () => {
    (api.getIndexStatus as any)
      .mockResolvedValueOnce({
        indexed_files: 5,
        total_chunks: 10,
        built_at: null,
        build_duration_ms: null,
        engine: "SBERT",
        model_id: "intfloat/e5-small-v2",
        dimension: 384,
        root_path: "/first",
        db_size_bytes: null,
      })
      .mockResolvedValueOnce({
        indexed_files: 5,
        total_chunks: 10,
        built_at: null,
        build_duration_ms: null,
        engine: "SBERT",
        model_id: "intfloat/e5-small-v2",
        dimension: 384,
        root_path: "/first",
        db_size_bytes: null,
      });

    useSettingsStore.setState({
      directory: "/first",
      preferSemantic: true,
    } as any);
    await flushAsync();

    useSettingsStore.setState({
      directory: "/second",
    } as any);
    await flushAsync();

    expect(api.buildIndex).toHaveBeenCalledTimes(1);
    expect(api.buildIndex).toHaveBeenCalledWith(
      "/second",
      expect.objectContaining({ model: "intfloat/e5-small-v2" }),
    );
  });

  it("deduplicates repeated ensure calls while the same root is already building", async () => {
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/old",
      db_size_bytes: null,
    });

    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);
    await flushAsync();

    await useSemanticStore.getState().ensureCurrentRootIndexed();

    expect(api.buildIndex).toHaveBeenCalledTimes(1);
    expect(useSemanticStore.getState().buildRoot).toBe("/project");
  });

  it("recovers after a build-start failure and retries on the next trigger", async () => {
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/old",
      db_size_bytes: null,
    });
    (api.buildIndex as any)
      .mockRejectedValueOnce(new Error("boom"))
      .mockResolvedValueOnce(undefined);

    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);
    await flushAsync();

    expect(useSemanticStore.getState().status).toBe("error");
    expect(useSemanticStore.getState().buildRoot).toBeNull();

    await useSemanticStore.getState().ensureCurrentRootIndexed();

    expect(api.buildIndex).toHaveBeenCalledTimes(2);
    expect(useSemanticStore.getState().status).toBe("building");
  });

  it("replays the last search after the current root becomes ready", async () => {
    const replaySearch = vi.fn().mockResolvedValue(undefined);
    useSearchStore.setState({ replaySearch } as any);
    useSettingsStore.setState({ directory: "/project" } as any);
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 5,
      total_chunks: 10,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/project",
      db_size_bytes: null,
    });

    await useSemanticStore.getState().handleIndexUpdated();

    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(true);
    expect(replaySearch).toHaveBeenCalled();
  });

  it("does not replay search for a stale completed index from a different root", async () => {
    const replaySearch = vi.fn().mockResolvedValue(undefined);
    useSearchStore.setState({ replaySearch } as any);
    useSettingsStore.setState({
      directory: "/new-root",
      preferSemantic: true,
    } as any);
    useSemanticStore.setState({
      buildRoot: "/old-root",
      status: "building",
    } as any);
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 5,
      total_chunks: 10,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/old-root",
      db_size_bytes: null,
    });

    await useSemanticStore.getState().handleIndexUpdated();

    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(false);
    expect(useSemanticStore.getState().buildRoot).toBe("/new-root");
    expect(replaySearch).not.toHaveBeenCalled();
  });

  it("clears stale semantic results when the current root index is removed", async () => {
    useSettingsStore.setState({ directory: "/project" } as any);
    useSearchStore.setState({
      lastQuery: { pattern: "hello", mode: "Semantic", root: "/project" } as any,
      results: [{ path: "/project/file.txt", file_type: "PlainText", matches: [] }],
      stats: { files_scanned: 1, total_matches: 1, elapsed_ms: 5, errors: [] },
      selectedMatch: { path: "/project/file.txt", origin: { TextFile: { line: 1, col: 1 } } } as any,
      previewData: { Text: { content: "hello", language: "txt", highlight_line: 1, highlight_range: { start: 0, end: 5 } } } as any,
    } as any);

    await useSemanticStore.getState().handleCurrentRootIndexRemoved();

    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(false);
    expect(useSemanticStore.getState().status).toBe("missing");
    expect(useSemanticStore.getState().blockedRoot).toBe("/project");
    expect(useSearchStore.getState().results).toEqual([]);
    expect(useSearchStore.getState().stats).toBeNull();
    expect(useSearchStore.getState().lastQuery).toEqual(
      expect.objectContaining({ pattern: "hello", mode: "Semantic", root: "/project" }),
    );
  });

  it("does not rebuild from stale pre-delete query state until a fresh attempt happens", async () => {
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/project",
      db_size_bytes: null,
    });

    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);
    await useSemanticStore.getState().handleCurrentRootIndexRemoved();
    (api.buildIndex as any).mockClear();

    await useSemanticStore.getState().ensureCurrentRootIndexed();
    expect(api.buildIndex).not.toHaveBeenCalled();

    await useSemanticStore.getState().ensureCurrentRootIndexed(true);
    expect(api.buildIndex).toHaveBeenCalledTimes(1);
    expect(useSemanticStore.getState().blockedRoot).toBeNull();
  });
});
