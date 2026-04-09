import { describe, it, expect, vi, beforeEach } from "vitest";
import { useSettingsStore } from "./useSettingsStore";
import { api } from "../services";

vi.mock("../services", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listFiles: vi.fn(),
    getIndexStatus: vi.fn(),
    buildIndex: vi.fn().mockResolvedValue(undefined),
  },
}));

describe("useSettingsStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({
      bookmarks: [],
      recentDirs: [],
      directory: "",
      respectGitignore: true,
      maxFileSize: 10 * 1024 * 1024,
      contextLines: 2,
      supportedExtensions: [],
      fileList: [],
      filterText: "",
      excluded: new Set(),
      semanticIndexBuilt: false,
      preferSemantic: false,
      indexing: false,
      theme: "System",
    });
  });

  it("should have initial state", () => {
    const state = useSettingsStore.getState();
    expect(state.directory).toBe("");
    expect(state.bookmarks).toEqual([]);
  });

  it("should load settings", async () => {
    const mockSettings = {
      bookmarked_dirs: ["/path/1"],
      recent_dirs: ["/path/recent"],
      last_directory: "/path/1",
      respect_gitignore: true,
      max_file_size: 100,
      theme: "Dark",
      search_prefer_semantic: true,
      semantic: { enabled: true, index_path: "/some/path" },
      supported_extensions: ["ts", "js"],
    };

    (api.getSettings as any).mockResolvedValue(mockSettings);
    (api.listFiles as any).mockResolvedValue([{ path: "/path/1/file.txt", size_bytes: 10, file_type: "PlainText", extension: "txt" }]);
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 1,
      total_chunks: 2,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "model",
      dimension: 384,
      root_path: "/path/1",
      db_size_bytes: null,
    });

    await useSettingsStore.getState().load();
    // Allow the directory-change subscription to resolve its async listFiles call
    await Promise.resolve();

    const state = useSettingsStore.getState();
    expect(state.bookmarks).toEqual(["/path/1"]);
    expect(state.directory).toBe("/path/1");
    expect(state.theme).toBe("Dark");
    expect(state.semanticIndexBuilt).toBe(true);
    expect(state.fileList.length).toBe(1);
  });

  it("should handle load with no directory", async () => {
    (api.getSettings as any).mockResolvedValue({
      bookmarked_dirs: [],
      recent_dirs: [],
      last_directory: null,
      respect_gitignore: true,
      max_file_size: 100,
      theme: "Light",
      search_prefer_semantic: false,
      semantic: { enabled: false, index_path: null },
      supported_extensions: [],
    });
    (api.getIndexStatus as any).mockRejectedValue(new Error("missing"));

    await useSettingsStore.getState().load();
    expect(useSettingsStore.getState().directory).toBe("");
    expect(useSettingsStore.getState().recentDirs).toEqual([]);
  });

  it("should update directory", async () => {
    (api.updateSettings as any).mockResolvedValue({});
    (api.listFiles as any).mockResolvedValue([]);

    useSettingsStore.getState().setDirectory("/new/path");

    const state = useSettingsStore.getState();
    expect(state.directory).toBe("/new/path");
    expect(state.recentDirs).toContain("/new/path");
    expect(api.updateSettings).toHaveBeenCalled();
  });

  it("should load file list reactively when directory changes", async () => {
    const mockFile = { path: "/dir/file.ts", size_bytes: 10, file_type: "PlainText", extension: "ts" };
    (api.updateSettings as any).mockResolvedValue({});
    (api.listFiles as any).mockResolvedValue([mockFile]);

    useSettingsStore.getState().setDirectory("/dir");
    await Promise.resolve();

    expect(api.listFiles).toHaveBeenCalledWith("/dir");
    expect(useSettingsStore.getState().fileList).toEqual([mockFile]);
  });

  it("should clear file list reactively when directory is removed", async () => {
    useSettingsStore.setState({ directory: "/some/dir", fileList: [{ path: "/some/dir/f.ts", size_bytes: 1, file_type: "PlainText", extension: "ts" }] });
    (api.updateSettings as any).mockResolvedValue({});

    useSettingsStore.getState().forgetDirectory("/some/dir");

    expect(useSettingsStore.getState().directory).toBe("");
    expect(useSettingsStore.getState().fileList).toEqual([]);
  });

  it("should add a bookmark", async () => {
    (api.updateSettings as any).mockResolvedValue({});

    useSettingsStore.getState().addBookmark("/bookmarked/path");

    const state = useSettingsStore.getState();
    expect(state.bookmarks).toContain("/bookmarked/path");
    expect(api.updateSettings).toHaveBeenCalled();
  });

  it("should not add duplicate bookmark", async () => {
    useSettingsStore.setState({ bookmarks: ["/path/1"] });
    useSettingsStore.getState().addBookmark("/path/1");
    expect(useSettingsStore.getState().bookmarks).toEqual(["/path/1"]);
    expect(api.updateSettings).not.toHaveBeenCalled();
  });

  it("should remove a bookmark", async () => {
    useSettingsStore.setState({ bookmarks: ["/path/1"] });
    (api.updateSettings as any).mockResolvedValue({});

    useSettingsStore.getState().removeBookmark("/path/1");

    const state = useSettingsStore.getState();
    expect(state.bookmarks).not.toContain("/path/1");
    expect(api.updateSettings).toHaveBeenCalled();
  });

  it("should forget a directory", async () => {
    useSettingsStore.setState({
      bookmarks: ["/path/1", "/path/2"],
      recentDirs: ["/path/1", "/path/3"],
      directory: "/path/1",
    });
    (api.updateSettings as any).mockResolvedValue({});

    useSettingsStore.getState().forgetDirectory("/path/1");

    const state = useSettingsStore.getState();
    expect(state.bookmarks).toEqual(["/path/2"]);
    expect(state.recentDirs).toEqual(["/path/3"]);
    expect(state.directory).toBe("");
    expect(api.updateSettings).toHaveBeenCalledWith({
      bookmarked_dirs: ["/path/2"],
      recent_dirs: ["/path/3"],
      last_directory: null,
    });
  });

  it("should apply settings patch", () => {
    useSettingsStore.getState().applySettingsPatch({ theme: "Light" });
    expect(useSettingsStore.getState().theme).toBe("Light");
  });

  it("should set prefer semantic", async () => {
    (api.updateSettings as any).mockResolvedValue({});
    useSettingsStore.getState().setPreferSemantic(true);
    expect(useSettingsStore.getState().preferSemantic).toBe(true);
    expect(api.updateSettings).toHaveBeenCalledWith({ search_prefer_semantic: true });
  });

  it("should refresh semantic ready", async () => {
    useSettingsStore.setState({ directory: "/path/1" });
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 1,
      total_chunks: 2,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "model",
      dimension: 384,
      root_path: "/path/1",
      db_size_bytes: null,
    });
    await useSettingsStore.getState().refreshSemanticReady();
    expect(useSettingsStore.getState().semanticIndexBuilt).toBe(true);
  });

  it("should treat an empty index as not ready", async () => {
    useSettingsStore.setState({ directory: "/path/1", semanticIndexBuilt: true });
    (api.getIndexStatus as any).mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      built_at: null,
      build_duration_ms: null,
      engine: "SBERT",
      model_id: "model",
      dimension: 384,
      root_path: "/path/1",
      db_size_bytes: null,
    });

    await useSettingsStore.getState().refreshSemanticReady();

    expect(useSettingsStore.getState().semanticIndexBuilt).toBe(false);
  });

  it("should update filter text", () => {
    useSettingsStore.getState().setFilterText("new filter");
    expect(useSettingsStore.getState().filterText).toBe("new filter");
  });

  it("should update excluded", () => {
    const excluded = new Set(["ts"]);
    useSettingsStore.getState().setExcluded(excluded);
    expect(useSettingsStore.getState().excluded).toBe(excluded);
  });

  it("should update indexing", () => {
    useSettingsStore.getState().setIndexing(true);
    expect(useSettingsStore.getState().indexing).toBe(true);
  });

  it("should apply settings patch for extensions", () => {
    useSettingsStore.getState().applySettingsPatch({ supported_extensions: ["rs"] });
    expect(useSettingsStore.getState().supportedExtensions).toEqual(["rs"]);
  });

  it("should apply settings patch for theme", () => {
    useSettingsStore.getState().applySettingsPatch({ theme: "Dark" });
    expect(useSettingsStore.getState().theme).toBe("Dark");
    expect(window.document.documentElement.classList.contains("dark")).toBe(true);

    useSettingsStore.getState().applySettingsPatch({ theme: "System" });
    expect(useSettingsStore.getState().theme).toBe("System");
  });

  it("should handle error in refreshSemanticReady", async () => {
    useSettingsStore.setState({ semanticIndexBuilt: true });
    (api.getIndexStatus as any).mockRejectedValue(new Error("Failed"));
    await useSettingsStore.getState().refreshSemanticReady();
    expect(useSettingsStore.getState().semanticIndexBuilt).toBe(false);
  });
});
