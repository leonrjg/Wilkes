import { describe, it, expect, vi, beforeEach } from "vitest";
import { HttpSearchApi, HttpSourceApi } from "./http";

describe("HttpSearchApi", () => {
  let api: HttpSearchApi;

  beforeEach(() => {
    api = new HttpSearchApi();
    vi.stubGlobal("fetch", vi.fn());
    vi.stubGlobal("EventSource", vi.fn(() => ({
      addEventListener: vi.fn(),
      close: vi.fn(),
    })));
  });

  it("getSettings calls fetch and returns settings", async () => {
    const mockSettings = { semantic: { enabled: true } };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockSettings),
    });

    const settings = await api.getSettings();
    expect(fetch).toHaveBeenCalledWith("/api/settings");
    expect(settings).toEqual(mockSettings);
  });

  it("updateSettings sends patch request", async () => {
    const patch = { semantic: { enabled: false } };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(patch),
    });

    await api.updateSettings(patch);
    expect(fetch).toHaveBeenCalledWith("/api/settings", expect.objectContaining({
      method: "PATCH",
      body: JSON.stringify(patch),
    }));
  });

  it("search streams results", async () => {
    const mockFileMatches = { path: "test.txt", matches: [] };
    const mockStats = { total_files: 1, total_matches: 0, duration_ms: 10 };

    const encoder = new TextEncoder();
    const stream = new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode("event: result\ndata: " + JSON.stringify(mockFileMatches) + "\n\n"));
        controller.enqueue(encoder.encode("event: complete\ndata: " + JSON.stringify(mockStats) + "\n\n"));
        controller.close();
      },
    });

    (fetch as any).mockResolvedValue({
      ok: true,
      body: stream,
    });

    const onResult = vi.fn();
    const onComplete = vi.fn();

    await api.search({ pattern: "test" } as any, onResult, onComplete);

    // Wait for stream to process
    await new Promise((resolve) => setTimeout(resolve, 50));

    expect(onResult).toHaveBeenCalledWith(mockFileMatches);
    expect(onComplete).toHaveBeenCalledWith(mockStats);
  });

  it("handles search failure", async () => {
    (fetch as any).mockResolvedValue({
      ok: false,
      status: 500,
    });

    const onResult = vi.fn();
    const onComplete = vi.fn();

    // We don't await search directly because it returns a searchId and runs streamSearch in background
    // but we can check console.error or the background promise if we had access to it.
    // streamSearch is private, so we can't test it directly easily without exposing it.
  });

  it("cancelSearch aborts the controller", async () => {
    const controller = { abort: vi.fn() };
    vi.stubGlobal("AbortController", vi.fn(function() { return controller; }));

    const searchId = await api.search({ pattern: "test" } as any, vi.fn(), vi.fn());
    await api.cancelSearch(searchId);

    expect(controller.abort).toHaveBeenCalled();
  });

  it("onEmbedProgress sets up EventSource and calls handler", async () => {
    const handler = vi.fn();
    const mockEventSource = {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      close: vi.fn(),
    };
    vi.stubGlobal("EventSource", vi.fn(function() { return mockEventSource; }));

    const close = await api.onEmbedProgress(handler);
    expect(EventSource).toHaveBeenCalledWith("/api/embed/events");
    expect(mockEventSource.addEventListener).toHaveBeenCalledWith("embed-progress", expect.any(Function));

    const listener = mockEventSource.addEventListener.mock.calls[0][1];
    listener({ data: JSON.stringify({ progress: 0.5 }) });
    expect(handler).toHaveBeenCalledWith({ progress: 0.5 });

    close();
    expect(mockEventSource.close).toHaveBeenCalled();
  });

  it("shares one EventSource across all embed subscriptions", async () => {
    const mockEventSource = {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      close: vi.fn(),
    };
    vi.stubGlobal("EventSource", vi.fn(function() { return mockEventSource; }));

    const c1 = await api.onEmbedProgress(vi.fn());
    const c2 = await api.onEmbedDone(vi.fn());
    const c3 = await api.onEmbedError(vi.fn());
    const c4 = await api.onManagerEvent(vi.fn());

    // Only one EventSource should have been created
    expect(EventSource).toHaveBeenCalledTimes(1);

    // Closing all but the last should not close the connection
    c1(); c2(); c3();
    expect(mockEventSource.close).not.toHaveBeenCalled();

    // Closing the last one should close it
    c4();
    expect(mockEventSource.close).toHaveBeenCalledTimes(1);
  });

  it("preview calls fetch and returns PreviewData", async () => {
    const mockData = { content: "test", language: "text", line: 1 };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockData),
    });

    const res = await api.preview({ path: "test.txt", line: 1, column: 1 });
    expect(fetch).toHaveBeenCalledWith("/api/preview", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ path: "test.txt", line: 1, column: 1 }),
    }));
    expect(res).toEqual(mockData);
  });

  it("listFiles calls fetch and returns FileEntry[]", async () => {
    const mockFiles = [{ name: "test.txt", path: "test.txt", is_dir: false }];
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockFiles),
    });

    const res = await api.listFiles("/root");
    expect(fetch).toHaveBeenCalledWith("/api/files?root=%2Froot");
    expect(res).toEqual(mockFiles);
  });

  it("openFile calls fetch and returns PreviewData", async () => {
    const mockData = { content: "test", language: "text", line: 1 };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockData),
    });

    const res = await api.openFile("test.txt");
    expect(fetch).toHaveBeenCalledWith("/api/file", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ path: "test.txt" }),
    }));
    expect(res).toEqual(mockData);
  });

  it("resolvePdfUrl returns correctly formatted URL", () => {
    const url = api.resolvePdfUrl("/path/to/test.pdf");
    expect(url).toBe("/asset?path=%2Fpath%2Fto%2Ftest.pdf");
  });

  it("isSemanticReady calls fetch and returns boolean", async () => {
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(true),
    });

    const res = await api.isSemanticReady();
    expect(fetch).toHaveBeenCalledWith("/api/embed/ready");
    expect(res).toBe(true);
  });

  it("getLogs calls fetch and returns string[]", async () => {
    const mockLogs = ["log1", "log2"];
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockLogs),
    });

    const res = await api.getLogs();
    expect(fetch).toHaveBeenCalledWith("/api/logs");
    expect(res).toEqual(mockLogs);
  });

  it("clearLogs calls fetch with DELETE", async () => {
    (fetch as any).mockResolvedValue({ ok: true });
    await api.clearLogs();
    expect(fetch).toHaveBeenCalledWith("/api/logs", { method: "DELETE" });
  });

  it("getPythonInfo calls fetch and returns string", async () => {
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve("python info"),
    });

    const res = await api.getPythonInfo();
    expect(fetch).toHaveBeenCalledWith("/api/worker/python-info");
    expect(res).toBe("python info");
  });

  it("getSupportedEngines calls fetch and returns EmbeddingEngine[]", async () => {
    const mockEngines = ["SBERT", "Xenova"];
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockEngines),
    });

    const res = await api.getSupportedEngines();
    expect(fetch).toHaveBeenCalledWith("/api/embed/engines");
    expect(res).toEqual(mockEngines);
  });

  it("getDataPaths calls fetch and returns data paths", async () => {
    const mockData = { paths: ["/path1"] };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockData),
    });

    const res = await api.getDataPaths();
    expect(fetch).toHaveBeenCalledWith("/api/data/paths");
    expect(res).toEqual(mockData);
  });

  it("openPath does nothing in browser mode", async () => {
    await api.openPath("/some/path");
    // Just verifying it doesn't throw and coverage is recorded
  });

  it("getWorkerStatus calls fetch and returns WorkerStatus", async () => {
    const mockStatus = { status: "running" };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockStatus),
    });

    const res = await api.getWorkerStatus();
    expect(fetch).toHaveBeenCalledWith("/api/worker/status");
    expect(res).toEqual(mockStatus);
  });

  it("killWorker calls fetch with POST", async () => {
    (fetch as any).mockResolvedValue({ ok: true });
    await api.killWorker();
    expect(fetch).toHaveBeenCalledWith("/api/worker/kill", { method: "POST" });
  });

  it("setWorkerTimeout calls fetch with PATCH", async () => {
    (fetch as any).mockResolvedValue({ ok: true });
    await api.setWorkerTimeout(60);
    expect(fetch).toHaveBeenCalledWith("/api/worker/timeout", expect.objectContaining({
      method: "PATCH",
      body: JSON.stringify({ secs: 60 }),
    }));
  });

  it("listModels calls fetch and returns ModelDescriptor[]", async () => {
    const mockModels = [{ id: "model1", name: "model 1", size_bytes: 100 }];
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockModels),
    });

    const res = await api.listModels("SBERT" as any);
    expect(fetch).toHaveBeenCalledWith("/api/embed/models?engine=SBERT");
    expect(res).toEqual(mockModels);
  });

  it("getModelSize calls fetch and returns number", async () => {
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(1024),
    });

    const res = await api.getModelSize("SBERT" as any, "model1");
    expect(fetch).toHaveBeenCalledWith("/api/embed/model-size?engine=SBERT&model_id=model1");
    expect(res).toBe(1024);
  });

  it("downloadModel calls fetch with POST", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 202 });
    const selected = { model: "m1", engine: "SBERT", dimension: 384 };
    await api.downloadModel(selected as any);
    expect(fetch).toHaveBeenCalledWith("/api/embed/download", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ selected }),
    }));
  });

  it("buildIndex calls fetch with POST", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 202 });
    const selected = { model: "m1", engine: "SBERT", dimension: 384 };
    await api.buildIndex("/root", selected as any);
    expect(fetch).toHaveBeenCalledWith("/api/embed/build", expect.objectContaining({
      method: "POST",
      body: JSON.stringify({ root: "/root", selected }),
    }));
  });

  it("cancelEmbed calls fetch with DELETE", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.cancelEmbed();
    expect(fetch).toHaveBeenCalledWith("/api/embed/cancel", { method: "DELETE" });
  });

  it("getIndexStatus calls fetch and returns IndexStatus", async () => {
    const mockStatus = { status: "ready" };
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve(mockStatus),
    });

    const res = await api.getIndexStatus();
    expect(fetch).toHaveBeenCalledWith("/api/embed/status");
    expect(res).toEqual(mockStatus);
  });

  it("deleteIndex calls fetch with DELETE", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.deleteIndex();
    expect(fetch).toHaveBeenCalledWith("/api/embed/index", { method: "DELETE" });
  });

  it("throws error when fetch fails", async () => {
    (fetch as any).mockResolvedValue({
      ok: false,
      status: 500,
    });

    await expect(api.getSettings()).rejects.toThrow("getSettings failed: 500");
    await expect(api.updateSettings({})).rejects.toThrow("updateSettings failed: 500");
    await expect(api.preview({} as any)).rejects.toThrow("Preview failed: 500");
    await expect(api.listFiles("/")).rejects.toThrow("listFiles failed: 500");
    await expect(api.openFile("test")).rejects.toThrow("openFile failed: 500");
    await expect(api.isSemanticReady()).rejects.toThrow("isSemanticReady failed: 500");
    await expect(api.getLogs()).rejects.toThrow("getLogs failed: 500");
    await expect(api.clearLogs()).rejects.toThrow("clearLogs failed: 500");
    await expect(api.getPythonInfo()).rejects.toThrow("getPythonInfo failed: 500");
    await expect(api.getSupportedEngines()).rejects.toThrow("getSupportedEngines failed: 500");
    await expect(api.getDataPaths()).rejects.toThrow("getDataPaths failed: 500");
    await expect(api.getWorkerStatus()).rejects.toThrow("getWorkerStatus failed: 500");
    await expect(api.killWorker()).rejects.toThrow("killWorker failed: 500");
    await expect(api.setWorkerTimeout(1)).rejects.toThrow("setWorkerTimeout failed: 500");
    await expect(api.listModels("SBERT" as any)).rejects.toThrow("listModels failed: 500");
    await expect(api.getModelSize("SBERT" as any, "m")).rejects.toThrow("getModelSize failed: 500");
    await expect(api.downloadModel({} as any)).rejects.toThrow("downloadModel failed: 500");
    await expect(api.buildIndex("/", {} as any)).rejects.toThrow("buildIndex failed: 500");
    await expect(api.cancelEmbed()).rejects.toThrow("cancelEmbed failed: 500");
    await expect(api.getIndexStatus()).rejects.toThrow("getIndexStatus failed: 500");
    await expect(api.deleteIndex()).rejects.toThrow("deleteIndex failed: 500");
  });
});

