import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import WorkersPanel from "./WorkersPanel";

describe("WorkersPanel", () => {
  const mockApi = {
    getWorkerStatuses: vi.fn(),
    killWorker: vi.fn(),
    setWorkerTimeout: vi.fn(),
    getPythonEnvs: vi.fn().mockResolvedValue({ envs: [], total_bytes: 0 }),
    removePythonEnvs: vi.fn().mockResolvedValue(undefined),
  } as any;

  const mockSettings = {
    semantic: {
      worker_timeout_secs: 300,
      embed_batch_size: 16,
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
    
    const applyButton = screen.getByLabelText("Apply idle timeout");
    await act(async () => {
      fireEvent.click(applyButton);
    });
    
    expect(mockApi.setWorkerTimeout).toHaveBeenCalledWith(600);
    expect(mockOnUpdateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ worker_timeout_secs: 600 })
    }));
  });

  it("applies a new embedding batch size", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });

    fireEvent.change(screen.getByPlaceholderText("16"), { target: { value: "8" } });
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Apply embedding batch size"));
    });

    expect(mockOnUpdateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ embed_batch_size: 8 })
    }));
  });

  // The control that decides whether a build fits in memory. A value that is
  // not a batch size is refused and said so, rather than quietly becoming one
  // the user never chose and cannot see.
  it.each([["0", "zero"], ["-4", "negative"], ["", "empty"]])(
    "refuses a %s batch size rather than substituting one",
    async (value) => {
      await act(async () => {
        render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
      });

      fireEvent.change(screen.getByPlaceholderText("16"), { target: { value } });
      await act(async () => {
        fireEvent.click(screen.getByLabelText("Apply embedding batch size"));
      });

      expect(mockOnUpdateSettings).not.toHaveBeenCalled();
      expect(screen.getByText(/must be a positive integer/i)).toBeInTheDocument();
    }
  );

  it("handles error during timeout update", async () => {
    await act(async () => {
      render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
    });
    
    mockApi.setWorkerTimeout.mockRejectedValue(new Error("Failed"));
    const input = screen.getByPlaceholderText("300");
    fireEvent.change(input, { target: { value: "600" } });
    
    const applyButton = screen.getByLabelText("Apply idle timeout");
    await act(async () => {
      fireEvent.click(applyButton);
    });
    // Coverage for catch block
  });

  describe("Python environments", () => {
    const base = {
      id: "b",
      is_base: true,
      transformers: "5.22.1",
      python: "3.14",
      packages: [],
      size_bytes: 700 * 1_048_576,
      models: [],
      last_used_at: null,
    };
    const inUse = { ...base, id: "u", is_base: false, transformers: "4.47.1", python: "3.13", packages: ["timm"], models: ["org/old"] };
    const idle = { ...base, id: "i", is_base: false, transformers: "4.39.3", python: "3.12" };
    const idleToo = { ...idle, id: "j", transformers: "4.36.2" };

    beforeEach(() => {
      mockApi.getWorkerStatuses.mockResolvedValue([]);
      mockApi.removePythonEnvs.mockClear();
      mockApi.getPythonEnvs.mockResolvedValue({ envs: [base, inUse, idle, idleToo], total_bytes: 900 * 1_048_576 });
    });

    afterEach(() => {
      mockApi.getPythonEnvs.mockResolvedValue({ envs: [], total_bytes: 0 });
    });

    const renderPanel = async () => {
      await act(async () => {
        render(<WorkersPanel api={mockApi} settings={mockSettings} onUpdateSettings={mockOnUpdateSettings} />);
      });
    };

    it("says what each environment is and what uses it", async () => {
      await renderPanel();

      expect(screen.getByText("Transformers 4.47.1 on Python 3.13")).toBeInTheDocument();
      expect(screen.getByText("org/old")).toBeInTheDocument();
      expect(screen.getByText(/with timm/)).toBeInTheDocument();
      expect(screen.getAllByText("Not used by any model")).toHaveLength(2);
      expect(screen.getByText("Built-in models")).toBeInTheDocument();
      expect(screen.getByText("900 MB in total")).toBeInTheDocument();
    });

    it("offers to rebuild the base environment and to remove the others", async () => {
      await renderPanel();

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Rebuild Transformers 5.22.1 on Python 3.14" }));
      });
      expect(mockApi.removePythonEnvs).toHaveBeenCalledWith(["b"]);

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Remove Transformers 4.39.3 on Python 3.12" }));
      });
      expect(mockApi.removePythonEnvs).toHaveBeenCalledWith(["i"]);
    });

    it("removes only what nothing uses when asked to remove all unused", async () => {
      await renderPanel();

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Remove all unused" }));
      });

      expect(mockApi.removePythonEnvs).toHaveBeenCalledWith(["i", "j"]);
    });

    it("shows why a removal failed", async () => {
      mockApi.removePythonEnvs.mockRejectedValueOnce("an index is being built");
      await renderPanel();

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Remove Transformers 4.39.3 on Python 3.12" }));
      });

      expect(screen.getByText("an index is being built")).toBeInTheDocument();
    });
  });
});
