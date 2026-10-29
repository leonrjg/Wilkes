import { useState, useCallback, useRef, useEffect, type ReactNode } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { BookOpen, Bookmark, Cloud, Globe, MessageSquare, Settings as SettingsIcon, ChevronDown, ChevronLeft, ChevronRight, Loader, Plus } from "react-feather";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import BookmarksPane from "./components/BookmarksPane";
import ChatPane from "./components/ChatPane";
import TopicCloudPane from "./components/TopicCloudPane";
import DirectoryPicker from "./components/DirectoryPicker";
import WorkspacePicker from "./components/WorkspacePicker";
import UploadZone from "./components/UploadZone";
import CataloguePane from "./components/CataloguePane";
import BrowserPane from "./components/BrowserPane";
import SettingsModal from "./components/SettingsModal";
import { useToasts } from "./components/Toast";
import { useContextMenu, ContextMenu } from "./components/ContextMenu";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useBookmarksStore } from "./stores/useBookmarksStore";
import { useSavedWorksStore } from "./stores/useSavedWorksStore";
import { useChatStore } from "./stores/useChatStore";
import { useSemanticStore } from "./stores/useSemanticStore";
import { useTopicsStore } from "./stores/useTopicsStore";
import { useCatalogueStore } from "./stores/useCatalogueStore";
import { useBrowserStore } from "./stores/useBrowserStore";
import {
  useActiveWorkspaceName,
  useActiveWorkspaceReadOnly,
  useWorkspaceStore,
} from "./stores/useWorkspaceStore";
import { activeViewerTab, useViewerStore } from "./stores/useViewerStore";
import { useGlobalEvents } from "./hooks/useGlobalEvents";
import { useNativeOpen } from "./hooks/useNativeOpen";
import { useWindowTitle } from "./hooks/useWindowTitle";
import { pathIsWithinRoot } from "./lib/configuredRoots";
import { arrangePanes, movePane } from "./lib/paneOrder";
import { relativeFolderPath, type FileTreeHandle } from "./components/FileTree";
import { api, source, isTauri } from "./services";
import type { AgentBackend, BookmarkDock, NativeOpenRequest } from "./lib/types";
import type { DesktopSourceApi, PathKind, WebSourceApi } from "./services/api";

/** A pane docked beside the reader, as the dock lays it out. */
interface DockedPane {
  /** Names the pane in the arrangement the user makes. */
  key: string;
  /** What the pane is, for its grip's label. */
  label: string;
  side: BookmarkDock;
  /** The width it opens at, and the narrowest it can be made. */
  minWidth: number;
  maxWidth: number;
  focused?: boolean;
  marked?: boolean;
  onFocus?: () => void;
  children: ReactNode;
}

