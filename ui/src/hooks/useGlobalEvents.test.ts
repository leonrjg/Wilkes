import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useGlobalEvents } from "./useGlobalEvents";
import { api } from "../services";
import { useToasts } from "../components/Toast";
import { useSearchStore } from "../stores/useSearchStore";

vi.mock("../services", () => ({
  api: {
    onManagerEvent: vi.fn().mockResolvedValue(vi.fn()),
  },
}));

vi.mock("../components/Toast", () => ({
  useToasts: vi.fn(),
}));

vi.mock("../stores/useSearchStore", () => ({
  useSearchStore: {
    getState: vi.fn(),
  },
}));

describe("useGlobalEvents", () => {
  const addToast = vi.fn().mockReturnValue("toast-id");
  const removeToast = vi.fn();
  const replaySearch = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    (useToasts as any).mockReturnValue({ addToast, removeToast });
    (useSearchStore.getState as any).mockReturnValue({ replaySearch });
  });

  it("handles WorkerStarting event", async () => {
    let handler: any;
    (api.onManagerEvent as any).mockImplementation((h: any) => {
      handler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());
    
    // Wait for the promise to resolve
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 0));
    });

    act(() => {
      handler("WorkerStarting");
    });

    expect(addToast).toHaveBeenCalledWith(expect.stringContaining("Starting worker"), expect.any(Object));
  });

  it("handles Reindexing and ReindexingDone events", async () => {
    let handler: any;
    (api.onManagerEvent as any).mockImplementation((h: any) => {
      handler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());
    
    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 0));
    });

    act(() => {
      handler("Reindexing");
    });
    expect(addToast).toHaveBeenCalledWith(expect.stringContaining("Indexing..."), expect.any(Object));

    act(() => {
      handler("ReindexingDone");
    });
    expect(removeToast).toHaveBeenCalledWith("toast-id");
    expect(replaySearch).toHaveBeenCalled();
  });
});