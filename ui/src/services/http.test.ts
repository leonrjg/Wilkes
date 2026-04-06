import { describe, it, expect, vi, beforeEach } from "vitest";
import { HttpSearchApi, HttpSourceApi } from "./http";

describe("HttpSearchApi", () => {
  let api: HttpSearchApi;

  beforeEach(() => {
    vi.clearAllMocks();
    api = new HttpSearchApi();
    global.fetch = vi.fn() as any;
    vi.stubGlobal("crypto", {
      randomUUID: () => "mock-uuid",
    });
  });

  it("should perform fetch for getSettings", async () => {
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ theme: "Dark" }),
    });

    const settings = await api.getSettings();
    expect(global.fetch).toHaveBeenCalledWith("/api/settings");
    expect(settings).toEqual({ theme: "Dark" });
  });

  it("should perform fetch for updateSettings", async () => {
    const patch = { theme: "Light" as const };
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ theme: "Light" }),
    });

    await api.updateSettings(patch);
    expect(global.fetch).toHaveBeenCalledWith("/api/settings", {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    });
  });

  it("should perform fetch for listFiles", async () => {
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve([]),
    });

    await api.listFiles("/some/root");
    expect(global.fetch).toHaveBeenCalledWith("/api/files?root=%2Fsome%2Froot");
  });

  it("should handle fetch error in getSettings", async () => {
    (global.fetch as any).mockResolvedValue({ ok: false, status: 500 });
    await expect(api.getSettings()).rejects.toThrow("getSettings failed: 500");
  });

  it("should perform a streaming search", async () => {
    const mockQuery = { pattern: "test" } as any;
    const onResult = vi.fn();
    const onComplete = vi.fn();

    const result1 = { path: "file1.txt", matches: [] };
    const stats = { files_scanned: 1, total_matches: 0, elapsed_ms: 10, errors: [] };

    const encoder = new TextEncoder();
    const chunks = [
      encoder.encode(`event: result\ndata: ${JSON.stringify(result1)}\n\n`),
      encoder.encode(`event: complete\ndata: ${JSON.stringify(stats)}\n\n`),
    ];

    let chunkIndex = 0;
    const mockReader = {
      read: vi.fn().mockImplementation(() => {
        if (chunkIndex < chunks.length) {
          return Promise.resolve({ value: chunks[chunkIndex++], done: false });
        }
        return Promise.resolve({ value: undefined, done: true });
      }),
    };

    (global.fetch as any).mockResolvedValue({
      ok: true,
      body: {
        getReader: () => mockReader,
      },
    });

    await api.search(mockQuery, onResult, onComplete);
    await new Promise(resolve => setTimeout(resolve, 10));

    expect(onResult).toHaveBeenCalledWith(result1);
    expect(onComplete).toHaveBeenCalledWith(stats);
  });

  it("should perform preview", async () => {
    const mockMatch = { path: "test.txt", origin: { TextFile: { line: 1, col: 1 } } } as any;
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ Text: { content: "test" } }),
    });

    const result = await api.preview(mockMatch);
    expect(global.fetch).toHaveBeenCalledWith("/api/preview", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(mockMatch),
    });
    expect(result).toEqual({ Text: { content: "test" } });
  });

  it("should perform buildIndex", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true });
    await api.buildIndex("/root", "model", "SBERT");
    expect(global.fetch).toHaveBeenCalledWith("/api/index/build", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ root: "/root", model_id: "model", engine: "SBERT" }),
    });
  });

  it("should perform downloadModel", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true });
    await api.downloadModel("model", "SBERT");
    expect(global.fetch).toHaveBeenCalledWith("/api/index/download", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model_id: "model", engine: "SBERT" }),
    });
  });

  it("should call getLogs", async () => {
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(["log1"]),
    });
    const logs = await api.getLogs();
    expect(global.fetch).toHaveBeenCalledWith("/api/logs");
    expect(logs).toEqual(["log1"]);
  });

  it("should call clearLogs", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.clearLogs();
    expect(global.fetch).toHaveBeenCalledWith("/api/logs", { method: "DELETE" });
  });

  it("should perform openFile", async () => {
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ Text: { content: "test" } }),
    });
    const result = await api.openFile("/path/to/file");
    expect(global.fetch).toHaveBeenCalledWith("/api/file", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: "/path/to/file" }),
    });
    expect(result).toEqual({ Text: { content: "test" } });
  });

  it("should resolvePdfUrl", () => {
    const url = api.resolvePdfUrl("/path/to/file.pdf");
    expect(url).toBe("/asset?path=%2Fpath%2Fto%2Ffile.pdf");
  });

  it("should perform cancelEmbed", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true });
    await api.cancelEmbed();
    expect(global.fetch).toHaveBeenCalledWith("/api/index/cancel", { method: "POST" });
  });

  it("should subscribe to embed progress via EventSource", async () => {
    const handler = vi.fn();
    const mockEventSource = {
      close: vi.fn(),
      onmessage: null as any,
    };
    function MockEventSource() { return mockEventSource; }
    vi.stubGlobal("EventSource", MockEventSource);

    const unlisten = await api.onEmbedProgress(handler);
    mockEventSource.onmessage({ data: JSON.stringify({ Build: { files_processed: 1 } }) });
    expect(handler).toHaveBeenCalledWith({ Build: { files_processed: 1 } });

    unlisten();
    expect(mockEventSource.close).toHaveBeenCalled();
  });

  it("should subscribe to embed done", async () => {
    const handler = vi.fn();
    const mockEventSource = { close: vi.fn(), onmessage: null as any };
    vi.stubGlobal("EventSource", function() { return mockEventSource; });
    await api.onEmbedDone(handler);
    mockEventSource.onmessage({ data: JSON.stringify({ operation: "Build" }) });
    expect(handler).toHaveBeenCalledWith({ operation: "Build" });
  });

  it("should subscribe to embed error", async () => {
    const handler = vi.fn();
    const mockEventSource = { close: vi.fn(), onmessage: null as any };
    vi.stubGlobal("EventSource", function() { return mockEventSource; });
    await api.onEmbedError(handler);
    mockEventSource.onmessage({ data: JSON.stringify({ operation: "Build", message: "err" }) });
    expect(handler).toHaveBeenCalledWith({ operation: "Build", message: "err" });
  });
});

describe("HttpSourceApi", () => {
  let source: HttpSourceApi;

  beforeEach(() => {
    vi.clearAllMocks();
    source = new HttpSourceApi();
    global.fetch = vi.fn() as any;
  });

  it("should upload files", async () => {
    (global.fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ root: "/uploaded/root" }),
    });

    const file = new File(["test"], "test.txt");
    const root = await source.uploadFiles([file]);

    expect(global.fetch).toHaveBeenCalledWith("/api/upload", expect.objectContaining({
      method: "POST",
      body: expect.any(FormData),
    }));
    expect(root).toBe("/uploaded/root");
  });

  it("should delete a file", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await source.deleteFile("/path/to/file");
    expect(global.fetch).toHaveBeenCalledWith("/api/upload?path=%2Fpath%2Fto%2Ffile", { method: "DELETE" });
  });

  it("should delete all files", async () => {
    (global.fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await source.deleteAll();
    expect(global.fetch).toHaveBeenCalledWith("/api/upload/all", { method: "DELETE" });
  });
});
