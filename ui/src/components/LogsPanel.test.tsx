import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import LogsPanel from "./LogsPanel";

describe("LogsPanel", () => {
  const mockApi = {
    getLogs: vi.fn(),
    clearLogs: vi.fn(),
  } as any;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    mockApi.getLogs.mockResolvedValue(["Log line 1", "Log line 2"]);
    
    // Mock clipboard
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: vi.fn(),
      },
      configurable: true,
    });
    
    // Mock confirm
    vi.stubGlobal("confirm", vi.fn(() => true));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders logs", async () => {
    await act(async () => {
      render(<LogsPanel api={mockApi} />);
    });
    expect(screen.getByText("Log line 1")).toBeInTheDocument();
    expect(screen.getByText("Log line 2")).toBeInTheDocument();
  });

  it("copies logs to clipboard", async () => {
    await act(async () => {
      render(<LogsPanel api={mockApi} />);
    });
    const copyButton = screen.getByText("Copy");
    fireEvent.click(copyButton);
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Log line 1\nLog line 2");
  });

  it("clears logs", async () => {
    await act(async () => {
      render(<LogsPanel api={mockApi} />);
    });
    const clearButton = screen.getByText("Clear");
    await act(async () => {
      fireEvent.click(clearButton);
    });
    expect(mockApi.clearLogs).toHaveBeenCalled();
    expect(screen.getByText(/No logs available/i)).toBeInTheDocument();
  });

  it("refreshes logs on interval", async () => {
    await act(async () => {
      render(<LogsPanel api={mockApi} />);
    });
    
    mockApi.getLogs.mockResolvedValue(["New log"]);
    
    await act(async () => {
      vi.advanceTimersByTime(3001);
    });
    
    expect(screen.getByText("New log")).toBeInTheDocument();
  });
});