export default function App() {
  useGlobalEvents();
  const { addToast } = useToasts();

  const loadSettings = useSettingsStore((s) => s.load);
  const loadWorkspaces = useWorkspaceStore((s) => s.load);
  const workspaceSwitching = useWorkspaceStore((s) => s.switching);
  const readOnly = useActiveWorkspaceReadOnly();
  // The folder tree answers where a drag from outside the application is
  // pointing, so a dropped file lands in the folder it was dropped on.
  const fileTreeRef = useRef<FileTreeHandle>(null);
  const loadBookmarks = useBookmarksStore((s) => s.load);
  const loadSavedWorks = useSavedWorksStore((s) => s.load);
  const openBookmarksPane = useBookmarksStore((s) => s.openPane);
  const closeBookmarksPane = useBookmarksStore((s) => s.closePane);
  const bookmarksPaneOpen = useBookmarksStore((s) => s.paneOpen);
  const bookmarksDock = useSettingsStore((s) => s.bookmarksDock);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const setDirectory = useSettingsStore((s) => s.setDirectory);
  const addRoots = useSettingsStore((s) => s.addRoots);
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
  const cataloguePanesVisible = useCatalogueStore((s) => s.panesVisible);
  const cataloguePanes = useCatalogueStore((s) => s.panes);
  const focusedCataloguePane = useCatalogueStore((s) => s.focusedPane);
  const showCataloguePanes = useCatalogueStore((s) => s.showPanes);
  const hideCataloguePanes = useCatalogueStore((s) => s.hidePanes);
  const newCataloguePane = useCatalogueStore((s) => s.newPane);
  const focusCataloguePane = useCatalogueStore((s) => s.focusPane);
  const topicsPaneOpen = useTopicsStore((s) => s.paneOpen);
  const openTopicsPane = useTopicsStore((s) => s.openPane);
  const closeTopicsPane = useTopicsStore((s) => s.closePane);

  const chatPanesVisible = useChatStore((s) => s.panesVisible);
  const chatPanes = useChatStore((s) => s.panes);
  const focusedChatPane = useChatStore((s) => s.focusedPane);
  const chatPaneOpening = useChatStore((s) => s.panes.some((pane) => pane.opening));
  const showChatPanes = useChatStore((s) => s.showPanes);
  const hideChatPanes = useChatStore((s) => s.hidePanes);
  const newChatPane = useChatStore((s) => s.newPane);
  const closeChatPane = useChatStore((s) => s.closePane);
  const focusChatPane = useChatStore((s) => s.focusPane);
  const loadChatBackends = useChatStore((s) => s.loadBackends);
  const { menu: chatBackendMenu, openMenu: openChatBackendMenu, closeMenu: closeChatBackendMenu } =
    useContextMenu<null>();
  const { menu: catalogueMenu, openMenu: openCatalogueMenu, closeMenu: closeCatalogueMenu } =
    useContextMenu<null>();

  const openMatch = useViewerStore((state) => state.openMatch);
  const openFile = useViewerStore((state) => state.openFile);
  const restoreViewerSession = useViewerStore((state) => state.restoreSession);
  const remapViewerPathPrefix = useViewerStore((state) => state.remapPathPrefix);
  const activeViewerPath = useViewerStore((state) => activeViewerTab(state)?.path ?? null);
  const workspaceName = useActiveWorkspaceName();

  useWindowTitle({ workspace: workspaceName, root: directory, document: activeViewerPath });

  const [settingsOpen, setSettingsOpen] = useState(false);
  const [workspaceLoaded, setWorkspaceLoaded] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  // The widths the user dragged the docked panes to, by pane key. A pane not
  // named here opens at its minimum width, and resizing one does not resize
  // the others.
  const [paneWidths, setPaneWidths] = useState<Record<string, number>>({});
  const browserPaneOpen = useBrowserStore((s) => s.paneOpen);
  const toggleBrowserPane = useBrowserStore((s) => s.togglePane);
  // The arrangement of the docked panes the user last made, as pane keys.
  const [paneOrder, setPaneOrder] = useState<string[]>([]);
  // The pane being dragged by its grip, and the line showing where it would
  // land — none while it would land where it is.
  const [paneDrag, setPaneDrag] = useState<{
    key: string;
    line: { left: number; top: number; height: number } | null;
  } | null>(null);
  const dockRef = useRef<HTMLDivElement>(null);
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
      .then(() => Promise.all([loadSettings(), loadBookmarks(), loadSavedWorks()]))
      .then(() => restoreViewerSession())
      .catch(console.error)
      // Even a failed load ends the wait. A link held back forever is the one
      // outcome with nothing to show for it; a link answered against an empty
      // workspace list at least says which library it could not find.
      .finally(() => setWorkspaceLoaded(true));
  }, [loadWorkspaces, loadSettings, loadBookmarks, loadSavedWorks, restoreViewerSession]);

  /**
   * A `wilkes://` link that named a workspace, opened here as a click in the
   * file list would have opened it: the workspace it belongs to becomes the
   * active one, its root becomes the visible root, and the document is shown
   * at the place the link asked for.
   *
   * The link has to name a document this workspace actually serves. A path
   * outside every root of it is not a document the main window can show
   * coherently — the file list would not contain it and search would not
   * reach it — so it is refused here rather than opened into a session that
   * disagrees with the sidebar beside it.
   */
  const openFromLink = useCallback(
    (request: NativeOpenRequest) => {
      for (const error of request.errors) addToast(error, { type: "error" });
      const path = request.paths[0];
      if (!path) return;
      if (!request.workspace) {
        // The host routes a request naming no workspace to the standalone
        // reader, so one arriving here has lost its address on the way.
        console.error("An open request with no workspace reached the main window:", request);
        addToast("Could not tell which library that link meant", { type: "error" });
        return;
      }
      const named = request.workspace;
      const { workspaces, activeWorkspaceId, switchTo } = useWorkspaceStore.getState();
      const target =
        workspaces.find((workspace) => workspace.id === named) ??
        workspaces.find((workspace) => workspace.name.toLowerCase() === named.toLowerCase());
      if (!target) {
        addToast(`No library here is called "${named}"`, { type: "error" });
        return;
      }
      const root = target.roots.find((candidate) => pathIsWithinRoot(path, candidate));
      if (!root) {
        addToast(`${path} is not in the "${target.name}" library`, { type: "error" });
        return;
      }
      void (async () => {
        try {
          if (target.id !== activeWorkspaceId) await switchTo(target.id);
        } catch (error) {
          console.error("Could not switch to the workspace a link named:", error);
          addToast(`Could not open the "${target.name}" library`, { type: "error" });
          return;
        }
        setDirectory(root);
        openFile(path, request.origin);
      })();
    },
    [addToast, openFile, setDirectory],
  );

  useNativeOpen(
    isTauri && workspaceLoaded,
    openFromLink,
    useCallback((message: string) => addToast(message, { type: "error" }), [addToast]),
  );

  useEffect(() => {
    setFileFilterText("");
  }, [directory]);

  useEffect(() => {
    if (!isTauri) return;

    let disposed = false;
    let unlisten: (() => void) | undefined;

    getCurrentWebview().onDragDropEvent(async (event) => {
      const payload = event.payload;
      if (payload.type === "leave") {
        fileTreeRef.current?.externalDragEnd();
        return;
      }
      // The shell reports the drag in physical pixels from the window's own
      // origin; the tree hit-tests in CSS pixels from the same origin.
      const x = payload.position.x / window.devicePixelRatio;
      const y = payload.position.y / window.devicePixelRatio;
      if (payload.type !== "drop") {
        // "enter" and "over": show the folder the files would land in.
        fileTreeRef.current?.externalDragOver(x, y);
        return;
      }
      // The folder the drop was released on, decided by the same hit test that
      // drew the highlight, so the files land where the highlight said. Null
      // when the drop was not on a folder — outside the tree, or on its empty
      // space, both of which mean the root.
      const droppedOn = fileTreeRef.current?.externalDragOver(x, y) ?? null;
      fileTreeRef.current?.externalDragEnd();
      const paths = payload.paths;
      if (paths.length === 0) return;
      // Roots and imports both write through the workspace manifest, which the
      // backend refuses to rewrite for a workspace another application owns.
      if (readOnly) {
        addToast("This workspace is read-only", { type: "error" });
        return;
      }

      const desktopSource = source as DesktopSourceApi;
      let kinds: PathKind[];
      try {
        kinds = await desktopSource.pathKinds(paths);
      } catch (e) {
        console.error("Could not inspect dropped paths:", e);
        addToast(e instanceof Error ? e.message : "Could not read dropped items", {
          type: "error",
        });
        return;
      }

      const folders = paths.filter((_, i) => kinds[i] === "directory");
      const files = paths.filter((_, i) => kinds[i] === "file");

      // Files land in the root that was active when the drop happened — in the
      // folder of it they were dropped on — and never in a folder the same drop
      // is adding. The backend admits an import only into the current root, so
      // the import has to run before the dropped folders are registered and one
      // of them becomes active.
      if (files.length > 0) {
        if (!directory) {
          addToast("Choose a directory before dropping files", { type: "error" });
        } else {
          try {
            const folder = droppedOn ? relativeFolderPath(directory, droppedOn) : null;
            const imported = await desktopSource.importFiles(
              files,
              directory,
              "move",
              folder ?? undefined,
            );
            refreshFileList();
            const count = `${imported.length} file${imported.length === 1 ? "" : "s"}`;
            addToast(folder ? `Imported ${count} into ${folder}` : `Imported ${count}`, {
              type: "success",
            });
          } catch (e) {
            console.error("Drop import failed:", e);
            addToast(e instanceof Error ? e.message : "Import failed", { type: "error" });
          }
        }
      }

      if (folders.length > 0) {
        addRoots(folders);
        addToast(
          folders.length === 1
            ? `Added "${folders[0].split(/[/\\]/).pop() || folders[0]}" as a root`
            : `Added ${folders.length} roots`,
          { type: "success" },
        );
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
  }, [addRoots, addToast, directory, readOnly, refreshFileList]);

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
    useBrowserStore.getState().setHeld(false);
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
      // The browser pane would take the pointer from the drag as it crossed it.
      useBrowserStore.getState().setHeld(true);
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

  const chatPanesShown = chatPanesVisible && chatPanes.length > 0;
  const cataloguePanesShown = cataloguePanesVisible && cataloguePanes.length > 0;

  const handleChatButtonClick = () => {
    if (chatPanesShown) {
      hideChatPanes();
      return;
    }
    showChatPanes().catch((e) => console.error("chat: open pane failed", e));
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
                chatPanesShown
                  ? "text-[var(--accent-blue)] shadow-inner"
                  : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
              }`}
            >
              {chatPaneOpening ? (
                <Loader size={14} className="animate-spin" />
              ) : (
                <MessageSquare size={14} fill={chatPanesShown ? "currentColor" : "none"} />
              )}
            </button>
          </Tooltip>
          <Tooltip content="Choose chat agent or open another chat">
            <button
              onClick={async (e) => {
                const event = e;
                await loadChatBackends().catch((error) => {
                  console.error("chat: failed to load backends", error);
                });
                const chatBackends = useChatStore.getState().backends;
                // With a chat already showing, an agent picked here opens a
                // chat of its own beside it: the open one switches agents from
                // its own selector.
                const open = (backend?: AgentBackend) =>
                  (chatPanesShown ? newChatPane(backend) : showChatPanes(backend)).catch((error) =>
                    console.error("chat: open pane failed", error),
                  );
                openChatBackendMenu({
                  event,
                  target: null,
                  size: "content",
                  items: [
                    ...(chatPanesShown
                      ? [{ id: "new-chat-pane", label: "New chat pane", icon: Plus, run: () => open() }]
                      : []),
                    ...chatBackends.map((b, index) => ({
                      id: b.backend,
                      label: `${b.available ? "●" : "○"} ${b.label}${
                        !b.available ? ` — ${b.unavailable_reason ?? b.auth_note}` : ""
                      }`,
                      disabled: !b.available,
                      dividerBefore: chatPanesShown && index === 0,
                      run: () => open(b.backend as AgentBackend),
                    })),
                  ],
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
      <div className="inline-flex rounded border border-[var(--border-main)] overflow-hidden">
        <Tooltip content="Catalogues & literature">
          <button
            type="button"
            onClick={() => {
              if (cataloguePanesShown) hideCataloguePanes();
              else showCataloguePanes();
            }}
            aria-label="Catalogues and literature"
            aria-pressed={cataloguePanesShown}
            className={`w-[32px] h-[32px] flex items-center justify-center bg-[var(--bg-active)] transition-all ${
              cataloguePanesShown
                ? "text-[var(--accent-blue)] shadow-inner"
                : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
            }`}
          >
            <BookOpen size={14} />
          </button>
        </Tooltip>
        <Tooltip content="Open another literature pane">
          <button
            type="button"
            onClick={(event) =>
              openCatalogueMenu({
                event,
                target: null,
                size: "content",
                items: [
                  { id: "new-literature-pane", label: "New literature pane", icon: Plus, run: newCataloguePane },
                ],
              })
            }
            aria-label="Literature pane options"
            className="w-[14px] h-[32px] flex items-center justify-center bg-[var(--bg-active)] text-[var(--text-dim)] hover:text-[var(--text-main)] transition-all border-l border-[var(--border-main)]"
          >
            <ChevronDown size={10} />
          </button>
        </Tooltip>
      </div>
      {isTauri && (
        <Tooltip content="Browser">
          <button
            onClick={toggleBrowserPane}
            aria-label="Browser pane"
            aria-pressed={browserPaneOpen}
            className={`w-[32px] h-[32px] flex items-center justify-center rounded bg-[var(--bg-active)] transition-all border border-[var(--border-main)] hover:border-[var(--border-strong)] ${
              browserPaneOpen
                ? "text-[var(--accent-blue)] shadow-inner"
                : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
            }`}
          >
            <Globe size={14} />
          </button>
        </Tooltip>
      )}
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
      <ContextMenu menu={catalogueMenu} onClose={closeCatalogueMenu} />
    </>
  );

  // Every pane docked beside the reader, in its default order: the order it
  // takes until the user moves one.
  const dockedPanes: DockedPane[] = [];
  if (bookmarksPaneOpen) {
    dockedPanes.push({
      key: "bookmarks",
      label: "bookmarks",
      side: bookmarksDock,
      minWidth: 240,
      maxWidth: window.innerWidth * 0.5,
      children: <BookmarksPane />,
    });
  }
  if (topicsPaneOpen) {
    dockedPanes.push({
      key: "topics",
      label: "topics",
      side: bookmarksDock,
      minWidth: 260,
      maxWidth: window.innerWidth * 0.55,
      children: <TopicCloudPane />,
    });
  }
  // Right-dock only, like chat: the left dock is bookmarks' and the pane is a
  // companion to the file list, not a second reading column.
  if (cataloguePanesVisible) {
    for (const pane of cataloguePanes) {
      dockedPanes.push({
        key: `catalogue:${pane.id}`,
        label: "literature",
        side: "Right",
        minWidth: 300,
        maxWidth: window.innerWidth * 0.5,
        focused: pane.id === focusedCataloguePane,
        marked: cataloguePanes.length > 1,
        onFocus: () => focusCataloguePane(pane.id),
        children: <CataloguePane pane={pane} />,
      });
    }
  }
  // Chat is right-dock-only for v1 (bookmarks already covers left, spec §7.1).
  if (isTauri && chatPanesVisible) {
    for (const pane of chatPanes) {
      dockedPanes.push({
        key: `chat:${pane.id}`,
        label: "chat",
        side: "Right",
        minWidth: 320,
        maxWidth: window.innerWidth * 0.5,
        focused: pane.id === focusedChatPane,
        marked: chatPanes.length > 1,
        onFocus: () => focusChatPane(pane.id),
        children: <ChatPane pane={pane} onClose={() => closeChatPane(pane.id)} />,
      });
    }
  }
  if (isTauri && browserPaneOpen) {
    dockedPanes.push({
      key: "browser",
      label: "browser",
      side: "Right",
      minWidth: 320,
      maxWidth: window.innerWidth * 0.5,
      children: <BrowserPane />,
    });
  }

  /** Each side's panes in the order the user arranged them. */
  const arrangedPanes = (side: BookmarkDock) =>
    arrangePanes(dockedPanes.filter((pane) => pane.side === side), paneOrder);

  /** Records `key` at `index` among `sideKeys`, the panes of its side as they
   *  stand. The rest of the arrangement — the other side, and panes closed
   *  since they were placed — keeps its places. */
  const placePane = (sideKeys: string[], key: string, index: number) => {
    const moved = movePane(sideKeys, key, index);
    setPaneOrder((order) => [...order.filter((other) => !moved.includes(other)), ...moved]);
  };

  /** Where a pane dragged to `x` would land among its side's panes, and the
   *  line that shows it; null where it would land where it already is. Asked
   *  of the panes as they are laid out, since that is what the user sees. */
  const paneLanding = (side: BookmarkDock, key: string, x: number) => {
    const panes = Array.from(
      dockRef.current?.querySelectorAll<HTMLElement>(`[data-dock-side="${side}"] [data-docked-pane]`) ?? [],
    )
      .map((element) => ({ key: element.dataset.dockedPane!, rect: element.getBoundingClientRect() }))
      .sort((a, b) => a.rect.left - b.rect.left);
    const keys = panes.map((pane) => pane.key);
    const from = keys.indexOf(key);
    const others = panes.filter((pane) => pane.key !== key);
    if (from < 0 || others.length === 0) return null;
    const index = others.filter((pane) => pane.rect.left + pane.rect.width / 2 < x).length;
    if (index === from) return null;
    const edge = index < others.length ? others[index].rect.left : others[others.length - 1].rect.right;
    const { top, height } = panes[from].rect;
    return { keys, index, line: { left: edge, top, height } };
  };

  /** Drag a pane by its grip to another place among its side's panes. */
  const startPaneDrag = (side: BookmarkDock, key: string) => (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0 || !event.isPrimary) return;
    event.preventDefault();
    const grip = event.currentTarget;
    const pointerId = event.pointerId;
    const startX = event.clientX;
    const startY = event.clientY;
    let active = false;
    let shown: number | null = null;

    const aim = (x: number) => {
      const landing = paneLanding(side, key, x);
      shown = landing?.index ?? null;
      setPaneDrag({ key, line: landing?.line ?? null });
    };
    const finish = () => {
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", release, true);
      window.removeEventListener("pointercancel", cancelPointer, true);
      window.removeEventListener("keydown", keydown, true);
      window.removeEventListener("blur", finish);
      grip.removeEventListener("lostpointercapture", cancelPointer);
      if (grip.hasPointerCapture(pointerId)) grip.releasePointerCapture(pointerId);
      setPaneDrag(null);
      if (active) {
        document.body.style.cursor = "";
        useBrowserStore.getState().setHeld(false);
      }
    };
    const cancelPointer = (e: PointerEvent) => {
      if (e.pointerId === pointerId) finish();
    };
    const keydown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      }
    };
    const move = (e: PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      if ((e.buttons & 1) === 0) {
        finish();
        return;
      }
      if (!active && Math.hypot(e.clientX - startX, e.clientY - startY) < 4) return;
      e.preventDefault();
      if (!active) {
        active = true;
        document.body.style.cursor = "grabbing";
        // The browser pane would take the pointer from the drag as it crossed it.
        useBrowserStore.getState().setHeld(true);
      }
      aim(e.clientX);
    };
    const release = (e: PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      // Only where the line was shown: a layout that moved under the pointer
      // since cancels rather than surprises.
      const landing = active ? paneLanding(side, key, e.clientX) : null;
      const accepted = landing !== null && landing.index === shown;
      finish();
      if (accepted) placePane(landing.keys, key, landing.index);
    };

    grip.setPointerCapture(pointerId);
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", release, true);
    window.addEventListener("pointercancel", cancelPointer, true);
    window.addEventListener("keydown", keydown, true);
    window.addEventListener("blur", finish);
    grip.addEventListener("lostpointercapture", cancelPointer);
  };

  /** One side's docked panes, each with its grip and resize handle. The panes
   *  stay where they are in the document and are placed by their flex order,
   *  so a move does not take a pane out of the page and put it back — which
   *  would lose its focus and scroll position. A pointer or focus inside a
   *  pane makes it the pane in use, which is where a request from elsewhere —
   *  "Ask about this file", a search of the catalogues — is sent. With more
   *  than one of a kind open, the pane in use is marked. */
  const dockSide = (side: BookmarkDock) => {
    const arranged = arrangedPanes(side);
    if (arranged.length === 0) return null;
    const keys = arranged.map((pane) => pane.key);
    const widthOf = (pane: DockedPane) => paneWidths[pane.key] ?? pane.minWidth;
    const handle = (pane: DockedPane) => (
      <div
        onMouseDown={startResize({
          width: widthOf(pane),
          setWidth: (width) => setPaneWidths((widths) => ({ ...widths, [pane.key]: width })),
          direction: side === "Right" ? -1 : 1,
          minWidth: pane.minWidth,
          maxWidth: pane.maxWidth,
        })}
        className={`w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 ${
          side === "Right" ? "border-l" : "border-r"
        } border-[var(--border-main)] transition-colors`}
      />
    );
    return (
      <div data-dock-side={side} className="flex min-h-0 flex-shrink-0">
        {dockedPanes
          .filter((pane) => pane.side === side)
          .map((pane) => {
            const at = keys.indexOf(pane.key);
            return (
              <div
                key={pane.key}
                data-docked-pane={pane.key}
                className={`flex min-h-0 flex-shrink-0 ${paneDrag?.key === pane.key ? "opacity-50" : ""}`}
                style={{ order: at }}
              >
                {side === "Right" && handle(pane)}
                <div
                  onPointerDownCapture={pane.onFocus}
                  onFocusCapture={pane.onFocus}
                  className="relative flex flex-shrink-0 flex-col overflow-hidden"
                  style={{ width: `${widthOf(pane)}px`, minWidth: `${pane.minWidth}px` }}
                >
                  <Tooltip content="Drag to move this pane">
                    <button
                      type="button"
                      aria-label={`Move ${pane.label} pane`}
                      onPointerDown={startPaneDrag(side, pane.key)}
                      onKeyDown={(event) => {
                        const step = event.key === "ArrowLeft" ? -1 : event.key === "ArrowRight" ? 1 : 0;
                        if (step === 0) return;
                        event.preventDefault();
                        placePane(keys, pane.key, at + step);
                      }}
                      style={{ touchAction: "none" }}
                      className="group flex h-2.5 w-full shrink-0 cursor-grab items-center justify-center bg-[var(--bg-sidebar)] focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-[var(--accent-blue)]"
                    >
                      <span className="h-[3px] w-8 rounded-full bg-[var(--border-strong)] transition-colors group-hover:bg-[var(--text-dim)]" />
                    </button>
                  </Tooltip>
                  {pane.marked && pane.focused && (
                    <div
                      aria-hidden="true"
                      className="pointer-events-none absolute inset-x-0 top-0 z-10 h-0.5 bg-[var(--accent-blue)]"
                    />
                  )}
                  <div className="min-h-0 flex-1">{pane.children}</div>
                </div>
                {side === "Left" && handle(pane)}
              </div>
            );
          })}
      </div>
    );
  };

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
      {paneDrag?.line && (
        <div
          aria-hidden="true"
          data-testid="pane-drop-line"
          className="pointer-events-none fixed z-50 w-0.5 -translate-x-1/2 bg-[var(--accent-blue)]"
          style={{
            left: `${paneDrag.line.left}px`,
            top: `${paneDrag.line.top}px`,
            height: `${paneDrag.line.height}px`,
          }}
        />
      )}

      <div ref={dockRef} className="flex flex-1 min-h-0 overflow-hidden">
        <div
          id="file-sidebar"
          className={sidebarCollapsed ? "hidden" : "flex-shrink-0 flex flex-col min-h-0 bg-[var(--bg-sidebar)]"}
          style={{ width: `${sidebarWidth}px`, minWidth: "200px" }}
        >
          <ResultList
            fileTreeRef={fileTreeRef}
            filterText={fileFilterText}
            onFilterTextChange={setFileFilterText}
            onMatchClick={openMatch}
            onFileClick={openFile}
          />
        </div>

        {!sidebarCollapsed && (
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
        )}

        <div className="relative w-0 shrink-0">
          <button
            type="button"
            aria-label={sidebarCollapsed ? "Show sidebar" : "Hide sidebar"}
            aria-expanded={!sidebarCollapsed}
            aria-controls="file-sidebar"
            onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
            className="absolute left-0 top-1/2 z-30 flex h-12 w-4 -translate-y-1/2 items-center justify-center rounded-r-md border border-l-0 border-[var(--border-strong)] bg-[var(--bg-sidebar)] text-[var(--text-muted)] shadow-sm transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--accent-blue)]"
          >
            {sidebarCollapsed ? <ChevronRight size={12} /> : <ChevronLeft size={12} />}
          </button>
        </div>

        {dockSide("Left")}

        <div className="flex-1 min-h-0 min-w-0 overflow-hidden bg-[var(--bg-app)]">
          <PreviewPane />
        </div>

        {dockSide("Right")}
      </div>
    </div>
  );
}
