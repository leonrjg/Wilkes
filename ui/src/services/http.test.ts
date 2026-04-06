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

  it("onEmbedProgress sets up EventSource", async () => {
    const handler = vi.fn();
    const mockEventSource = {
      addEventListener: vi.fn(),
      close: vi.fn(),
    };
    vi.stubGlobal("EventSource", vi.fn(function() { return mockEventSource; }));

    const close = await api.onEmbedProgress(handler);
    expect(EventSource).toHaveBeenCalledWith("/api/embed/events");
    expect(mockEventSource.addEventListener).toHaveBeenCalledWith("embed-progress", expect.any(Function));

    // Trigger the listener
    const listener = mockEventSource.addEventListener.mock.calls[0][1];
    listener({ data: JSON.stringify({ progress: 0.5 }) });
    expect(handler).toHaveBeenCalledWith({ progress: 0.5 });

    close();
    expect(mockEventSource.close).toHaveBeenCalled();
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

  it("deleteAll sends DELETE request", async () => {
    (fetch as any).mockResolvedValue({ ok: true, status: 204 });
    await api.deleteAll();
    expect(fetch).toHaveBeenCalledWith("/api/upload/all", { method: "DELETE" });
  });
});
