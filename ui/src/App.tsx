import { useState, useCallback, useRef, useEffect } from "react";
import { Bookmark, Settings as SettingsIcon } from "react-feather";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import BookmarksPane from "./components/BookmarksPane";
import DirectoryPicker from "./components/DirectoryPicker";
import UploadZone from "./components/UploadZone";
import SettingsModal from "./components/SettingsModal";
import { useToasts } from "./components/Toast";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useBookmarksStore } from "./stores/useBookmarksStore";
import { useSemanticStore } from "./stores/useSemanticStore";
import { useHistory } from "./hooks/useHistory";
import { useGlobalEvents } from "./hooks/useGlobalEvents";
import { api, source } from "./services";
import type { DesktopSourceApi, WebSourceApi } from "./services/api";

export default function App() {
  useGlobalEvents();
  const { addToast } = useToasts();

  const loadSettings = useSettingsStore((s) => s.load);
  const loadBookmarks = useBookmarksStore((s) => s.load);
  const toggleBookmarksPane = useBookmarksStore((s) => s.togglePane);
  const bookmarksPaneOpen = useBookmarksStore((s) => s.paneOpen);
  const bookmarksDock = useSettingsStore((s) => s.bookmarksDock);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const setDirectory = useSettingsStore((s) => s.setDirectory);
  const addFavorite = useSettingsStore((s) => s.addFavorite);
  const removeFavorite = useSettingsStore((s) => s.removeFavorite);
  const forgetDirectory = useSettingsStore((s) => s.forgetDirectory);
  const applySettingsPatch = useSettingsStore((s) => s.applySettingsPatch);
  const setIndexing = useSettingsStore((s) => s.setIndexing);
  const refreshSemanticReady = useSemanticStore((s) => s.refreshCurrentRootStatus);
  const handleIndexUpdated = useSemanticStore((s) => s.handleIndexUpdated);

  const { canGoBack, canGoForward, goBack, goForward, handleMatchClick, handleFileClick } =
    useHistory();

  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const [bookmarksWidth, setBookmarksWidth] = useState(320);
  const resizeRef = useRef<{
    startX: number;
    startWidth: number;
    setWidth: (width: number) => void;
    direction: 1 | -1;
    minWidth: number;
    maxWidth: number;
  } | null>(null);

  useEffect(() => {
    loadSettings().catch(() => {});
    loadBookmarks().catch(() => {});
  }, [loadSettings, loadBookmarks]);

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

  const sourceSlot =
    source.type === "desktop" ? (
      <DirectoryPicker
        directory={directory}
        favorites={favorites}
        recentDirs={recentDirs}
        onChange={setDirectory}
        onPickDirectory={handlePickDirectory}
        onFavoriteAdd={addFavorite}
        onFavoriteRemove={removeFavorite}
        onForgetDirectory={forgetDirectory}
      />
    ) : (
      <UploadZone
        source={source as WebSourceApi}
        onRootChange={setDirectory}
      />
    );

  const settingsSlot = (
    <>
      <button
        onClick={toggleBookmarksPane}
        title="Bookmarks"
        className="w-[32px] h-[32px] flex items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)] transition-all border border-[var(--border-main)] hover:border-[var(--border-strong)]"
      >
        <Bookmark size={14} fill={bookmarksPaneOpen ? "currentColor" : "none"} />
      </button>
      <button
        onClick={() => setSettingsOpen(true)}
        title="Settings"
        className="w-[32px] h-[32px] flex items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)] transition-all border border-[var(--border-main)] hover:border-[var(--border-strong)]"
      >
        <SettingsIcon size={14} />
      </button>
      <SettingsModal
        api={api}
        isOpen={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        directory={directory}
        refreshSemanticReady={refreshSemanticReady}
        onSettingsUpdate={applySettingsPatch}
      />
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

  return (
    <div className="flex flex-col h-screen bg-[var(--bg-app)] text-[var(--text-main)]">
      <SearchBar sourceSlot={sourceSlot} settingsSlot={settingsSlot} />

      <div className="flex flex-1 overflow-hidden">
        <div
          className="flex-shrink-0 flex flex-col bg-[var(--bg-sidebar)]"
          style={{ width: `${sidebarWidth}px`, minWidth: "200px" }}
        >
          <ResultList onMatchClick={handleMatchClick} onFileClick={handleFileClick} />
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

        <div className="flex-1 overflow-hidden bg-[var(--bg-app)]">
          <PreviewPane
            canGoBack={canGoBack}
            canGoForward={canGoForward}
            onGoBack={goBack}
            onGoForward={goForward}
          />
        </div>

        {bookmarksDock === "Right" && bookmarksColumn}
      </div>
    </div>
  );
}
