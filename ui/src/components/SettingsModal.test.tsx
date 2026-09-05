import { render, screen, fireEvent, act, within } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SettingsModal from "./SettingsModal";
import { useGenerationStore } from "../stores/useGenerationStore";

// Mock sub-components
vi.mock("./SemanticPanel", () => ({ default: () => <div data-testid="semantic-panel">SemanticPanel</div> }));
vi.mock("./GenerationPanel", () => ({ default: () => <div data-testid="generation-panel">GenerationPanel</div> }));
vi.mock("./ChunkingPanel", () => ({ default: () => <div data-testid="chunking-panel">ChunkingPanel</div> }));
vi.mock("./DataPanel", () => ({ default: () => <div data-testid="data-panel">DataPanel</div> }));
vi.mock("./ExtensionsPanel", () => ({ default: () => <div data-testid="extensions-panel">ExtensionsPanel</div> }));
vi.mock("./IntegrationsPanel", () => ({ default: () => <div data-testid="integrations-panel">IntegrationsPanel</div> }));
vi.mock("./LogsPanel", () => ({ default: () => <div data-testid="logs-panel">LogsPanel</div> }));
vi.mock("./WorkersPanel", () => ({ default: () => <div data-testid="workers-panel">WorkersPanel</div> }));

vi.mock("codemirror", () => ({ basicSetup: [] }));
vi.mock("@codemirror/lang-json", () => ({ json: vi.fn() }));
vi.mock("@codemirror/theme-one-dark", () => ({ oneDark: [] }));
vi.mock("@codemirror/commands", () => ({ indentWithTab: {} }));
vi.mock("../services", () => ({ isTauri: true }));

