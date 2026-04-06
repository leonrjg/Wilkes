import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SemanticPanel from "./SemanticPanel";

describe("SemanticPanel", () => {
  const mockApi = {
    getSettings: vi.fn(),
    getSupportedEngines: vi.fn(),
    listModels: vi.fn(),
    getIndexStatus: vi.fn(),
    getPythonInfo: vi.fn(),
    onEmbedProgress: vi.fn(() => Promise.resolve(() => {})),
    onEmbedDone: vi.fn(() => Promise.resolve(() => {})),
    onEmbedError: vi.fn(() => Promise.resolve(() => {})),
    updateSettings: vi.fn(),
    downloadModel: vi.fn(),
    buildIndex: vi.fn(),
    deleteIndex: vi.fn(),
    cancelEmbed: vi.fn(),
    getModelSize: vi.fn(),
    getLogs: vi.fn(),
  } as any;

  const defaultSettings = {
    semantic: {
      enabled: false,
      engine: "SBERT",
      model: "initial-id",
      dimension: 384,
      engine_devices: { SBERT: "cpu" },
      custom_models: [],
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockApi.getSettings.mockResolvedValue(defaultSettings);
    mockApi.getSupportedEngines.mockResolvedValue(["SBERT", "Candle"]);
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
    ]);
    mockApi.getIndexStatus.mockResolvedValue(null);
    mockApi.getPythonInfo.mockResolvedValue("/usr/bin/python3");
    mockApi.getLogs.mockResolvedValue([]);
  });

  it("renders correctly and loads data", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    expect(screen.getByText("SBERT")).toBeInTheDocument();
    expect(screen.getByText(/Sentence-Transformers via Python/)).toBeInTheDocument();
    expect(screen.getByText("Initial")).toBeInTheDocument();
  });

  it("switches to Candle engine", async () => {
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

  it("handles build index starting", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    const buildButton = screen.getByText("Build semantic index");
    mockApi.buildIndex.mockResolvedValue(undefined);
    await act(async () => {
      fireEvent.click(buildButton);
    });
    expect(mockApi.buildIndex).toHaveBeenCalled();
  });

  it("toggles hardware acceleration", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    const advancedButton = screen.getByText(/Advanced/);
    fireEvent.click(advancedButton);
    const checkbox = screen.getByLabelText("Disable hardware acceleration");
    await act(async () => {
      fireEvent.click(checkbox);
    });
    expect(mockApi.updateSettings).toHaveBeenCalled();
  });
});
