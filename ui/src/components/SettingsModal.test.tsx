import { render, screen, fireEvent, act, within } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SettingsModal from "./SettingsModal";

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
      model: null,
      device: null,
      sampling_overrides: {},
    },
    supported_extensions: ["ts"],
    external_mcp: {
      enabled: false,
      require_token: false,
      bind_address: "127.0.0.1",
      port: 39217,
    },
  };

  beforeEach(() => {
    vi.clearAllMocks();
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

  it("starts the external MCP server from Chat settings", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

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
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
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
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
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
    fireEvent.click(screen.getByRole("button", { name: "Chat" }));

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
    fireEvent.click(screen.getByText("Settings (JSON)"));
    const applyBtn = screen.getByText("Apply Changes");
    mockApi.updateSettings.mockResolvedValue(undefined);
    await act(async () => {
      fireEvent.click(applyBtn);
    });
    expect(mockApi.updateSettings).toHaveBeenCalled();
  });
});
