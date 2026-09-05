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

  it("reads generic startup status before the app runtime is used", async () => {
    (invoke as any).mockResolvedValue({ blockers: [] });
    await expect(api.getStartupStatus()).resolves.toEqual({ blockers: [] });
    expect(invoke).toHaveBeenCalledWith("get_startup_status");
  });

  it("bridges the standalone document window without workspace commands", async () => {
    (invoke as any).mockResolvedValueOnce({ theme: "Dark" });
    await api.getGlobalSettings();
    expect(invoke).toHaveBeenLastCalledWith("get_global_settings");

    const matchRef = {
      path: "/outside/paper.pdf",
      origin: { PdfPage: { page: 1, bbox: null } },
    } as const;
    (invoke as any).mockResolvedValueOnce({ Pdf: { page: 1, highlight_bbox: null } });
    await api.previewStandalone(matchRef);
    expect(invoke).toHaveBeenLastCalledWith("preview_standalone", { matchRef });

    (invoke as any).mockResolvedValueOnce({ title: "Outside" });
    await api.getStandaloneFileMetadata(matchRef.path);
    expect(invoke).toHaveBeenLastCalledWith("get_standalone_file_metadata", {
      path: matchRef.path,
    });

    (invoke as any).mockResolvedValueOnce([{ paths: [matchRef.path], errors: [] }]);
    await api.documentWindowReady();
    expect(invoke).toHaveBeenLastCalledWith("document_window_ready");

    const handler = vi.fn();
    (listen as any).mockResolvedValue(() => {});
    await api.onNativeOpen(handler);
    expect(listen).toHaveBeenCalledWith("native-open", expect.any(Function));
    const eventHandler = (listen as any).mock.calls.at(-1)[1];
    eventHandler({ payload: { paths: [matchRef.path], errors: [] } });
    expect(handler).toHaveBeenCalledWith({ paths: [matchRef.path], errors: [] });
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

  it("manages workspaces through dedicated commands", async () => {
    (invoke as any).mockResolvedValue({ active_workspace_id: "a", workspaces: [] });
    await api.listWorkspaces();
    expect(invoke).toHaveBeenLastCalledWith("list_workspaces");

    await api.createWorkspace("Second");
    expect(invoke).toHaveBeenLastCalledWith("create_workspace", { name: "Second" });

    await api.renameWorkspace("b", "Renamed");
    expect(invoke).toHaveBeenLastCalledWith("rename_workspace", {
      workspaceId: "b",
      name: "Renamed",
    });

    await api.switchWorkspace("b");
    expect(invoke).toHaveBeenLastCalledWith("switch_workspace", { workspaceId: "b" });
  });

  it("should configure the external MCP endpoint", async () => {
    (invoke as any).mockResolvedValue({ enabled: true, running: true, port: 39217 });
    await api.configureExternalMcp(true, false, "192.168.1.20", 39217);
    expect(invoke).toHaveBeenCalledWith("configure_external_mcp", {
      enabled: true,
      requireToken: false,
      bindAddress: "192.168.1.20",
      port: 39217,
    });
  });

  it("should get and rotate external MCP credentials", async () => {
    (invoke as any).mockResolvedValue({ enabled: true, running: true, port: 39217 });
    await api.getExternalMcpStatus();
    expect(invoke).toHaveBeenCalledWith("get_external_mcp_status");
    await api.rotateExternalMcpToken();
    expect(invoke).toHaveBeenCalledWith("rotate_external_mcp_token");
  });

  it("should update the active document exposed by external MCP", async () => {
    await api.setActiveDocument("/docs/paper.pdf", 3);
    expect(invoke).toHaveBeenCalledWith("set_active_document", {
      path: "/docs/paper.pdf",
      page: 3,
    });

    await api.setActiveDocument(null);
    expect(invoke).toHaveBeenLastCalledWith("set_active_document", {
      path: null,
      page: null,
    });
  });

  it("should call invoke for updateBookmarkNote", async () => {
    (invoke as any).mockResolvedValue({ id: "b1", note: "hi" });
    await api.updateBookmarkNote("b1", "hi");
    expect(invoke).toHaveBeenCalledWith("update_bookmark_note", { id: "b1", note: "hi" });
  });

  it("should call invoke for clusterBookmarks", async () => {
    (invoke as any).mockResolvedValue({ clusters: [], unclustered_bookmark_ids: [] });
    const query = { bookmark_ids: ["b1", "b2", "b3"], granularity: "more" as const };
    await api.clusterBookmarks(query);
    expect(invoke).toHaveBeenCalledWith("cluster_bookmarks", { query });
  });

  it("should call invoke for chunkTopics", async () => {
    (invoke as any).mockResolvedValue({ topics: [] });
    const query = { root: "/library", granularity: "much_fewer" as const };
    await api.chunkTopics("topics-1", query);
    expect(invoke).toHaveBeenCalledWith("chunk_topics", {
      requestId: "topics-1",
      query,
    });
  });

  it("should call invoke for cancelChunkTopics", async () => {
    await api.cancelChunkTopics("topics-1");
    expect(invoke).toHaveBeenCalledWith("cancel_chunk_topics", {
      requestId: "topics-1",
    });
  });

  it("should call invoke for listFiles", async () => {
    (invoke as any).mockResolvedValue({ files: [], omitted: [] });
    await api.listFiles("/some/root");
    expect(invoke).toHaveBeenCalledWith("list_files", { root: "/some/root" });
  });

  it("should call rename_file", async () => {
    (invoke as any).mockResolvedValue("/some/new.txt");
    const path = await api.renameFile("/some/old.txt", "new.txt");
    expect(invoke).toHaveBeenCalledWith("rename_file", {
      path: "/some/old.txt",
      newName: "new.txt",
    });
    expect(path).toBe("/some/new.txt");
  });

  it("should import filesystem paths", async () => {
    const source = new TauriSourceApi();
    (invoke as any).mockResolvedValue(["/root/file.pdf"]);
    const paths = await source.importFiles(["/external/file.pdf"], "/root", "copy");
    expect(invoke).toHaveBeenCalledWith("import_files", {
      paths: ["/external/file.pdf"],
      root: "/root",
      mode: "copy",
    });
    expect(paths).toEqual(["/root/file.pdf"]);
  });

  it("should read copied filesystem paths", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(["/external/file.pdf"]);
    const source = new TauriSourceApi();

    await expect(source.readClipboardFiles()).resolves.toEqual(["/external/file.pdf"]);
    expect(invoke).toHaveBeenCalledWith("read_clipboard_files");
  });

  it("should call trash_file through the shared deleteFile abstraction", async () => {
    const source = new TauriSourceApi();
    (invoke as any).mockResolvedValue(undefined);
    await source.deleteFile("/root/file.pdf");
    expect(source.deletionKind).toBe("trash");
    expect(invoke).toHaveBeenCalledWith("trash_file", { path: "/root/file.pdf" });
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
    const url = api.resolveAssetUrl("/path/to/file.pdf");
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

  it("should call index_activity, continue_index_job and retry_failed_documents", async () => {
    const selected = { model: "model", engine: "SBERT", dimension: 384 } as any;
    (invoke as any).mockResolvedValue({ root: "/root", job: null, documents: [], history: [] });
    await api.indexActivity("/root");
    expect(invoke).toHaveBeenCalledWith("index_activity", { root: "/root" });

    await api.continueIndexJob("/root", selected);
    expect(invoke).toHaveBeenCalledWith("continue_index_job", { root: "/root", selected });

    // Two commands, because continuing and retrying are two decisions.
    await api.retryFailedDocuments("/root", selected);
    expect(invoke).toHaveBeenCalledWith("retry_failed_documents", { root: "/root", selected });
  });

  it("should call get_index_status", async () => {
    (invoke as any).mockResolvedValue({ engine: "SBERT" });
    const status = await api.getIndexStatus();
    expect(invoke).toHaveBeenCalledWith("get_index_status", { root: null });
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
    expect(invoke).toHaveBeenCalledWith("delete_index", { root: null });
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

  it("should call get_file_metadata", async () => {
    (invoke as any).mockResolvedValue({ title: "Test Title", author: "Test Author", doi: null, created_at: "2025-04" });
    const result = await api.getFileMetadata("/path/to/file.pdf");
    expect(invoke).toHaveBeenCalledWith("get_file_metadata", { path: "/path/to/file.pdf" });
    expect(result).toEqual({ title: "Test Title", author: "Test Author", doi: null, created_at: "2025-04" });
  });

  it("should call refresh_file_metadata without a path", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.refreshFileMetadata();
    expect(invoke).toHaveBeenCalledWith("refresh_file_metadata", { path: undefined });
  });

  it("should call refresh_file_metadata with a path", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.refreshFileMetadata("/path/to/file.pdf");
    expect(invoke).toHaveBeenCalledWith("refresh_file_metadata", { path: "/path/to/file.pdf" });
  });

  it("should call get_data_paths", async () => {
    (invoke as any).mockResolvedValue({ app_data: "/app", workspace: "/app/workspaces/w1" });
    const result = await api.getDataPaths();
    expect(invoke).toHaveBeenCalledWith("get_data_paths");
    expect(result).toEqual({ app_data: "/app", workspace: "/app/workspaces/w1" });
  });

  it("should call open_path", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.openPath("/some/path");
    expect(invoke).toHaveBeenCalledWith("open_path", { path: "/some/path" });
  });

  it("should call reveal_path", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.revealPath("/some/path");
    expect(invoke).toHaveBeenCalledWith("reveal_path", { path: "/some/path" });
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

  it("starts generation tasks and subscribes to their shared stream", async () => {
    (invoke as any).mockResolvedValue(undefined);
    await api.explainRelatedDocument("relation-1", "/docs/a.pdf", "/docs/b.pdf");
    expect(invoke).toHaveBeenLastCalledWith("explain_related_document", {
      requestId: "relation-1",
      anchorPath: "/docs/a.pdf",
      path: "/docs/b.pdf",
    });
    await api.summarizeDocument("summary-1", "/docs/a.pdf");
    expect(invoke).toHaveBeenLastCalledWith("summarize_document", {
      requestId: "summary-1",
      path: "/docs/a.pdf",
    });
    const resultsInput = {
      query: "cache",
      sources: [{ title: "a.pdf", path: "/docs/a.pdf" }],
      passages: [{ text: "Caching reduces repeated work.", source_index: 0 }],
    };
    await api.summarizeSearchResults("results-1", resultsInput);
    expect(invoke).toHaveBeenLastCalledWith("summarize_search_results", {
      requestId: "results-1",
      input: resultsInput,
    });

    const handler = vi.fn();
    (listen as any).mockResolvedValue(vi.fn());
    await api.onGenerationStream(handler);
    expect(listen).toHaveBeenLastCalledWith(
      "generation-stream",
      expect.any(Function),
    );
    const listener = (listen as any).mock.calls.at(-1)[1];
    const event = {
      phase: "completed",
      request_id: "summary-1",
      task: "document_summary",
      text: "Done",
    };
    listener({ payload: event });
    expect(handler).toHaveBeenCalledWith(event);
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
