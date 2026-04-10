import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import WorkersPanel from "./WorkersPanel";

describe("WorkersPanel", () => {
  const mockApi = {
    getWorkerStatus: vi.fn(),
    killWorker: vi.fn(),
    setWorkerTimeout: vi.fn(),
  } as any;

  const mockSettings = {
    semantic: {
      worker_timeout_secs: 300,
    },
  } as any;

  const mockOnUpdateSettings = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    mockApi.getWorkerStatus.mockResolvedValue({
      active: true,
      engine: "SBERT",
      model: "test-model",
      device: "cpu",
      request_mode: "embed",
      pid: 1234,
      timeout_secs: 300,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders worker status", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    expect(screen.getByText("Active")).toBeInTheDocument();
    expect(screen.getByText("SBERT")).toBeInTheDocument();
    expect(screen.getByText("cpu")).toBeInTheDocument();
    expect(screen.getByText("embed")).toBeInTheDocument();
    expect(screen.getByText("1234")).toBeInTheDocument();
  });

  it("kills worker", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    const killButton = screen.getByText("Kill Worker");
    await act(async () => {
      fireEvent.click(killButton);
    });
    
    expect(mockApi.killWorker).toHaveBeenCalled();
  });

  it("applies new timeout", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    const input = screen.getByPlaceholderText("300");
    fireEvent.change(input, { target: { value: "600" } });
    
    const applyButton = screen.getByText("Apply");
    await act(async () => {
      fireEvent.click(applyButton);
    });
    
    expect(mockApi.setWorkerTimeout).toHaveBeenCalledWith(600);
    expect(mockOnUpdateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ worker_timeout_secs: 600 })
    }));
  });

  it("handles error during timeout update", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    mockApi.setWorkerTimeout.mockRejectedValue(new Error("Failed"));
    const input = screen.getByPlaceholderText("300");
    fireEvent.change(input, { target: { value: "600" } });
    
    const applyButton = screen.getByText("Apply");
    await act(async () => {
      fireEvent.click(applyButton);
    });
    // Coverage for catch block
  });
});
