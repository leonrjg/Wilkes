import { describe, it, expect, vi, beforeEach } from "vitest";
import { TauriSearchApi, TauriSourceApi } from "./tauri";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  convertFileSrc: vi.fn((path) => `asset://${path}`),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

describe("TauriSearchApi", () => {
  let api: TauriSearchApi;

  beforeEach(() => {
    vi.clearAllMocks();
    api = new TauriSearchApi();
  });

  it("should call invoke for getSettings", async () => {
    (invoke as any).mockResolvedValue({ theme: "Dark" });
    const settings = await api.getSettings();
    expect(invoke).toHaveBeenCalledWith("get_settings");
    expect(settings).toEqual({ theme: "Dark" });
  });

  it("should call invoke for updateSettings", async () => {
    const patch = { theme: "Light" as const };
    (invoke as any).mockResolvedValue({ theme: "Light" });
    await api.updateSettings(patch);
    expect(invoke).toHaveBeenCalledWith("update_settings", { patch });
  });

  it("should call invoke for listFiles", async () => {
    (invoke as any).mockResolvedValue({ files: [], omitted: [] });
    await api.listFiles("/some/root");
    expect(invoke).toHaveBeenCalledWith("list_files", { root: "/some/root" });
  });

  it("should perform a search with listeners", async () => {
    const mockQuery = { pattern: "test" } as any;
    const onResult = vi.fn();
    const onComplete = vi.fn();

    (listen as any).mockResolvedValue(vi.fn()); // mock unlisten function
    (invoke as any).mockResolvedValue(undefined);

    const searchId = await api.search(mockQuery, onResult, onComplete);

    expect(searchId).toBeDefined();
    expect(listen).toHaveBeenCalledWith(`search-result-${searchId}`, expect.any(Function));
    expect(listen).toHaveBeenCalledWith(`search-complete-${searchId}`, expect.any(Function));
    expect(invoke).toHaveBeenCalledWith("search", { query: mockQuery, searchId });
  });

  it("should resolve pdf url", () => {
    const url = api.resolvePdfUrl("/path/to/file.pdf");
    expect(url).toContain("/path/to/file.pdf");
  });

  it("should call get_logs", async () => {
    (invoke as any).mockResolvedValue(["log1"]);
    const logs = await api.getLogs();
    expect(invoke).toHaveBeenCalledWith("get_logs");
    expect(logs).toEqual(["log1"]);
  });

  it("should call clear_logs", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.clearLogs();
    expect(invoke).toHaveBeenCalledWith("clear_logs");
  });

  it("should call build_index", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.buildIndex("/root", { model: "model", engine: "SBERT", dimension: 384 });
    expect(invoke).toHaveBeenCalledWith("build_index", {
      root: "/root",
      selected: { model: "model", engine: "SBERT", dimension: 384 },
    });
  });

  it("should call get_index_status", async () => {
    (invoke as any).mockResolvedValue({ engine: "SBERT" });
    const status = await api.getIndexStatus();
    expect(invoke).toHaveBeenCalledWith("get_index_status");
    expect(status).toEqual({ engine: "SBERT" });
  });

  it("should call download_model", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.downloadModel({ model: "model", engine: "SBERT", dimension: 384 });
    expect(invoke).toHaveBeenCalledWith("download_model", {
      selected: { model: "model", engine: "SBERT", dimension: 384 },
    });
  });

  it("should call delete_index", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.deleteIndex();
    expect(invoke).toHaveBeenCalledWith("delete_index");
  });

  it("should call get_worker_status", async () => {
    (invoke as any).mockResolvedValue({ active: true });
    const status = await api.getWorkerStatus();
    expect(invoke).toHaveBeenCalledWith("get_worker_status");
    expect(status).toEqual({ active: true });
  });

  it("should call kill_worker", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.killWorker();
    expect(invoke).toHaveBeenCalledWith("kill_worker");
  });

  it("should call set_worker_timeout", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.setWorkerTimeout(100);
    expect(invoke).toHaveBeenCalledWith("set_worker_timeout", { secs: 100 });
  });

  it("should call open_file", async () => {
    (invoke as any).mockResolvedValue({ Text: { content: "test" } });
    const result = await api.openFile("/path/to/file");
    expect(invoke).toHaveBeenCalledWith("open_file", { path: "/path/to/file" });
    expect(result).toEqual({ Text: { content: "test" } });
  });

  it("should call get_data_paths", async () => {
    (invoke as any).mockResolvedValue({ app_data: "/app" });
    const result = await api.getDataPaths();
    expect(invoke).toHaveBeenCalledWith("get_data_paths");
    expect(result).toEqual({ app_data: "/app" });
  });

  it("should call open_path", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.openPath("/some/path");
    expect(invoke).toHaveBeenCalledWith("open_path", { path: "/some/path" });
  });

  it("should call get_python_info", async () => {
    (invoke as any).mockResolvedValue("/usr/bin/python");
    const result = await api.getPythonInfo();
    expect(invoke).toHaveBeenCalledWith("get_python_info");
    expect(result).toBe("/usr/bin/python");
  });

  it("should call cancel_search", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.cancelSearch("id");
    expect(invoke).toHaveBeenCalledWith("cancel_search", { searchId: "id" });
  });

  it("should call cancel_embed", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.cancelEmbed();
    expect(invoke).toHaveBeenCalledWith("cancel_embed");
  });

  it("should subscribe to embed progress", async () => {
    const handler = vi.fn();
    const unlisten = vi.fn();
    (listen as any).mockResolvedValue(unlisten);

    const result = await api.onEmbedProgress(handler);
    expect(listen).toHaveBeenCalledWith("embed-progress", expect.any(Function));
    
    // Call the handler
    const eventHandler = (listen as any).mock.calls.find((call: any) => call[0] === "embed-progress")[1];
    eventHandler({ payload: { Build: { files_processed: 5, total_files: 10 } } });
    expect(handler).toHaveBeenCalledWith({ Build: { files_processed: 5, total_files: 10 } });

    result(); // call unlisten
    expect(unlisten).toHaveBeenCalled();
  });

  it("should subscribe to embed done", async () => {
    const handler = vi.fn();
    (listen as any).mockResolvedValue(vi.fn());
    await api.onEmbedDone(handler);
    expect(listen).toHaveBeenCalledWith("embed-done", expect.any(Function));
  });

  it("should subscribe to embed error", async () => {
    const handler = vi.fn();
    (listen as any).mockResolvedValue(vi.fn());
    await api.onEmbedError(handler);
    expect(listen).toHaveBeenCalledWith("embed-error", expect.any(Function));
  });
});

describe("TauriSourceApi", () => {
  let source: TauriSourceApi;

  beforeEach(() => {
    vi.clearAllMocks();
    source = new TauriSourceApi();
  });

  it("should call pick_directory", async () => {
    (invoke as any).mockResolvedValue("/picked/path");
    const result = await source.pickDirectory();
    expect(invoke).toHaveBeenCalledWith("pick_directory");
    expect(result).toBe("/picked/path");
  });
});
