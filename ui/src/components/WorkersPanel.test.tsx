import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import WorkersPanel from "./WorkersPanel";

describe("WorkersPanel", () => {
  const mockApi = {
    getWorkerStatuses: vi.fn(),
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
    mockApi.getWorkerStatuses.mockResolvedValue([
      {
        active: true,
        role: "embed",
        engine: "SBERT",
        model: "test-model",
        device: "cpu",
        request_mode: "embed",
        pid: 1234,
        timeout_secs: 300,
      },
      {
        active: false,
        role: "generate",
        engine: null,
        model: null,
        device: null,
        request_mode: null,
        pid: null,
        timeout_secs: 300,
      },
      {
        active: false,
        role: "recognize",
        engine: null,
        model: null,
        device: null,
        request_mode: null,
        pid: null,
        timeout_secs: 300,
      },
    ]);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("renders worker status", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    expect(screen.getByText("Embedding worker")).toBeInTheDocument();
    // Every role is listed: the processes die independently, so one status
    // would misreport a dead generation or recognition worker as healthy.
    expect(screen.getByText("Generation worker")).toBeInTheDocument();
    expect(screen.getByText("Recognition worker")).toBeInTheDocument();
    expect(screen.getByText("Active")).toBeInTheDocument();
    expect(screen.getAllByText("Idle")).toHaveLength(2);
    expect(screen.getByText("SBERT")).toBeInTheDocument();
    expect(screen.getByText("cpu")).toBeInTheDocument();
    expect(screen.getByText("embed")).toBeInTheDocument();
    expect(screen.getByText("1234")).toBeInTheDocument();
  });

  it("renders the realized generation device and timing telemetry", async () => {
    mockApi.getWorkerStatuses.mockResolvedValue([
      {
        active: true,
        role: "generate",
        engine: "candle",
        model: "qwen3-0.6b",
        device: "cpu",
        request_mode: "generate",
        pid: 4321,
        timeout_secs: 300,
        generation: {
          requested_device: "auto",
          fallback_reason: "Metal initialization failed",
          model_load_micros: 1_250_000,
          timings: {
            prompt_micros: 2_000,
            decode_micros: 130_000,
            constraint_micros: 25_000,
          },
        },
      },
    ]);

    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });

    expect(screen.getByText("cpu")).toBeInTheDocument();
    expect(screen.getByText("1250.0 ms")).toBeInTheDocument();
    expect(screen.getByText("2.0 ms")).toBeInTheDocument();
    expect(screen.getByText("130.0 ms")).toBeInTheDocument();
    expect(screen.getByText("25.0 ms")).toBeInTheDocument();
    expect(screen.getByText(/Requested auto; using cpu/)).toBeInTheDocument();
    expect(screen.getByText(/Metal initialization failed/)).toBeInTheDocument();
  });

  it("names each idle worker by its own role", async () => {
    // An idle worker used to report no role at all, and the panel guessed
    // "embed" for every row: three identical "Embedding worker" headings.
    mockApi.getWorkerStatuses.mockResolvedValue(
      ["embed", "generate", "recognize"].map((role) => ({
        active: false,
        role,
        engine: null,
        model: null,
        device: null,
        request_mode: null,
        pid: null,
        timeout_secs: 300,
      })),
    );

    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });

    expect(screen.getByText("Embedding worker")).toBeInTheDocument();
    expect(screen.getByText("Generation worker")).toBeInTheDocument();
    expect(screen.getByText("Recognition worker")).toBeInTheDocument();
  });

  it("offers Kill Worker only for the embedding worker", async () => {
    // `killWorker` kills the embedder, so the button on any other row would
    // kill a worker other than the one it sits next to.
    mockApi.getWorkerStatuses.mockResolvedValue([
      {
        active: true,
        role: "recognize",
        engine: "onnx",
        model: "recognizer",
        device: "cpu",
        request_mode: "recognize",
        pid: 9001,
        timeout_secs: 300,
      },
    ]);

    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });

    expect(screen.getByText("Recognition worker")).toBeInTheDocument();
    expect(screen.queryByText("Kill Worker")).not.toBeInTheDocument();
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
