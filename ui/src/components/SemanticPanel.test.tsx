import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SemanticPanel from "./SemanticPanel";
import { useSettingsStore } from "../stores/useSettingsStore";

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
    updateSettings: vi.fn(),
    downloadModel: vi.fn().mockResolvedValue(undefined),
    buildIndex: vi.fn().mockResolvedValue(undefined),
    deleteIndex: vi.fn().mockResolvedValue(undefined),
    cancelEmbed: vi.fn().mockResolvedValue(undefined),
    getModelSize: vi.fn(),
    getLogs: vi.fn(),
  } as any;

  const defaultSettings = {
    bookmarked_dirs: [],
    recent_dirs: [],
    last_directory: null,
    respect_gitignore: true,
    max_file_size: 10 * 1024 * 1024,
    context_lines: 2,
    theme: "System",
    search_prefer_semantic: false,
    supported_extensions: [],
    max_results: 50,
    semantic: {
      enabled: false,
      selected: {
        engine: "Candle",
        model: "initial-id",
        dimension: 384,
      },
      engine_devices: { SBERT: "cpu", Candle: "cpu" },
      custom_models: [],
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({
      bookmarks: [],
      recentDirs: [],
      directory: "",
      semantic: null,
      respectGitignore: true,
      maxFileSize: 10 * 1024 * 1024,
      contextLines: 2,
      supportedExtensions: [],
      fileList: [],
      filterText: "",
      excluded: new Set(),
      preferSemantic: false,
      indexing: false,
      theme: "System",
      maxResults: 50,
    });
    mockApi.getSettings.mockResolvedValue(defaultSettings);
    mockApi.getSupportedEngines.mockResolvedValue(["SBERT", "Candle"]);
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "", size_bytes: 1024 * 1024 },
    ]);
    mockApi.getIndexStatus.mockResolvedValue(null);
    mockApi.getPythonInfo.mockResolvedValue("/usr/bin/python3");
    mockApi.getLogs.mockResolvedValue([]);
    mockApi.getModelSize.mockResolvedValue(1024 * 1024 * 100);
    mockApi.updateSettings.mockImplementation(async (patch: any) => ({
      ...defaultSettings,
      ...patch,
      semantic: patch.semantic ?? defaultSettings.semantic,
    }));
  });

  it("renders correctly and loads data", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    expect(screen.getByText("SBERT")).toBeInTheDocument();
    expect(screen.getByText("Initial")).toBeInTheDocument();
  });

  it("clears cancelling state after cancel succeeds", async () => {
    mockApi.getIndexStatus.mockResolvedValue({
      indexed_files: 0,
      total_chunks: 0,
      engine: "Candle",
      model_id: "initial-id",
      dimension: 384,
      root_path: null,
      built_at: null,
      build_duration_ms: null,
      db_size_bytes: null,
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    act(() => {
      progressHandler({ Build: { files_processed: 1, total_files: 10, message: "Indexing", done: false } });
    });

    const cancelButton = screen.getByRole("button", { name: /cancel build/i });
    await act(async () => {
      fireEvent.click(cancelButton);
    });

    expect(mockApi.cancelEmbed).toHaveBeenCalled();
    expect(screen.queryByText("Cancelling…")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /build semantic index/i })).toBeInTheDocument();
  });

  it("switches to Candle engine", async () => {
    mockApi.getSettings.mockResolvedValue({
        semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, engine: "SBERT" } }
    });
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });
    const candleBtn = screen.getByText("Candle");
    await act(async () => {
      fireEvent.click(candleBtn);
    });
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  it("keeps engine changes as draft until action", async () => {
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, engine: "SBERT", model: "initial-id" } }
    });
    mockApi.listModels.mockImplementation(async (engine: string) => {
      if (engine === "Candle") {
        return [{ model_id: "candle-default", display_name: "Candle Default", is_cached: true, description: "" }];
      }
      return [{ model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" }];
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Candle"));
    });

    expect(mockApi.updateSettings).not.toHaveBeenCalled();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: /download model and index files/i }));
    });

    expect(mockApi.downloadModel).toHaveBeenCalledWith({
      engine: "Candle",
      model: "sentence-transformers/all-MiniLM-L12-v2",
      dimension: 384,
    });
  });

  it("handles model selection", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
      { model_id: "new-id", display_name: "New Model", is_cached: true, description: "" },
    ]);

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    // Clicking a model only sets the pending selection — no API call yet.
    const newModelBtn = screen.getByText("New Model");
    await act(async () => {
      fireEvent.click(newModelBtn);
    });
    expect(mockApi.updateSettings).not.toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({ model: "new-id" })
    }));

    // Pressing the action button uses the draft selection without persisting settings first.
    const actionBtn = screen.getByRole("button", { name: /build semantic index/i });
    await act(async () => {
      fireEvent.click(actionBtn);
    });
    expect(mockApi.buildIndex).toHaveBeenCalledWith("/test", expect.objectContaining({ model: "new-id" }));
  });

  it("handles model download and triggering progress", async () => {
    // Force not_downloaded phase by providing a non-cached model
    mockApi.listModels.mockResolvedValue([
      { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
    ]);
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, model: "not-cached" } }
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const downloadBtn = screen.getByText(/Download model and index files/i);
    await act(async () => {
      fireEvent.click(downloadBtn);
    });

    expect(mockApi.downloadModel).toHaveBeenCalled();

    // Trigger progress
    await act(async () => {
      progressHandler({ Download: { bytes_received: 25000000, total_bytes: 50000000 } });
    });

    // Now it should show the active download state with the compact in-bar percentage.
    expect(screen.getByText(/Cancel download/i)).toBeInTheDocument();
    expect(screen.getByText(/Downloading model/i)).toBeInTheDocument();
    expect(screen.getByText(/50%/i)).toBeInTheDocument();

    // Trigger done
    await act(async () => {
      doneHandler({ operation: "Download" });
    });

    expect(mockApi.updateSettings).not.toHaveBeenCalled();
    expect(mockApi.buildIndex).toHaveBeenCalledWith("/test", {
      engine: "Candle",
      model: "not-cached",
      dimension: 384,
    });
    expect(screen.getByText(/Cancel build/i)).toBeInTheDocument();
  });

  it("does not start a queued build after cancelling a download", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
    ]);
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, model: "not-cached" } }
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText(/Download model and index files/i));
    });

    await act(async () => {
      progressHandler({ Download: { bytes_received: 25000000, total_bytes: 50000000 } });
    });

    await act(async () => {
      fireEvent.click(screen.getByText(/Cancel download/i));
    });

    expect(mockApi.cancelEmbed).toHaveBeenCalled();

    await act(async () => {
      doneHandler({ operation: "Download" });
    });

    expect(mockApi.buildIndex).not.toHaveBeenCalled();
    expect(screen.getByText(/Download model and index files/i)).toBeInTheDocument();
  });

  it("clears the queued build when download emits an error", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
    ]);
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, model: "not-cached" } }
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText(/Download model and index files/i));
    });

    await act(async () => {
      errorHandler({ message: "Download failed", operation: "Download" });
    });

    expect(await screen.findByText("Download failed")).toBeInTheDocument();

    await act(async () => {
      doneHandler({ operation: "Download" });
    });

    expect(mockApi.buildIndex).not.toHaveBeenCalled();
  });

  it("returns to idle after a plain download completion with no queued build", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      progressHandler({ Download: { bytes_received: 25000000, total_bytes: 50000000 } });
    });

    expect(screen.getByText(/Cancel download/i)).toBeInTheDocument();

    await act(async () => {
      doneHandler({ operation: "Download" });
    });

    expect(mockApi.buildIndex).not.toHaveBeenCalled();
    expect(screen.getByText(/Build semantic index/i)).toBeInTheDocument();
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

  it("does not persist draft selection on build completion from the panel", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
      { model_id: "new-id", display_name: "New Model", is_cached: true, description: "" },
    ]);

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText("New Model"));
      fireEvent.click(screen.getByRole("button", { name: /build semantic index/i }));
    });

    expect(mockApi.updateSettings).not.toHaveBeenCalled();

    await act(async () => {
      doneHandler({ operation: "Build" });
    });

    expect(mockApi.updateSettings).not.toHaveBeenCalled();
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

    expect(await screen.findByText("Something went wrong")).toBeInTheDocument();
  });

  it("toggles advanced settings and handles device change", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const advancedBtn = screen.getByText(/Advanced/i);
    await act(async () => {
      fireEvent.click(advancedBtn);
    });

    const checkbox = screen.getByLabelText(/Disable hardware acceleration/i);
    expect(checkbox).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(checkbox);
    });

    expect(mockApi.updateSettings).toHaveBeenCalled();
  });

  it("handles adding a custom model", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const addCustomBtn = screen.getByText(/Add Custom/i);
    await act(async () => {
      fireEvent.click(addCustomBtn);
    });

    const input = screen.getByPlaceholderText(/e.g. org\/repo-name/i);
    await act(async () => {
      fireEvent.change(input, { target: { value: "org/custom-model" } });
      fireEvent.click(screen.getByText(/^Add$/));
    });

    expect(mockApi.updateSettings).toHaveBeenCalledWith(expect.objectContaining({
      semantic: expect.objectContaining({
        custom_models: expect.arrayContaining([{ engine: "Candle", model_id: "org/custom-model" }])
      })
    }));
  });

  it("filters models by search text", async () => {
    mockApi.listModels.mockResolvedValue([
      { model_id: "model-a", display_name: "Apple", is_cached: true, description: "" },
      { model_id: "model-b", display_name: "Banana", is_cached: true, description: "" },
    ]);

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    const searchInput = screen.getByPlaceholderText(/Search models…/i);
    await act(async () => {
      fireEvent.change(searchInput, { target: { value: "apple" } });
    });

    expect(screen.getByText("Apple")).toBeInTheDocument();
    expect(screen.queryByText("Banana")).not.toBeInTheDocument();
  });

  it("displays index stats when indexed", async () => {
    mockApi.getIndexStatus.mockResolvedValue({
      engine: "Candle",
      model_id: "initial-id",
      indexed_files: 100,
      total_chunks: 500,
      built_at: Math.floor(Date.now() / 1000),
      build_duration_ms: 5000,
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    expect(screen.getByText("100")).toBeInTheDocument();
    expect(screen.getByText("500")).toBeInTheDocument();
    expect(screen.getByText(/Delete Index/i)).toBeInTheDocument();
  });
});