describe("SettingsModal", () => {
  const mockApi = {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listGenerationModels: vi.fn(() => Promise.resolve([])),
    isGenerationReady: vi.fn(() => Promise.resolve(false)),
    loadGenerationModel: vi.fn(() => Promise.resolve(false)),
    onGenerationProgress: vi.fn(() => Promise.resolve(() => {})),
    onGenerationDone: vi.fn(() => Promise.resolve(() => {})),
    onGenerationError: vi.fn(() => Promise.resolve(() => {})),
    catalogueStatus: vi.fn(() => Promise.resolve({ providers: [], total_records: 0 })),
    catalogueSync: vi.fn(() => Promise.resolve({ providers: [], total_records: 0 })),
    onCatalogueSyncProgress: vi.fn(() => Promise.resolve(() => {})),
    imageRecognizerCatalogue: vi.fn(() =>
      Promise.resolve({ engines: ["Onnx"], models: [] }),
    ),
    imageRecognizerInventory: vi.fn(() =>
      Promise.resolve({
        name: "paddleocr-vl-1.6",
        repo: "PaddlePaddle/PaddleOCR-VL-1.6",
        revision: "c5630abae1d940eafe0697512a0325494b02ab42",
        license: "Apache-2.0",
        license_url: "https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6",
        derived_from: [],
        artifacts: [],
        footprint_bytes: 1_928_447_087,
      }),
    ),
    installImageRecognizer: vi.fn(() => Promise.resolve()),
    onImageAnalysisProgress: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisDone: vi.fn(() => Promise.resolve(() => {})),
    onImageAnalysisError: vi.fn(() => Promise.resolve(() => {})),
    getExternalMcpStatus: vi.fn(),
    configureExternalMcp: vi.fn(),
    rotateExternalMcpToken: vi.fn(),
    writeClipboard: vi.fn(),
  } as any;

  const defaultProps = {
    api: mockApi,
    isOpen: true,
    onClose: vi.fn(),
    directory: "/test",
    refreshSemanticReady: vi.fn(),
  };

  const mockSettings = {
    favorites: [],
    recent_dirs: [],
    last_directory: "/test",
    respect_gitignore: true,
    max_file_size: 1024 * 1024,
    theme: "System",
    pdf_auto_zoom_target_px: 15.5,
    search_prefer_semantic: false,
    semantic: { enabled: true, index_path: null, worker_timeout_secs: 300 },
    generation: {
      enabled: false,
      engine: "candle",
      model: null,
      device: null,
      ollama_url: "http://127.0.0.1:11434",
      sampling_overrides: {},
    },
    supported_extensions: ["ts"],
    file_tree_enabled: false,
    external_mcp: {
      enabled: false,
      require_token: false,
      bind_address: "127.0.0.1",
      port: 39217,
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
    useGenerationStore.setState({ ready: false });
    mockApi.getSettings.mockResolvedValue(mockSettings);
    mockApi.getExternalMcpStatus.mockResolvedValue({
      enabled: false,
      running: false,
      require_token: false,
      bind_address: "127.0.0.1",
      port: 39217,
      url: null,
      token: null,
      error: null,
    });
    mockApi.configureExternalMcp.mockResolvedValue({
      enabled: true,
      running: true,
      require_token: false,
      bind_address: "127.0.0.1",
      port: 39217,
      url: "http://127.0.0.1:39217/mcp",
      token: null,
      error: null,
    });
    mockApi.rotateExternalMcpToken.mockResolvedValue({
      enabled: true,
      running: true,
      require_token: true,
      bind_address: "127.0.0.1",
      port: 39217,
      url: "http://127.0.0.1:39217/mcp",
      token: "rotated-token",
      error: null,
    });
    mockApi.writeClipboard.mockResolvedValue(undefined);
  });

  it("renders when open", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("remains mounted but hidden when isOpen is false", async () => {
    let testContainer;
    await act(async () => {
      const { container } = render(<SettingsModal {...defaultProps} isOpen={false} />);
      testContainer = container;
    });
    expect(testContainer.firstChild).toHaveClass("hidden");
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("switches tabs", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByText("File extensions"));
    expect(screen.getByTestId("extensions-panel")).toBeInTheDocument();
  });

  it("groups Chat and Models under Generation", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });

    const generationSection = screen.getByText("Generation").parentElement;
    expect(generationSection).not.toBeNull();
    expect(within(generationSection!).getByRole("button", { name: "Chat" })).toBeInTheDocument();
    expect(
      within(generationSection!).getByRole("button", { name: "Generation Models" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Generation Models" }));
    expect(screen.getByTestId("generation-panel")).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Semantic Search Models" }));
    expect(screen.getByTestId("semantic-panel")).toBeVisible();
  });

  it("updates respect_gitignore", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByLabelText("Respect .gitignore files"));
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ respect_gitignore: false });
  });

  it("configures the file list as a collapsible folder tree", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByLabelText("Show folders in file list"));
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ file_tree_enabled: true });
  });

  it("updates theme", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByText("Dark"));
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ theme: "Dark" });
  });

  it("updates the PDF auto-zoom target", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.change(screen.getByLabelText("PDF auto-zoom target (px)"), {
      target: { value: "17" },
    });
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ pdf_auto_zoom_target_px: 17 });
  });

  it("persists chat custom instructions", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    fireEvent.change(screen.getByLabelText("Custom instructions"), {
      target: { value: "Answer in Spanish." },
    });
    fireEvent.blur(screen.getByLabelText("Custom instructions"));
    expect(mockApi.updateSettings).toHaveBeenCalledWith({
      chat_custom_instructions: "Answer in Spanish.",
    });
  });

  it("keeps the server listeners out of the Chat tab", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

    // jsdom knows nothing of Tailwind, so ask which pane each control sits in
    // rather than whether it paints.
    const paneOf = (el: HTMLElement) => el.closest("div.block, div.hidden");
    expect(paneOf(screen.getByLabelText("Custom instructions"))).toHaveClass("block");
    expect(paneOf(screen.getByLabelText("Serve MCP for external clients"))).toHaveClass("hidden");
    expect(paneOf(screen.getByLabelText("Serve the HTTP API"))).toHaveClass("hidden");
  });

  it("starts the external MCP server from Servers settings", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "MCP and HTTP API servers" }));

    await act(async () => {
      fireEvent.click(screen.getByLabelText("Serve MCP for external clients"));
    });

    expect(mockApi.configureExternalMcp).toHaveBeenCalledWith(
      true,
      false,
      "127.0.0.1",
      39217,
    );
    expect(await screen.findByText("Listening")).toBeInTheDocument();
    expect(screen.getByText("http://127.0.0.1:39217/mcp")).toBeInTheDocument();
  });

  it("copies tokenless client setup commands by default", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "MCP and HTTP API servers" }));
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Serve MCP for external clients"));
    });

    fireEvent.click(await screen.findByRole("button", { name: "Copy Claude setup" }));
    expect(mockApi.writeClipboard).toHaveBeenLastCalledWith(
      "claude mcp add --transport http --scope user wilkes http://127.0.0.1:39217/mcp",
    );
    fireEvent.click(screen.getByRole("button", { name: "Copy Codex setup" }));
    expect(mockApi.writeClipboard).toHaveBeenLastCalledWith(
      "codex mcp add --url http://127.0.0.1:39217/mcp wilkes",
    );
    expect(screen.queryByRole("button", { name: "Copy token" })).not.toBeInTheDocument();
  });

  it("allows a non-loopback bind address and warns about network exposure", async () => {
    mockApi.configureExternalMcp.mockResolvedValue({
      enabled: true,
      running: true,
      require_token: false,
      bind_address: "0.0.0.0",
      port: 39217,
      url: "http://0.0.0.0:39217/mcp",
      token: null,
      error: null,
    });
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "MCP and HTTP API servers" }));
    fireEvent.change(screen.getByLabelText("External MCP bind address"), {
      target: { value: "0.0.0.0" },
    });
    expect(screen.getByText(/exposes Wilkes MCP without authentication/)).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    });
    expect(mockApi.configureExternalMcp).toHaveBeenCalledWith(
      false,
      false,
      "0.0.0.0",
      39217,
    );
    expect(await screen.findByText("http://0.0.0.0:39217/mcp")).toBeInTheDocument();
  });

  it("copies client setup commands without exposing the token as text", async () => {
    mockApi.getExternalMcpStatus.mockResolvedValue({
      enabled: true,
      running: true,
      require_token: true,
      bind_address: "127.0.0.1",
      port: 39217,
      url: "http://127.0.0.1:39217/mcp",
      token: "secret-token",
      error: null,
    });
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "MCP and HTTP API servers" }));

    await act(async () => {
      fireEvent.click(await screen.findByRole("button", { name: "Copy Claude setup" }));
    });
    expect(mockApi.writeClipboard).toHaveBeenCalledWith(
      expect.stringContaining('Authorization: Bearer secret-token'),
    );
    expect(screen.queryByText("secret-token")).not.toBeInTheDocument();
  });

  it("preserves the custom instructions cursor while persisting an edit", async () => {
    mockApi.getSettings.mockResolvedValue({
      ...mockSettings,
      chat_custom_instructions: "Write concise answers.",
    });
    mockApi.updateSettings.mockResolvedValue({
      ...mockSettings,
      chat_custom_instructions: "Write very concise answers.",
    });

    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

    const textarea = screen.getByLabelText("Custom instructions") as HTMLTextAreaElement;
    vi.useFakeTimers();
    fireEvent.change(textarea, { target: { value: "Write very concise answers." } });
    textarea.focus();
    textarea.setSelectionRange(10, 10);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    expect(textarea).toHaveValue("Write very concise answers.");
    expect(textarea.selectionStart).toBe(10);
    expect(textarea.selectionEnd).toBe(10);
    vi.useRealTimers();
  });

  it("switches to JSON and applies changes", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByText("Raw config"));
    const applyBtn = screen.getByText("Apply Changes");
    mockApi.updateSettings.mockResolvedValue(undefined);
    await act(async () => {
      fireEvent.click(applyBtn);
    });
    expect(mockApi.updateSettings).toHaveBeenCalled();
  });

  it("gates HyDE on generation readiness and persists PRF as a complete retrieval object", async () => {
    // Return the merged settings so the component re-renders with the toggle on.
    mockApi.updateSettings.mockImplementation(async (patch: any) => ({ ...mockSettings, ...patch }));

    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });

    expect(screen.getByText("Query enhancement")).toBeInTheDocument();

    // Generation is not ready in this harness, so enabling HyDE must be disabled.
    const hydeCheckbox = screen
      .getByText(/HyDE/i)
      .closest("label")!
      .querySelector("input[type=checkbox]") as HTMLInputElement;
    expect(hydeCheckbox.disabled).toBe(true);

    // PRF has no generation dependency and must persist the full nested object,
    // because the backend merges settings at the top level only.
    const prfCheckbox = screen
      .getByText(/Pseudo-relevance feedback/i)
      .closest("label")!
      .querySelector("input[type=checkbox]") as HTMLInputElement;
    await act(async () => {
      fireEvent.click(prfCheckbox);
    });

    expect(mockApi.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({
        retrieval: expect.objectContaining({
          hyde: expect.any(Object),
          pseudo_relevance_feedback: expect.objectContaining({ enabled: true }),
        }),
      }),
    );
  });

  it("allows enabled HyDE to be turned off when generation is unavailable", async () => {
    const settingsWithHyde = {
      ...mockSettings,
      retrieval: {
        hyde: { enabled: true, hypotheticals: 1, include_query: true },
        pseudo_relevance_feedback: {
          enabled: false,
          feedback_docs: 5,
          alpha: 1,
          beta: 0.5,
        },
      },
    };
    mockApi.getSettings.mockResolvedValue(settingsWithHyde);
    mockApi.updateSettings.mockImplementation(async (patch: any) => ({
      ...settingsWithHyde,
      ...patch,
    }));

    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });

    const hydeCheckbox = screen
      .getByText(/HyDE/i)
      .closest("label")!
      .querySelector("input[type=checkbox]") as HTMLInputElement;
    expect(hydeCheckbox.checked).toBe(true);
    expect(hydeCheckbox.disabled).toBe(false);

    await act(async () => {
      fireEvent.click(hydeCheckbox);
    });

    expect(mockApi.updateSettings).toHaveBeenCalledWith(
      expect.objectContaining({
        retrieval: expect.objectContaining({
          hyde: expect.objectContaining({ enabled: false }),
        }),
      }),
    );
  });
});
