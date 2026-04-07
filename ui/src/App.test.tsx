import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "./App";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useSearchStore } from "./stores/useSearchStore";
import { api, source } from "./services";
import { ToastProvider } from "./components/Toast";

// Mock services and hooks at top level
vi.mock("./services", () => ({
  api: {
    onEmbedProgress: vi.fn(() => Promise.resolve(() => {})),
    onEmbedDone: vi.fn(() => Promise.resolve(() => {})),
    onEmbedError: vi.fn(() => Promise.resolve(() => {})),
    onManagerEvent: vi.fn(() => Promise.resolve(() => {})),
    getSettings: vi.fn(() => Promise.resolve({
      bookmarked_dirs: [],
      recent_dirs: [],
      last_directory: "/test/dir",
      respect_gitignore: true,
      max_file_size: 1024,
      theme: "Dark",
      search_prefer_semantic: false,
      semantic: { enabled: true, index_path: null, worker_timeout_secs: 300 },
      supported_extensions: ["ts"],
    })),
    getLogs: vi.fn(() => Promise.resolve([])),
    getSupportedEngines: vi.fn(() => Promise.resolve(["SBERT"])),
    getIndexStatus: vi.fn(() => Promise.resolve(null)),
    isSemanticReady: vi.fn(() => Promise.resolve(true)),
    getDataPaths: vi.fn(() => Promise.resolve({ app_data: "" })),
    listFiles: vi.fn(() => Promise.resolve([])),
  },
  source: {
    type: "desktop",
    pickDirectory: vi.fn(),
  },
  isTauri: true,
}));

vi.mock("./hooks/useTauriEvents", () => ({ useTauriEvents: vi.fn() }));
vi.mock("./components/preview/CodeViewer", () => ({ default: () => <div data-testid="code-viewer">CodeViewer</div> }));
vi.mock("./components/preview/PdfViewer", () => ({ default: () => <div data-testid="pdf-viewer">PdfViewer</div> }));
vi.mock("./components/SettingsModal", () => ({ default: ({ isOpen }: any) => isOpen ? <div data-testid="settings-modal">Settings Modal</div> : null }));

describe("App", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSettingsStore.setState({
      load: vi.fn().mockResolvedValue(undefined),
      directory: "/test/dir",
      bookmarks: [],
      recentDirs: [],
      setDirectory: vi.fn(),
      addBookmark: vi.fn(),
      removeBookmark: vi.fn(),
      refreshSemanticReady: vi.fn(),
      applySettingsPatch: vi.fn(),
      setIndexing: vi.fn(),
    });
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
    });
  });

  it("renders correctly", async () => {
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });
    expect(screen.getByPlaceholderText("Search…")).toBeInTheDocument();
    expect(screen.getByText("Open folder")).toBeInTheDocument();
  });

  it("loads settings on mount", async () => {
    const loadMock = vi.fn().mockResolvedValue(undefined);
    useSettingsStore.setState({ load: loadMock });
    
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });
    
    expect(loadMock).toHaveBeenCalled();
  });

  it("opens settings modal when clicked", async () => {
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });
    
    const settingsButton = screen.getByTitle("Settings");
    fireEvent.click(settingsButton);
    
    expect(screen.getByTestId("settings-modal")).toBeInTheDocument();
  });

  it("picks a directory", async () => {
    const setDirectoryMock = vi.fn();
    useSettingsStore.setState({ setDirectory: setDirectoryMock });
    (source as any).pickDirectory.mockResolvedValue("/picked/path");

    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });

    const pickButton = screen.getByText("Open folder");
    await act(async () => {
      fireEvent.click(pickButton);
    });

    expect(source.pickDirectory).toHaveBeenCalled();
    expect(setDirectoryMock).toHaveBeenCalledWith("/picked/path");
  });
});
