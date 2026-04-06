import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import SettingsModal from "./SettingsModal";

// Mock sub-components
vi.mock("./SemanticPanel", () => ({ default: () => <div data-testid="semantic-panel">SemanticPanel</div> }));
vi.mock("./ChunkingPanel", () => ({ default: () => <div data-testid="chunking-panel">ChunkingPanel</div> }));
vi.mock("./DataPanel", () => ({ default: () => <div data-testid="data-panel">DataPanel</div> }));
vi.mock("./ExtensionsPanel", () => ({ default: () => <div data-testid="extensions-panel">ExtensionsPanel</div> }));
vi.mock("./LogsPanel", () => ({ default: () => <div data-testid="logs-panel">LogsPanel</div> }));
vi.mock("./WorkersPanel", () => ({ default: () => <div data-testid="workers-panel">WorkersPanel</div> }));

vi.mock("codemirror", () => ({ basicSetup: [] }));
vi.mock("@codemirror/lang-json", () => ({ json: vi.fn() }));
vi.mock("@codemirror/theme-one-dark", () => ({ oneDark: [] }));
vi.mock("@codemirror/commands", () => ({ indentWithTab: {} }));

describe("SettingsModal", () => {
  const mockApi = {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
  } as any;

  const defaultProps = {
    api: mockApi,
    isOpen: true,
    onClose: vi.fn(),
    directory: "/test",
    refreshSemanticReady: vi.fn(),
  };

  const mockSettings = {
    bookmarked_dirs: [],
    recent_dirs: [],
    last_directory: "/test",
    respect_gitignore: true,
    max_file_size: 1024 * 1024,
    theme: "System",
    search_prefer_semantic: false,
    semantic: { enabled: true, index_path: null, worker_timeout_secs: 300 },
    supported_extensions: ["ts"],
  };

  beforeEach(() => {
    vi.clearAllMocks();
    mockApi.getSettings.mockResolvedValue(mockSettings);
  });

  it("renders when open", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    expect(screen.getByText("Settings")).toBeInTheDocument();
  });

  it("switches tabs", async () => {
    await act(async () => {
      render(<SettingsModal {...defaultProps} />);
    });
    fireEvent.click(screen.getByText("File extensions"));
    expect(screen.getByTestId("extensions-panel")).toBeInTheDocument();
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
