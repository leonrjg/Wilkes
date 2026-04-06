import { describe, it, expect, vi, beforeEach } from "vitest";
import { useSettingsStore } from "./useSettingsStore";
import { api } from "../services";

vi.mock("../services", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listFiles: vi.fn(),
    isSemanticReady: vi.fn().mockResolvedValue(true),
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

    await useSettingsStore.getState().load();

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
    (api.getSettings as any).mockResolvedValue({
      semantic: { enabled: true, index_path: "/some/path" }
    });
    await useSettingsStore.getState().refreshSemanticReady();
    expect(useSettingsStore.getState().semanticIndexBuilt).toBe(true);
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
    (api.getSettings as any).mockRejectedValue(new Error("Failed"));
    await useSettingsStore.getState().refreshSemanticReady();
    expect(useSettingsStore.getState().semanticIndexBuilt).toBe(true); // Should not change on error if it doesn't set it to false
    // Actually the code doesn't set it to false on error, it just logs it.
  });
});
