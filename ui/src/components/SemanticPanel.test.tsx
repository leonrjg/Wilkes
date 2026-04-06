import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SemanticPanel from "./SemanticPanel";

describe("SemanticPanel", () => {
  let progressHandler: any;
  let doneHandler: any;
  let errorHandler: any;

  const mockApi = {
    getSettings: vi.fn(),
    getSupportedEngines: vi.fn(),
    listModels: vi.fn(),
    getIndexStatus: vi.fn(),
    getPythonInfo: vi.fn(),
    onEmbedProgress: vi.fn().mockImplementation((h) => {
      progressHandler = h;
      return Promise.resolve(() => {});
    }),
    onEmbedDone: vi.fn().mockImplementation((h) => {
      doneHandler = h;
      return Promise.resolve(() => {});
    }),
    onEmbedError: vi.fn().mockImplementation((h) => {
      errorHandler = h;
      return Promise.resolve(() => {});
    }),
    onManagerEvent: vi.fn().mockImplementation(() => {
      return Promise.resolve(() => {});
    }),
    updateSettings: vi.fn().mockResolvedValue(undefined),
    downloadModel: vi.fn().mockResolvedValue(undefined),
    buildIndex: vi.fn().mockResolvedValue(undefined),
    deleteIndex: vi.fn().mockResolvedValue(undefined),
    cancelEmbed: vi.fn().mockResolvedValue(undefined),
    getModelSize: vi.fn(),
    getLogs: vi.fn(),
  } as any;

  const defaultSettings = {
    semantic: {
      enabled: false,
      engine: "Candle",
      model: "initial-id",
      dimension: 384,
      engine_devices: { SBERT: "cpu", Candle: "cpu" },
      custom_models: [],
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockApi.getSettings.mockResolvedValue(defaultSettings);
    mockApi.getSupportedEngines.mockResolvedValue(["SBERT", "Candle"]);
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "", size_bytes: 1024 * 1024 },
    ]);
    mockApi.getIndexStatus.mockResolvedValue(null);
    mockApi.getPythonInfo.mockResolvedValue("/usr/bin/python3");
    mockApi.getLogs.mockResolvedValue([]);
    mockApi.getModelSize.mockResolvedValue(1024 * 1024 * 100);
  });

  it("renders correctly and loads data", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    expect(screen.getByText("SBERT")).toBeInTheDocument();
    expect(screen.getByText("Initial")).toBeInTheDocument();
  });

  it("switches to Candle engine", async () => {
    mockApi.getSettings.mockResolvedValue({
        semantic: { ...defaultSettings.semantic, engine: "SBERT" }
    });
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    const candleBtn = screen.getByText("Candle");
    await act(async () => {
      fireEvent.click(candleBtn);
    });
    expect(mockApi.updateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ engine: "Candle" })
    }));
  });

  it("handles model selection", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
      { model_id: "new-id", display_name: "New Model", is_cached: true, description: "" },
    ]);

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const newModelBtn = screen.getByText("New Model");
    await act(async () => {
      fireEvent.click(newModelBtn);
    });

    expect(mockApi.updateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ model: "new-id" })
    }));
  });

  it("handles model download and triggering progress", async () => {
    // Force not_downloaded phase by providing a non-cached model
    mockApi.listModels.mockResolvedValue([
      { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
    ]);
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, model: "not-cached" }
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const downloadBtn = screen.getByText(/Download Model/i);
    await act(async () => {
      fireEvent.click(downloadBtn);
    });

    expect(mockApi.downloadModel).toHaveBeenCalled();

    // Trigger progress
    await act(async () => {
      progressHandler({ Download: { bytes_received: 25000000, total_bytes: 50000000 } });
    });

    // Now it should show "Cancel download" and "Starting engine..." (as per current implementation)
    expect(screen.getByText(/Cancel download/i)).toBeInTheDocument();
    expect(screen.getByText(/Starting engine.../i)).toBeInTheDocument();

    // Trigger done
    await act(async () => {
      doneHandler({ operation: "Download" });
    });

    // Should return to ready phase
    expect(screen.queryByText(/Downloading…/)).not.toBeInTheDocument();
  });

  it("shows indexing progress via events", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const buildButton = screen.getByText("Build semantic index");
    await act(async () => {
      fireEvent.click(buildButton);
    });

    await act(async () => {
      progressHandler({ Build: { files_processed: 45, total_files: 100 } });
    });

    expect(screen.getByText(/45%/)).toBeInTheDocument();
  });

  it("handles cancel embedding", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const buildButton = screen.getByText("Build semantic index");
    await act(async () => {
      fireEvent.click(buildButton);
    });

    await act(async () => {
      progressHandler({ Build: { files_processed: 50, total_files: 100 } });
    });

    const cancelBtn = screen.getByText(/Cancel build/i);
    await act(async () => {
      fireEvent.click(cancelBtn);
    });

    expect(mockApi.cancelEmbed).toHaveBeenCalled();
  });

  it("displays error from event", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      errorHandler({ message: "Something went wrong", operation: "Build" });
    });

    expect(screen.getByText("Something went wrong")).toBeInTheDocument();
  });
});
