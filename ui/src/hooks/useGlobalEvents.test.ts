import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useGlobalEvents } from "./useGlobalEvents";
import { api } from "../services";
import { useToasts } from "../components/Toast";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";

vi.mock("../services", () => ({
  api: {
    onManagerEvent: vi.fn().mockResolvedValue(vi.fn()),
    onFileListChanged: vi.fn().mockResolvedValue(vi.fn()),
    onFileMetadataUpdated: vi.fn().mockResolvedValue(vi.fn()),
    onBookmarkClusterLabelled: vi.fn().mockResolvedValue(vi.fn()),
    isGenerationReady: vi.fn().mockResolvedValue(false),
  },
}));

vi.mock("../components/Toast", () => ({
  useToasts: vi.fn(),
}));

vi.mock("../stores/useSettingsStore", () => ({
  useSettingsStore: {
    getState: vi.fn(),
  },
}));

vi.mock("../stores/useSemanticStore", () => ({
  useSemanticStore: {
    getState: vi.fn(),
  },
}));

vi.mock("../stores/useBookmarksStore", () => ({
  useBookmarksStore: {
    getState: vi.fn(),
  },
}));

describe("useGlobalEvents", () => {
  const addToast = vi.fn().mockReturnValue("toast-id");
  const removeToast = vi.fn();
  const handleIndexUpdated = vi.fn().mockResolvedValue(undefined);
  const handleIndexTerminated = vi.fn().mockResolvedValue(undefined);
  const refreshFileList = vi.fn();
  const applyMetadataUpdates = vi.fn();
  const loadBookmarks = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.clearAllMocks();
    (useToasts as any).mockReturnValue({ addToast, removeToast });
    (useSettingsStore.getState as any).mockReturnValue({
      directory: "/docs",
      refreshFileList,
      applyMetadataUpdates,
    });
    (useSemanticStore.getState as any).mockReturnValue({
      handleIndexUpdated,
      handleIndexTerminated,
    });
    (useBookmarksStore.getState as any).mockReturnValue({ load: loadBookmarks });
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

  it("handles Reindexing and ReindexingDone events without refreshing the file list", async () => {
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
    expect(refreshFileList).not.toHaveBeenCalled();
    expect(addToast).toHaveBeenCalledWith(expect.stringContaining("Indexing..."), expect.any(Object));

    act(() => {
      handler("ReindexingDone");
    });
    expect(removeToast).toHaveBeenCalledWith("toast-id");
    expect(handleIndexUpdated).toHaveBeenCalled();
  });

  it("refreshes the file list on matching file-list-changed events", async () => {
    let fileListHandler: any;
    (api.onFileListChanged as any).mockImplementation((h: any) => {
      fileListHandler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    act(() => {
      fileListHandler({ root: "/docs" });
    });

    expect(refreshFileList).toHaveBeenCalled();
  });

  it("ignores file-list-changed events for other roots", async () => {
    let fileListHandler: any;
    (api.onFileListChanged as any).mockImplementation((h: any) => {
      fileListHandler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    act(() => {
      fileListHandler({ root: "/other" });
    });

    expect(refreshFileList).not.toHaveBeenCalled();
  });

  it("applies metadata updates and reloads bookmarks on file-metadata-updated", async () => {
    let metadataHandler: any;
    (api.onFileMetadataUpdated as any).mockImplementation((h: any) => {
      metadataHandler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    const updates = [{ path: "/docs/renamed.pdf", publication_date: "2021" }];
    act(() => {
      metadataHandler(updates);
    });

    expect(applyMetadataUpdates).toHaveBeenCalledWith(updates);
    expect(loadBookmarks).toHaveBeenCalled();
  });

  it("closes the reindex toast when reindexing is cancelled", async () => {
    let managerHandler: any;
    (api.onManagerEvent as any).mockImplementation((h: any) => {
      managerHandler = h;
      return Promise.resolve(vi.fn());
    });

    renderHook(() => useGlobalEvents());

    await act(async () => {
      await new Promise(resolve => setTimeout(resolve, 0));
    });

    act(() => {
      managerHandler("Reindexing");
    });

    act(() => {
      managerHandler("ReindexingCancelled");
    });

    expect(removeToast).toHaveBeenCalledWith("toast-id");
    expect(handleIndexUpdated).not.toHaveBeenCalled();
    expect(handleIndexTerminated).toHaveBeenCalled();
  });
});
