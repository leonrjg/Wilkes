import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useTauriEvents } from "./useTauriEvents";
import { useToasts } from "../components/Toast";
import { useSearchStore } from "../stores/useSearchStore";

// Mock the services to control isTauri
vi.mock("../services", async () => {
  const actual = await vi.importActual("../services") as any;
  return {
    ...actual,
    isTauri: true,
  };
});

// Mock Toast provider
vi.mock("../components/Toast", () => ({
  useToasts: vi.fn(),
}));

// Mock Tauri event listen
const mockListen = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: any[]) => mockListen(...args),
}));

describe("useTauriEvents", () => {
  const mockAddToast = vi.fn().mockReturnValue("toast-id");
  const mockRemoveToast = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    (useToasts as any).mockReturnValue({
      addToast: mockAddToast,
      removeToast: mockRemoveToast,
    });
    useSearchStore.setState({
      replaySearch: vi.fn(),
    });
    mockListen.mockResolvedValue(vi.fn());
  });

  it("should setup listener when in Tauri", async () => {
    renderHook(() => useTauriEvents());

    await waitFor(() => {
      expect(mockListen).toHaveBeenCalledWith("manager-event", expect.any(Function));
    });
  });

  it("should handle WorkerStarting event", async () => {
    renderHook(() => useTauriEvents());

    await waitFor(() => expect(mockListen).toHaveBeenCalled());
    
    const handler = mockListen.mock.calls[0][1];
    handler({ payload: "WorkerStarting" });

    expect(mockAddToast).toHaveBeenCalledWith(expect.stringContaining("Starting worker"), { type: "info" });
  });

  it("should handle Reindexing and ReindexingDone events", async () => {
    renderHook(() => useTauriEvents());

    await waitFor(() => expect(mockListen).toHaveBeenCalled());
    
    const handler = mockListen.mock.calls[0][1];
    
    // Trigger Reindexing
    handler({ payload: "Reindexing" });
    expect(mockAddToast).toHaveBeenCalledWith(expect.stringContaining("Reindexing"), expect.any(Object));
    
    // Trigger ReindexingDone
    const replaySearchMock = vi.fn();
    useSearchStore.setState({ replaySearch: replaySearchMock });
    
    handler({ payload: "ReindexingDone" });
    expect(mockRemoveToast).toHaveBeenCalledWith("toast-id");
    expect(replaySearchMock).toHaveBeenCalled();
  });
});