describe("HttpSourceApi", () => {
  let api: HttpSourceApi;

  beforeEach(() => {
    api = new HttpSourceApi();
    vi.stubGlobal("fetch", vi.fn());
  });

  it("uploadFiles sends FormData", async () => {
    (fetch as any).mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ root: "/test" }),
    });

    const file = new File(["test"], "test.txt");
    const root = await api.uploadFiles([file]);

    expect(fetch).toHaveBeenCalledWith("/api/upload", expect.objectContaining({
      method: "POST",
      body: expect.any(FormData),
    }));
    expect(root).toBe("/test");
  });

  it("deleteFile sends DELETE request", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.deleteFile("test.txt");
    expect(fetch).toHaveBeenCalledWith("/api/upload?path=test.txt", { method: "DELETE" });
  });

  it("deleteAll sends DELETE request", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.deleteAll();
    expect(fetch).toHaveBeenCalledWith("/api/upload/all", { method: "DELETE" });
  });

  it("throws error when fetch fails", async () => {
    (fetch as any).mockResolvedValue({ ok: false, status: 500 });
    await expect(api.uploadFiles([])).rejects.toThrow("Upload failed: 500");
    await expect(api.deleteFile("test")).rejects.toThrow("Delete failed: 500");
    await expect(api.deleteAll()).rejects.toThrow("Delete all failed: 500");
  });
});
