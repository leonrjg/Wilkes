import { render, screen, fireEvent, act, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SemanticPanel from "./SemanticPanel";
import { useSettingsStore } from "../stores/useSettingsStore";

describe("SemanticPanel", () => {
  let progressHandler: any;
  let doneHandler: any;
  let errorHandler: any;

  const mockApi = {
    getSettings: vi.fn(),
    getEmbedderCapabilities: vi.fn(),
    getIndexStatus: vi.fn(),
    getPythonEnvs: vi.fn(),
    removePythonEnvs: vi.fn(),
    removeCustomModel: vi.fn(),
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
    onFileMetadataUpdated: vi.fn().mockImplementation(() => {
      return Promise.resolve(() => {});
    }),
    updateSettings: vi.fn(),
    downloadModel: vi.fn().mockResolvedValue(undefined),
    buildIndex: vi.fn().mockResolvedValue(undefined),
    deleteIndex: vi.fn().mockResolvedValue(undefined),
    cancelEmbed: vi.fn().mockResolvedValue(undefined),
    getModelSize: vi.fn(),
    searchCustomModels: vi.fn(),
    getLogs: vi.fn(),
  } as any;

  /** The wire shape of one model, with only the fields a test names spelled
   *  out. `engine` defaults to Candle because that is what defaultSettings
   *  selects. */
  const capability = (model: any) => ({
    engine: "Candle",
    description: "",
    repository_id: null,
    dimension: 384,
    supported_dimensions: [384],
    query_prefix: null,
    passage_prefix: null,
    prefix_source: "undetermined",
    max_input_tokens: null,
    size_bytes: null,
    preferred_batch_size: null,
    catalogued: true,
    is_default: false,
    is_recommended: false,
    ...model,
    locally_available: model.locally_available ?? model.is_cached ?? false,
  });

  const manifest = (models: any[], engines: string[] = ["SBERT", "Candle"]) => ({
    engines,
    roles: ["query", "passage"],
    models: models.map(capability),
  });

  const defaultSettings = {
    favorites: [],
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
      favorites: [],
      recentDirs: [],
      directory: "",
      semantic: null,
      respectGitignore: true,
      maxFileSize: 10 * 1024 * 1024,
      contextLines: 2,
      supportedExtensions: [],
      fileList: [],
      preferSemantic: false,
      indexing: false,
      theme: "System",
      maxResults: 50,
    });
    mockApi.getSettings.mockResolvedValue(defaultSettings);
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "initial-id", display_name: "Initial", is_cached: true, size_bytes: 1024 * 1024 },
      ]),
    );
    mockApi.getIndexStatus.mockResolvedValue(null);
    mockApi.getPythonEnvs.mockResolvedValue({ envs: [], total_bytes: 0 });
    mockApi.removePythonEnvs.mockResolvedValue(undefined);
    mockApi.removeCustomModel.mockResolvedValue(undefined);
    mockApi.getLogs.mockResolvedValue([]);
    mockApi.getModelSize.mockResolvedValue(1024 * 1024 * 100);
    mockApi.searchCustomModels.mockResolvedValue([]);
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
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { engine: "Candle", model_id: "candle-default", display_name: "Candle Default", is_cached: true },
        { engine: "SBERT", model_id: "initial-id", display_name: "Initial", is_cached: true },
      ]),
    );

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
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
        { model_id: "new-id", display_name: "New Model", is_cached: true, description: "" },
      ]),
    );

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

  describe("Python environments", () => {
    const sbertSettings = (selected: string, custom: string[]) => ({
      ...defaultSettings,
      semantic: {
        ...defaultSettings.semantic,
        selected: { engine: "SBERT", model: selected, dimension: 384 },
        custom_models: custom.map((model_id) => ({ engine: "SBERT", model_id })),
      },
    });
    const env = (models: string[]) => ({
      id: "e",
      is_base: false,
      transformers: "4.47.1",
      python: "3.13",
      packages: [],
      size_bytes: 600 * 1_048_576,
      models,
      last_used_at: null,
    });

    const renderOn = async (selected: string, custom: string[], models: any[]) => {
      mockApi.getSettings.mockResolvedValue(sbertSettings(selected, custom));
      mockApi.getEmbedderCapabilities.mockResolvedValue(manifest(models));
      await act(async () => {
        render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
      });
    };

    const models = [
      { engine: "SBERT", model_id: "org/kept", display_name: "Kept", is_cached: true },
      { engine: "SBERT", model_id: "org/old", display_name: "Old", is_cached: true },
    ];

    it("says which environment the selected model runs in", async () => {
      mockApi.getPythonEnvs.mockResolvedValue({ envs: [env(["org/kept"])], total_bytes: 0 });

      await renderOn("org/kept", ["org/kept"], models);

      expect(screen.getByText("Transformers 4.47.1 on Python 3.13")).toBeInTheDocument();
    });

    it("says an environment is coming for a custom model not installed yet", async () => {
      await renderOn("org/kept", ["org/kept"], models);

      expect(screen.getByText("Set up to suit this model when it is installed")).toBeInTheDocument();
    });

    it("offers removal only for models the user added", async () => {
      await renderOn("org/kept", ["org/old"], [
        ...models,
        { engine: "SBERT", model_id: "org/shipped", display_name: "Shipped", is_cached: true },
      ]);

      expect(screen.getByRole("button", { name: "Remove Old" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Remove Shipped" })).not.toBeInTheDocument();
    });

    it("does not offer to remove the selected model", async () => {
      await renderOn("org/old", ["org/old"], models);

      expect(screen.queryByRole("button", { name: "Remove Old" })).not.toBeInTheDocument();
    });

    it("removes a model and, when nothing else uses it, its environment", async () => {
      mockApi.getPythonEnvs.mockResolvedValue({ envs: [env(["org/old"])], total_bytes: 0 });
      await renderOn("org/kept", ["org/old"], models);

      fireEvent.click(screen.getByRole("button", { name: "Remove Old" }));
      expect(screen.getByText(/Also remove its Python environment \(600 MB\)/)).toBeInTheDocument();
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Remove" }));
      });

      expect(mockApi.removeCustomModel).toHaveBeenCalledWith("SBERT", "org/old");
      expect(mockApi.removePythonEnvs).toHaveBeenCalledWith(["e"]);
    });

    it("keeps an environment the user chose to keep or another model shares", async () => {
      mockApi.getPythonEnvs.mockResolvedValue({ envs: [env(["org/old", "org/other"])], total_bytes: 0 });
      await renderOn("org/kept", ["org/old"], models);

      fireEvent.click(screen.getByRole("button", { name: "Remove Old" }));
      expect(screen.queryByText(/Also remove its Python environment/)).not.toBeInTheDocument();
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Remove" }));
      });

      expect(mockApi.removeCustomModel).toHaveBeenCalled();
      expect(mockApi.removePythonEnvs).not.toHaveBeenCalled();
    });

    it("warns when the current index was built with the model", async () => {
      mockApi.getIndexStatus.mockResolvedValue({ model_id: "org/old", engine: "SBERT" });
      await renderOn("org/kept", ["org/old"], models);

      fireEvent.click(screen.getByRole("button", { name: "Remove Old" }));

      expect(screen.getByText(/will need rebuilding/)).toBeInTheDocument();
    });

    it("shows what the environment setup is doing while it does", async () => {
      await renderOn("org/kept", ["org/kept"], [
        { engine: "SBERT", model_id: "org/kept", display_name: "Kept", is_cached: false },
      ]);

      await act(async () => {
        progressHandler({ Setup: "Installing Python 3.13" });
      });

      expect(screen.getByText("Installing Python 3.13")).toBeInTheDocument();
    });
  });

  it("handles model download and triggering progress", async () => {
    // Force not_downloaded phase by providing a non-cached model
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
      ]),
    );
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

  it("shows download feedback immediately after starting a model download", async () => {
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
      ]),
    );
    mockApi.getSettings.mockResolvedValue({
      semantic: { ...defaultSettings.semantic, selected: { ...defaultSettings.semantic.selected, model: "not-cached" } }
    });

    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText(/Download model and index files/i));
    });

    expect(mockApi.downloadModel).toHaveBeenCalled();
    expect(screen.getByText(/Cancel download/i)).toBeInTheDocument();
    expect(screen.getByText(/Downloading model/i)).toBeInTheDocument();
    expect(screen.getByText(/0%/i)).toBeInTheDocument();
  });

  it("does not start a queued build after cancelling a download", async () => {
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
      ]),
    );
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
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "not-cached", display_name: "Not Cached", is_cached: false, description: "", size_bytes: 50000000 },
      ]),
    );
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

  it("shows build feedback immediately after starting an index build", async () => {
    await act(async () => {
      render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
    });

    await act(async () => {
      fireEvent.click(screen.getByText("Build semantic index"));
    });

    expect(mockApi.buildIndex).toHaveBeenCalled();
    expect(screen.getByText(/Cancel build/i)).toBeInTheDocument();
    expect(screen.getByText(/Indexing/i)).toBeInTheDocument();
    expect(screen.getByText(/0%/i)).toBeInTheDocument();
  });

  it("does not persist draft selection on build completion from the panel", async () => {
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "initial-id", display_name: "Initial", is_cached: true, description: "" },
        { model_id: "new-id", display_name: "New Model", is_cached: true, description: "" },
      ]),
    );

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

    const input = screen.getByPlaceholderText(/org\/repo-name/i);
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

  describe("searching Hugging Face for a custom model", () => {
    const openForm = async () => {
      await act(async () => {
        render(<SemanticPanel api={mockApi} directory="/test" refreshSemanticReady={vi.fn()} />);
      });
      await act(async () => {
        fireEvent.click(screen.getByText(/Add Custom/i));
      });
    };

    it("opens on the most downloaded models for the engine", async () => {
      mockApi.searchCustomModels.mockResolvedValue([
        { model_id: "org/popular", model_type: "bert", downloads: 1_500_000 },
      ]);

      await openForm();

      expect(await screen.findByText("org/popular")).toBeInTheDocument();
      expect(screen.getByText("bert")).toBeInTheDocument();
      expect(screen.getByText(/1\.5M downloads/)).toBeInTheDocument();
      expect(mockApi.searchCustomModels).toHaveBeenCalledWith("Candle", "");
    });

    it("waits for typing to pause and searches once", async () => {
      await openForm();
      await waitFor(() => expect(mockApi.searchCustomModels).toHaveBeenCalledTimes(1));
      mockApi.searchCustomModels.mockClear();

      const input = screen.getByPlaceholderText(/org\/repo-name/i);
      fireEvent.change(input, { target: { value: "b" } });
      fireEvent.change(input, { target: { value: "bg" } });
      fireEvent.change(input, { target: { value: "bge" } });

      await waitFor(() => expect(mockApi.searchCustomModels).toHaveBeenCalled());
      expect(mockApi.searchCustomModels).toHaveBeenCalledTimes(1);
      expect(mockApi.searchCustomModels).toHaveBeenCalledWith("Candle", "bge");
    });

    it("searches a pasted URL as the repository it names", async () => {
      await openForm();
      await waitFor(() => expect(mockApi.searchCustomModels).toHaveBeenCalledTimes(1));

      fireEvent.change(screen.getByPlaceholderText(/org\/repo-name/i), {
        target: { value: "https://huggingface.co/org/model/tree/main" },
      });

      await waitFor(() => expect(mockApi.searchCustomModels).toHaveBeenCalledWith("Candle", "org/model"));
    });

    it("ignores an answer to a search that has been replaced", async () => {
      let answerFirst: (hits: any[]) => void = () => {};
      mockApi.searchCustomModels.mockReturnValueOnce(new Promise((resolve) => { answerFirst = resolve; }));
      await openForm();
      await waitFor(() => expect(mockApi.searchCustomModels).toHaveBeenCalledTimes(1));

      mockApi.searchCustomModels.mockResolvedValue([
        { model_id: "org/current", model_type: null, downloads: 1 },
      ]);
      fireEvent.change(screen.getByPlaceholderText(/org\/repo-name/i), { target: { value: "cur" } });
      expect(await screen.findByText("org/current")).toBeInTheDocument();

      await act(async () => {
        answerFirst([{ model_id: "org/stale", model_type: null, downloads: 1 }]);
      });

      expect(screen.queryByText("org/stale")).not.toBeInTheDocument();
      expect(screen.getByText("org/current")).toBeInTheDocument();
    });

    it("adds and selects a result when it is clicked", async () => {
      mockApi.searchCustomModels.mockResolvedValue([
        { model_id: "org/found", model_type: "bert", downloads: 10 },
      ]);
      await openForm();

      const hit = await screen.findByText("org/found");
      await act(async () => {
        fireEvent.click(hit);
      });

      expect(mockApi.updateSettings).toHaveBeenCalledWith(expect.objectContaining({
        semantic: expect.objectContaining({
          custom_models: expect.arrayContaining([{ engine: "Candle", model_id: "org/found" }]),
        }),
      }));
      expect(screen.queryByPlaceholderText(/org\/repo-name/i)).not.toBeInTheDocument();
    });

    it("chooses a result already in the list instead of adding it again", async () => {
      mockApi.searchCustomModels.mockResolvedValue([
        { model_id: "initial-id", model_type: "bert", downloads: 10 },
      ]);
      await openForm();

      const row = (await screen.findByText("In list")).closest("button")!;
      await act(async () => {
        fireEvent.click(row);
      });

      expect(mockApi.updateSettings).not.toHaveBeenCalledWith(expect.objectContaining({
        semantic: expect.objectContaining({
          custom_models: expect.arrayContaining([expect.objectContaining({ model_id: "initial-id" })]),
        }),
      }));
      expect(screen.queryByPlaceholderText(/org\/repo-name/i)).not.toBeInTheDocument();
    });

    it("keeps the repository ID route open when the search fails", async () => {
      vi.spyOn(console, "error").mockImplementation(() => {});
      mockApi.searchCustomModels.mockRejectedValue(new Error("offline"));
      await openForm();

      expect(await screen.findByText(/Could not search Hugging Face/)).toBeInTheDocument();
      expect(screen.getByText(/^Add$/)).toBeEnabled();
    });

    it("says when nothing matched", async () => {
      await openForm();

      expect(await screen.findByText("No embedding models found")).toBeInTheDocument();
    });
  });

  it("filters models by search text", async () => {
    mockApi.getEmbedderCapabilities.mockResolvedValue(
      manifest([
        { model_id: "model-a", display_name: "Apple", is_cached: true, description: "" },
        { model_id: "model-b", display_name: "Banana", is_cached: true, description: "" },
      ]),
    );

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
