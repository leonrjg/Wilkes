import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "./App";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useSearchStore } from "./stores/useSearchStore";
import { useSemanticStore } from "./stores/useSemanticStore";
import { useChatStore } from "./stores/useChatStore";
import { useBookmarksStore } from "./stores/useBookmarksStore";
import { useViewerStore } from "./stores/useViewerStore";
import { ToastProvider } from "./components/Toast";
import { source } from "./services";

// Mock services and hooks at top level
vi.mock("./services", () => ({
  api: {
    onEmbedProgress: vi.fn(() => Promise.resolve(() => {})),
    onEmbedDone: vi.fn(() => Promise.resolve(() => {})),
    onEmbedError: vi.fn(() => Promise.resolve(() => {})),
    onManagerEvent: vi.fn(() => Promise.resolve(() => {})),
    onFileMetadataUpdated: vi.fn(() => Promise.resolve(() => {})),
    onFileListChanged: vi.fn(() => Promise.resolve(() => {})),
    onBookmarkClusterLabelled: vi.fn(() => Promise.resolve(() => {})),
    onChunkTopicLabelled: vi.fn(() => Promise.resolve(() => {})),
    isGenerationReady: vi.fn(() => Promise.resolve(false)),
    getSettings: vi.fn(() => Promise.resolve({
      favorites: [],
      recent_dirs: [],
      last_directory: "/test/dir",
      respect_gitignore: true,
      max_file_size: 1024,
      theme: "Dark",
      search_prefer_semantic: false,
      semantic: { enabled: true, index_path: null, worker_timeout_secs: 300 },
      generation: {
        enabled: false,
        model: null,
        device: null,
        sampling_overrides: {},
      },
      supported_extensions: ["ts"],
    })),
    getLogs: vi.fn(() => Promise.resolve([])),
    getSupportedEngines: vi.fn(() => Promise.resolve(["SBERT"])),
    getIndexStatus: vi.fn(() => Promise.resolve(null)),
    isSemanticReady: vi.fn(() => Promise.resolve(true)),
    getDataPaths: vi.fn(() => Promise.resolve({ app_data: "" })),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: {
    type: "desktop",
    pickDirectory: vi.fn(),
    importFiles: vi.fn(() => Promise.resolve([])),
    readClipboardFiles: vi.fn(() => Promise.resolve([])),
  },
  isTauri: true,
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: vi.fn(() => ({
    onDragDropEvent: vi.fn(() => Promise.resolve(() => {})),
  })),
}));

vi.mock("./hooks/useTauriEvents", () => ({ useTauriEvents: vi.fn() }));
vi.mock("./components/preview/CodeViewer", () => ({ default: () => <div data-testid="code-viewer">CodeViewer</div> }));
vi.mock("./components/preview/PdfViewer", () => ({ default: () => <div data-testid="pdf-viewer">PdfViewer</div> }));
vi.mock("./components/SettingsModal", () => ({ default: ({ isOpen }: any) => isOpen ? <div data-testid="settings-modal">Settings Modal</div> : null }));

const bookmarkPaneActions = {
  openPane: useBookmarksStore.getState().openPane,
  closePane: useBookmarksStore.getState().closePane,
};
const viewerSessionActions = {
  restoreSession: useViewerStore.getState().restoreSession,
};

describe("App", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    useSettingsStore.setState({
      load: vi.fn().mockResolvedValue(undefined),
      directory: "/test/dir",
      favorites: [],
      recentDirs: [],
      setDirectory: vi.fn(),
      addFavorite: vi.fn(),
      removeFavorite: vi.fn(),
      applySettingsPatch: vi.fn(),
      setIndexing: vi.fn(),
    });
    useSemanticStore.setState({
      refreshCurrentRootStatus: vi.fn().mockResolvedValue(false),
      handleIndexUpdated: vi.fn().mockResolvedValue(undefined),
      readyForCurrentRoot: false,
      status: "idle",
      buildRoot: null,
      indexStatus: null,
      error: null,
    } as any);
    useSearchStore.setState({
      results: [],
      stats: null,
      searching: false,
      hasQuery: false,
    });
    useChatStore.setState({
      paneOpen: false,
      paneOpening: false,
      openPane: vi.fn().mockResolvedValue(undefined),
      togglePane: vi.fn(),
    });
    useBookmarksStore.setState({
      ...bookmarkPaneActions,
      bookmarks: [],
      filterText: "",
      scope: "all",
      paneOpen: false,
      load: vi.fn().mockResolvedValue(undefined),
    });
    useViewerStore.setState({
      activeTabId: null,
      tabs: [],
      sessionHydrated: false,
      restoreSession: viewerSessionActions.restoreSession,
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

  it("restores the viewer session after settings are ready", async () => {
    let finishSettings: (() => void) | undefined;
    const loadSettings = vi.fn(
      () => new Promise<void>((resolve) => {
        finishSettings = resolve;
      }),
    );
    const restoreSession = vi.fn().mockResolvedValue(undefined);
    useSettingsStore.setState({ load: loadSettings });
    useViewerStore.setState({ restoreSession });

    render(
      <ToastProvider>
        <App />
      </ToastProvider>,
    );

    expect(loadSettings).toHaveBeenCalledTimes(1);
    expect(restoreSession).not.toHaveBeenCalled();

    await act(async () => finishSettings?.());

    expect(restoreSession).toHaveBeenCalledTimes(1);
  });

  it("opens settings modal when clicked", async () => {
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });
    
    const settingsButton = screen.getByRole("button", { name: "Settings" });
    fireEvent.click(settingsButton);
    
    expect(screen.getByTestId("settings-modal")).toBeInTheDocument();
  });

  it("toggles the chat pane from the navbar button", async () => {
    const openPane = vi.fn().mockResolvedValue(undefined);
    const togglePane = vi.fn();
    useChatStore.setState({ paneOpen: false, openPane, togglePane });

    const { rerender } = render(
      <ToastProvider>
        <App />
      </ToastProvider>
    );

    fireEvent.click(screen.getByRole("button", { name: "Ask the documents" }));
    expect(openPane).toHaveBeenCalledTimes(1);
    expect(togglePane).not.toHaveBeenCalled();

    useChatStore.setState({ paneOpen: true, openPane, togglePane });
    rerender(
      <ToastProvider>
        <App />
      </ToastProvider>
    );

    fireEvent.click(screen.getByRole("button", { name: "Ask the documents" }));
    expect(togglePane).toHaveBeenCalledTimes(1);
    expect(openPane).toHaveBeenCalledTimes(1);
  });

  it("opens bookmarks in This file scope when a document was opened while the pane was closed", () => {
    const match = {
      path: "/tmp/current.txt",
      origin: { TextFile: { line: 1, col: 0 } },
    } as const;
    useViewerStore.setState({
      activeTabId: "current-tab",
      tabs: [{
        id: "current-tab",
        path: match.path,
        match,
        history: [match],
        historyIndex: 0,
        previewData: {
          Text: {
            content: "Current document",
            language: "text",
            highlight_line: 1,
            highlight_range: { start: 0, end: 0 },
          },
        },
        previewLoading: false,
        previewError: null,
        pdfLoadAttempt: 0,
        metadata: null,
        metadataStatus: "idle",
        requestId: 1,
      }],
    });

    render(
      <ToastProvider>
        <App />
      </ToastProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Bookmarks" }));

    expect(useBookmarksStore.getState()).toMatchObject({
      paneOpen: true,
      scope: "current",
    });
  });

  it("handles sidebar resizing", async () => {
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });

    const resizer = document.querySelector(".cursor-col-resize");
    expect(resizer).toBeInTheDocument();

    if (resizer) {
      fireEvent.mouseDown(resizer);
      fireEvent.mouseMove(window, { clientX: 400 });
      fireEvent.mouseUp(window);
    }
  });

  it("shows DirectoryPicker for desktop source", async () => {
    await act(async () => {
      render(
        <ToastProvider>
          <App />
        </ToastProvider>
      );
    });
    expect(screen.getByText("Open folder")).toBeInTheDocument();
  });

  it("imports files copied to the native clipboard on paste", async () => {
    const refreshFileList = vi.fn();
    useSettingsStore.setState({ refreshFileList });
    vi.mocked(source.readClipboardFiles).mockResolvedValueOnce(["/external/paper.pdf"]);
    vi.mocked(source.importFiles).mockResolvedValueOnce(["/test/dir/paper.pdf"]);

    render(
      <ToastProvider>
        <App />
      </ToastProvider>
    );

    fireEvent.paste(window);

    await vi.waitFor(() => {
      expect(source.importFiles).toHaveBeenCalledWith(
        ["/external/paper.pdf"],
        "/test/dir",
        "copy",
      );
      expect(refreshFileList).toHaveBeenCalledTimes(1);
    });
  });

  it("leaves ordinary text paste alone", async () => {
    vi.mocked(source.readClipboardFiles).mockResolvedValueOnce([]);

    render(
      <ToastProvider>
        <App />
      </ToastProvider>
    );

    fireEvent.paste(window);

    await vi.waitFor(() => expect(source.readClipboardFiles).toHaveBeenCalledTimes(1));
    expect(source.importFiles).not.toHaveBeenCalled();
  });
});
