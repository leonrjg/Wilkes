import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import { useSearchStore } from "./useSearchStore";
import { useSemanticStore } from "./useSemanticStore";
import { useSettingsStore } from "./useSettingsStore";

vi.mock("../services", () => ({
  api: {
    getIndexStatus: vi.fn(),
    indexCoverage: vi.fn().mockResolvedValue([]),
    buildIndex: vi.fn().mockResolvedValue(undefined),
    listFiles: vi.fn().mockResolvedValue({ files: [], omitted: [] }),
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
        embed_batch_size: 16,
      },
      load: async () => {
        const settings = await (api as any).getSettings();
        useSettingsStore.setState({
          favorites: settings.favorites,
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
      readyGlobally: false,
      status: "idle",
      buildRoot: null,
      blockedRoot: null,
      error: null,
      coverage: {},
      coverageRoots: [],
    });
  });

  describe("refreshCoverage", () => {
    const usableStatus = {
      indexed_files: 5,
      total_chunks: 10,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: null,
      db_size_bytes: null,
    };

    it("keys each root's coverage by the root it was asked about", async () => {
      (api.getIndexStatus as any).mockResolvedValue(usableStatus);
      (api.indexCoverage as any).mockResolvedValue([
        { root: "/a", indexable: 10, covered: 10, complete: true },
        { root: "/b", indexable: 10, covered: 3, complete: false },
      ]);

      await useSemanticStore.getState().refreshCoverage(["/a", "/b"]);

      expect(api.indexCoverage).toHaveBeenCalledWith(["/a", "/b"]);
      expect(useSemanticStore.getState().coverage["/a"].complete).toBe(true);
      expect(useSemanticStore.getState().coverage["/b"].covered).toBe(3);
    });

    it("does not walk any directory when no index exists to cover them", async () => {
      (api.getIndexStatus as any).mockRejectedValue(new Error("No semantic index found"));

      await useSemanticStore.getState().refreshCoverage(["/a"]);

      expect(api.indexCoverage).not.toHaveBeenCalled();
      expect(useSemanticStore.getState().coverage).toEqual({});
    });

    it("re-asks about the remembered roots when called with none", async () => {
      (api.getIndexStatus as any).mockResolvedValue(usableStatus);
      (api.indexCoverage as any).mockResolvedValue([
        { root: "/a", indexable: 1, covered: 1, complete: true },
      ]);

      await useSemanticStore.getState().refreshCoverage(["/a"]);
      await useSemanticStore.getState().refreshCoverage();

      expect(api.indexCoverage).toHaveBeenNthCalledWith(2, ["/a"]);
    });

    it("leaves the roots unmarked rather than guessing when the backend fails", async () => {
      (api.getIndexStatus as any).mockResolvedValue(usableStatus);
      (api.indexCoverage as any).mockRejectedValue(new Error("journal unreadable"));
      const logged = vi.spyOn(console, "error").mockImplementation(() => {});

      await useSemanticStore.getState().refreshCoverage(["/a"]);

      expect(useSemanticStore.getState().coverage).toEqual({});
      expect(logged).toHaveBeenCalled();
      logged.mockRestore();
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
      favorites: [],
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
        embed_batch_size: 16,
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

  it("reads the new root's index state on a switch without starting a build", async () => {
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

    // The detection still runs — the interface needs it to mark the root —
    // but the hours of inference behind it are the user's to start.
    expect(api.getIndexStatus).toHaveBeenCalledWith("/second");
    expect(api.buildIndex).not.toHaveBeenCalled();
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

  it("clears a cancelled build marker so the same root can retry", async () => {
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
      preferSemantic: false,
    } as any);
    await flushAsync();
    useSemanticStore.setState({
      buildRoot: "/project",
      readyForCurrentRoot: false,
      status: "building",
    } as any);

    await useSemanticStore.getState().handleIndexTerminated();

    expect(useSemanticStore.getState().buildRoot).toBeNull();
    expect(useSemanticStore.getState().status).toBe("missing");

    (api.buildIndex as any).mockClear();
    useSettingsStore.setState({ preferSemantic: true } as any);
    await flushAsync();
    expect(api.buildIndex).toHaveBeenCalledWith(
      "/project",
      expect.objectContaining({ model: "intfloat/e5-small-v2" }),
    );
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
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "intfloat/e5-small-v2",
      dimension: 384,
      root_path: "/new-root",
      db_size_bytes: null,
    });

    await useSemanticStore.getState().handleIndexUpdated();

    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(false);
    expect(useSemanticStore.getState().buildRoot).toBe("/old-root");
    expect(replaySearch).not.toHaveBeenCalled();
  });

  it("clears current build root when the completed index is still unusable", async () => {
    const replaySearch = vi.fn().mockResolvedValue(undefined);
    useSearchStore.setState({ replaySearch } as any);
    useSettingsStore.setState({
      directory: "/project",
      preferSemantic: true,
    } as any);
    useSemanticStore.setState({
      buildRoot: "/project",
      status: "building",
    } as any);
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

    await useSemanticStore.getState().handleIndexUpdated();

    expect(useSemanticStore.getState().readyForCurrentRoot).toBe(false);
    expect(useSemanticStore.getState().buildRoot).toBeNull();
    expect(useSemanticStore.getState().status).toBe("missing");
    expect(replaySearch).not.toHaveBeenCalled();
  });

  it("clears stale semantic results when the current root index is removed", async () => {
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
    useSettingsStore.setState({ directory: "/project" } as any);
    useSearchStore.setState({
      lastQuery: { pattern: "hello", mode: "Semantic", root: "/project" } as any,
      results: [{ path: "/project/file.txt", file_type: "PlainText", matches: [] }],
      stats: { files_scanned: 1, total_matches: 1, elapsed_ms: 5, errors: [] },
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
