import { useState, useCallback, useRef, useEffect } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Bookmark, Cloud, MessageSquare, Settings as SettingsIcon, ChevronDown, Loader } from "react-feather";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import BookmarksPane from "./components/BookmarksPane";
import ChatPane from "./components/ChatPane";
import TopicCloudPane from "./components/TopicCloudPane";
import DirectoryPicker from "./components/DirectoryPicker";
import WorkspacePicker from "./components/WorkspacePicker";
import UploadZone from "./components/UploadZone";
import SettingsModal from "./components/SettingsModal";
import { useToasts } from "./components/Toast";
import { useContextMenu, ContextMenu } from "./components/ContextMenu";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useBookmarksStore } from "./stores/useBookmarksStore";
import { useChatStore } from "./stores/useChatStore";
import { useSemanticStore } from "./stores/useSemanticStore";
import { useTopicsStore } from "./stores/useTopicsStore";
import { useActiveWorkspaceReadOnly, useWorkspaceStore } from "./stores/useWorkspaceStore";
import { activeViewerTab, useViewerStore } from "./stores/useViewerStore";
import { useGlobalEvents } from "./hooks/useGlobalEvents";
import { api, source, isTauri } from "./services";
import type { AgentBackend } from "./lib/types";
import type { DesktopSourceApi, WebSourceApi } from "./services/api";

export default function App() {
  useGlobalEvents();
  const { addToast } = useToasts();

  const loadSettings = useSettingsStore((s) => s.load);
  const loadWorkspaces = useWorkspaceStore((s) => s.load);
  const workspaceSwitching = useWorkspaceStore((s) => s.switching);
  const readOnly = useActiveWorkspaceReadOnly();
  const loadBookmarks = useBookmarksStore((s) => s.load);
  const openBookmarksPane = useBookmarksStore((s) => s.openPane);
  const closeBookmarksPane = useBookmarksStore((s) => s.closePane);
  const bookmarksPaneOpen = useBookmarksStore((s) => s.paneOpen);
  const bookmarksDock = useSettingsStore((s) => s.bookmarksDock);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const setDirectory = useSettingsStore((s) => s.setDirectory);
  const addFavorite = useSettingsStore((s) => s.addFavorite);
  const removeFavorite = useSettingsStore((s) => s.removeFavorite);
  const forgetDirectory = useSettingsStore((s) => s.forgetDirectory);
  const renameDirectory = useSettingsStore((s) => s.renameDirectory);
  const refreshFileList = useSettingsStore((s) => s.refreshFileList);
  const applySettingsPatch = useSettingsStore((s) => s.applySettingsPatch);
  const setIndexing = useSettingsStore((s) => s.setIndexing);
  const refreshSemanticReady = useSemanticStore((s) => s.refreshCurrentRootStatus);
  const handleIndexUpdated = useSemanticStore((s) => s.handleIndexUpdated);
  const semanticReadyForRoot = useSemanticStore((s) => s.readyForCurrentRoot);
  const preferSemantic = useSettingsStore((s) => s.preferSemantic);
  const topicsPaneOpen = useTopicsStore((s) => s.paneOpen);
  const openTopicsPane = useTopicsStore((s) => s.openPane);
  const closeTopicsPane = useTopicsStore((s) => s.closePane);

  const chatPaneOpen = useChatStore((s) => s.paneOpen);
  const chatPaneOpening = useChatStore((s) => s.paneOpening);
  const toggleChatPane = useChatStore((s) => s.togglePane);
  const openChatPane = useChatStore((s) => s.openPane);
  const loadChatBackends = useChatStore((s) => s.loadBackends);
  const { menu: chatBackendMenu, openMenu: openChatBackendMenu, closeMenu: closeChatBackendMenu } =
    useContextMenu<null>();

  const openMatch = useViewerStore((state) => state.openMatch);
  const openFile = useViewerStore((state) => state.openFile);
  const restoreViewerSession = useViewerStore((state) => state.restoreSession);
  const remapViewerPathPrefix = useViewerStore((state) => state.remapPathPrefix);
  const activeViewerPath = useViewerStore((state) => activeViewerTab(state)?.path ?? null);

  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const [bookmarksWidth, setBookmarksWidth] = useState(320);
  const [topicsWidth, setTopicsWidth] = useState(340);
  const [chatWidth, setChatWidth] = useState(320);
  const [fileFilterText, setFileFilterText] = useState("");
  const resizeRef = useRef<{
    startX: number;
    startWidth: number;
    setWidth: (width: number) => void;
    direction: 1 | -1;
    minWidth: number;
    maxWidth: number;
  } | null>(null);

  useEffect(() => {
    loadWorkspaces()
      .then(() => Promise.all([loadSettings(), loadBookmarks()]))
      .then(() => restoreViewerSession())
      .catch(console.error);
  }, [loadWorkspaces, loadSettings, loadBookmarks, restoreViewerSession]);

  useEffect(() => {
    setFileFilterText("");
  }, [directory]);

  useEffect(() => {
    if (!isTauri) return;

    let disposed = false;
    let unlisten: (() => void) | undefined;

    getCurrentWebview().onDragDropEvent(async (event) => {
      if (event.payload.type !== "drop") return;
      const paths = event.payload.paths;
      if (paths.length === 0) return;
      if (!directory) {
        addToast("Choose a directory before dropping files", { type: "error" });
        return;
      }
      if (readOnly) {
        addToast("This workspace is read-only", { type: "error" });
        return;
      }

      try {
        const imported = await (source as DesktopSourceApi).importFiles(paths, directory, "move");
        refreshFileList();
        addToast(`Imported ${imported.length} file${imported.length === 1 ? "" : "s"}`, {
          type: "success",
        });
      } catch (e) {
        console.error("Drop import failed:", e);
        addToast(e instanceof Error ? e.message : "Import failed", { type: "error" });
      }
    }).then((u) => {
      if (disposed) u();
      else unlisten = u;
    }).catch((e) => {
      console.error("Failed to subscribe to file drops:", e);
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [addToast, directory, readOnly, refreshFileList]);

  useEffect(() => {
    if (!isTauri) return;

    const handlePaste = async () => {
      try {
        const desktopSource = source as DesktopSourceApi;
        const paths = await desktopSource.readClipboardFiles();
        if (paths.length === 0) return;
        if (!directory) {
          addToast("Choose a directory before pasting files", { type: "error" });
          return;
        }
        if (readOnly) {
          addToast("This workspace is read-only", { type: "error" });
          return;
        }

        const imported = await desktopSource.importFiles(paths, directory, "copy");
        refreshFileList();
        addToast(`Imported ${imported.length} file${imported.length === 1 ? "" : "s"}`, {
          type: "success",
        });
      } catch (e) {
        console.error("Paste import failed:", e);
        addToast(e instanceof Error ? e.message : "Import failed", { type: "error" });
      }
    };

    window.addEventListener("paste", handlePaste);
    return () => window.removeEventListener("paste", handlePaste);
  }, [addToast, directory, readOnly, refreshFileList]);

  useEffect(() => {
    let mounted = true;
    const unlisteners: Array<() => void> = [];

    const setupSubscriptions = async () => {
      try {
        const u1 = await api.onEmbedProgress(() => {
          if (mounted) setIndexing(true);
        });
        if (mounted) unlisteners.push(u1);
        else u1();

        const u2 = await api.onEmbedDone(() => {
          if (mounted) {
            setIndexing(false);
            handleIndexUpdated().catch(console.error);
          }
        });
        if (mounted) unlisteners.push(u2);
        else u2();

        const u3 = await api.onEmbedError((err) => {
          if (mounted) {
            setIndexing(false);
            if (err.message) {
              addToast(`${err.operation} failed: ${err.message}`, { type: "error" });
            }
          }
        });
        if (mounted) unlisteners.push(u3);
        else u3();
      } catch (e) {
        console.error("Failed to subscribe to embed events:", e);
      }
    };

    setupSubscriptions();

    return () => {
      mounted = false;
      unlisteners.forEach((u) => u());
    };
  }, [handleIndexUpdated, setIndexing]);

  const handleMouseMove = useCallback((e: MouseEvent) => {
    const resize = resizeRef.current;
    if (!resize) return;
    const delta = (e.clientX - resize.startX) * resize.direction;
    const newWidth = Math.max(resize.minWidth, Math.min(resize.maxWidth, resize.startWidth + delta));
    resize.setWidth(newWidth);
  }, []);

  const handleMouseUp = useCallback(() => {
    resizeRef.current = null;
    document.removeEventListener("mousemove", handleMouseMove);
    document.removeEventListener("mouseup", handleMouseUp);
    document.body.style.cursor = "";
  }, [handleMouseMove]);

  const startResize = useCallback(
    ({
      width,
      setWidth,
      direction,
      minWidth = 200,
      maxWidth = window.innerWidth * 0.8,
    }: {
      width: number;
      setWidth: (width: number) => void;
      direction: 1 | -1;
      minWidth?: number;
      maxWidth?: number;
    }) =>
    (e: React.MouseEvent) => {
      e.preventDefault();
      resizeRef.current = {
        startX: e.clientX,
        startWidth: width,
        setWidth,
        direction,
        minWidth,
        maxWidth,
      };
      document.addEventListener("mousemove", handleMouseMove);
      document.addEventListener("mouseup", handleMouseUp);
      document.body.style.cursor = "col-resize";
    },
    [handleMouseMove, handleMouseUp],
  );

  useEffect(() => {
    return () => {
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };
  }, [handleMouseMove, handleMouseUp]);

  const handlePickDirectory = useCallback(async () => {
    const picked = await (source as DesktopSourceApi).pickDirectory();
    if (picked) setDirectory(picked);
  }, [setDirectory]);

  const handleRenameDirectory = useCallback(
    (oldPath: string, newPath: string) => {
      renameDirectory(oldPath, newPath);
      remapViewerPathPrefix(oldPath, newPath);
    },
    [renameDirectory, remapViewerPathPrefix],
  );

  const rootPicker = source.type === "desktop" ? (
      <DirectoryPicker
        directory={directory}
        favorites={favorites}
        recentDirs={recentDirs}
        onChange={setDirectory}
        onPickDirectory={handlePickDirectory}
        onFavoriteAdd={addFavorite}
        onFavoriteRemove={removeFavorite}
        onForgetDirectory={forgetDirectory}
        onRenameDirectory={handleRenameDirectory}
      />
    ) : (
      <UploadZone
        source={source as WebSourceApi}
        onRootChange={setDirectory}
      />
    );
  const sourceSlot = (
    <div className="flex min-w-0 flex-1 items-center gap-1">
      <WorkspacePicker />
      {rootPicker}
    </div>
  );

  const handleChatButtonClick = () => {
    if (chatPaneOpen) {
      toggleChatPane();
      return;
    }
    openChatPane().catch((e) => console.error("chat: open pane failed", e));
  };

  const settingsSlot = (
    <>
      {isTauri && (
        <div className="inline-flex rounded border border-[var(--border-main)] overflow-hidden">
          <Tooltip content="Ask the documents">
            <button
              onClick={handleChatButtonClick}
              aria-label="Ask the documents"
              aria-busy={chatPaneOpening}
              className={`w-[32px] h-[32px] flex items-center justify-center bg-[var(--bg-active)] transition-all active:scale-95 ${
                chatPaneOpen
                  ? "text-[var(--accent-blue)] shadow-inner"
                  : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
              }`}
            >
              {chatPaneOpening ? (
                <Loader size={14} className="animate-spin" />
              ) : (
                <MessageSquare size={14} fill={chatPaneOpen ? "currentColor" : "none"} />
              )}
            </button>
          </Tooltip>
          <Tooltip content="Choose chat agent">
            <button
              onClick={async (e) => {
                const event = e;
                await loadChatBackends().catch((error) => {
                  console.error("chat: failed to load backends", error);
                });
                const chatBackends = useChatStore.getState().backends;
                openChatBackendMenu({
                  event,
                  target: null,
                  size: "content",
                  items: chatBackends.map((b) => ({
                    id: b.backend,
                    label: `${b.available ? "●" : "○"} ${b.label}${
                      !b.available ? ` — ${b.unavailable_reason ?? b.auth_note}` : ""
                    }`,
                    disabled: !b.available,
                    run: () => openChatPane(b.backend as AgentBackend),
                  })),
                });
              }}
              aria-label="Choose chat agent"
              className="w-[14px] h-[32px] flex items-center justify-center bg-[var(--bg-active)] text-[var(--text-dim)] hover:text-[var(--text-main)] transition-all border-l border-[var(--border-main)]"
            >
              <ChevronDown size={10} />
            </button>
          </Tooltip>
        </div>
      )}
      <Tooltip content="Bookmarks">
        <button
          onClick={() => {
            if (bookmarksPaneOpen) {
              closeBookmarksPane();
            } else {
              openBookmarksPane(activeViewerPath ? "current" : "all");
            }
          }}
          aria-label="Bookmarks"
          className="w-[32px] h-[32px] flex items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)] transition-all border border-[var(--border-main)] hover:border-[var(--border-strong)]"
        >
          <Bookmark size={14} fill={bookmarksPaneOpen ? "currentColor" : "none"} />
        </button>
      </Tooltip>
      <Tooltip
        content={
          preferSemantic && semanticReadyForRoot
            ? "Chunk topic cloud"
            : "Build the semantic index to view the topic cloud"
        }
      >
        <button
          type="button"
          disabled={!directory || !preferSemantic || !semanticReadyForRoot}
          onClick={() => {
            if (topicsPaneOpen) closeTopicsPane();
            else openTopicsPane();
          }}
          aria-label="Chunk topic cloud"
          aria-pressed={topicsPaneOpen}
          className={`flex h-[32px] w-[32px] items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] transition-all disabled:opacity-40 ${
            topicsPaneOpen
              ? "text-[var(--accent-blue)] shadow-inner"
              : "text-[var(--text-muted)] hover:border-[var(--border-strong)] hover:text-[var(--text-main)]"
          }`}
        >
          <Cloud size={14} fill={topicsPaneOpen ? "currentColor" : "none"} />
        </button>
      </Tooltip>
      <Tooltip content="Settings">
        <button
          onClick={() => setSettingsOpen(true)}
          aria-label="Settings"
          className="w-[32px] h-[32px] flex items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)] transition-all border border-[var(--border-main)] hover:border-[var(--border-strong)]"
        >
          <SettingsIcon size={14} />
        </button>
      </Tooltip>
      <SettingsModal
        api={api}
        isOpen={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        directory={directory}
        refreshSemanticReady={refreshSemanticReady}
        onSettingsUpdate={applySettingsPatch}
      />
      <ContextMenu menu={chatBackendMenu} onClose={closeChatBackendMenu} />
    </>
  );

  const bookmarksColumn = bookmarksPaneOpen ? (
    <>
      {bookmarksDock === "Right" && (
        <div
          onMouseDown={startResize({
            width: bookmarksWidth,
            setWidth: setBookmarksWidth,
            direction: -1,
            minWidth: 240,
            maxWidth: window.innerWidth * 0.5,
          })}
          className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-l border-[var(--border-main)] transition-colors"
        />
      )}
      <div
        className="flex-shrink-0 overflow-hidden"
        style={{ width: `${bookmarksWidth}px`, minWidth: "240px" }}
      >
        <BookmarksPane />
      </div>
      {bookmarksDock === "Left" && (
        <div
          onMouseDown={startResize({
            width: bookmarksWidth,
            setWidth: setBookmarksWidth,
            direction: 1,
            minWidth: 240,
            maxWidth: window.innerWidth * 0.5,
          })}
          className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-r border-[var(--border-main)] transition-colors"
        />
      )}
    </>
  ) : null;

  const topicsColumn = topicsPaneOpen ? (
    <>
      {bookmarksDock === "Right" && (
        <div
          onMouseDown={startResize({
            width: topicsWidth,
            setWidth: setTopicsWidth,
            direction: -1,
            minWidth: 260,
            maxWidth: window.innerWidth * 0.55,
          })}
          className="w-1 cursor-col-resize flex-shrink-0 border-l border-[var(--border-main)] bg-transparent transition-colors hover:bg-[var(--accent-blue)]/30"
        />
      )}
      <div
        className="flex-shrink-0 overflow-hidden"
        style={{ width: `${topicsWidth}px`, minWidth: "260px" }}
      >
        <TopicCloudPane />
      </div>
      {bookmarksDock === "Left" && (
        <div
          onMouseDown={startResize({
            width: topicsWidth,
            setWidth: setTopicsWidth,
            direction: 1,
            minWidth: 260,
            maxWidth: window.innerWidth * 0.55,
          })}
          className="w-1 cursor-col-resize flex-shrink-0 border-r border-[var(--border-main)] bg-transparent transition-colors hover:bg-[var(--accent-blue)]/30"
        />
      )}
    </>
  ) : null;

  // Chat is right-dock-only for v1 (bookmarks already covers left, spec §7.1).
  const chatColumn = isTauri && chatPaneOpen ? (
    <>
      <div
        onMouseDown={startResize({
          width: chatWidth,
          setWidth: setChatWidth,
          direction: -1,
          minWidth: 320,
          maxWidth: window.innerWidth * 0.5,
        })}
        className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-l border-[var(--border-main)] transition-colors"
      />
      <div
        className="flex-shrink-0 overflow-hidden"
        style={{ width: `${chatWidth}px`, minWidth: "320px" }}
      >
        <ChatPane onClose={toggleChatPane} />
      </div>
    </>
  ) : null;

  return (
    <div
      aria-busy={workspaceSwitching}
      className="relative flex flex-col h-screen min-h-0 overflow-hidden bg-[var(--bg-app)] text-[var(--text-main)]"
    >
      {workspaceSwitching && (
        <div
          aria-label="Switching workspace"
          className="absolute inset-0 z-[100] cursor-wait"
        />
      )}
      <SearchBar sourceSlot={sourceSlot} settingsSlot={settingsSlot} />

      <div className="flex flex-1 min-h-0 overflow-hidden">
        <div
          className="flex-shrink-0 flex flex-col min-h-0 bg-[var(--bg-sidebar)]"
          style={{ width: `${sidebarWidth}px`, minWidth: "200px" }}
        >
          <ResultList
            filterText={fileFilterText}
            onFilterTextChange={setFileFilterText}
            onMatchClick={openMatch}
            onFileClick={openFile}
          />
        </div>

        <div
          onMouseDown={startResize({
            width: sidebarWidth,
            setWidth: setSidebarWidth,
            direction: 1,
            minWidth: 200,
            maxWidth: window.innerWidth * 0.45,
          })}
          className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-l border-[var(--border-main)] transition-colors"
        />

        {bookmarksDock === "Left" && bookmarksColumn}
        {bookmarksDock === "Left" && topicsColumn}

        <div className="flex-1 min-h-0 min-w-0 overflow-hidden bg-[var(--bg-app)]">
          <PreviewPane />
        </div>

        {bookmarksDock === "Right" && bookmarksColumn}
        {bookmarksDock === "Right" && topicsColumn}
        {chatColumn}
      </div>
    </div>
  );
}
