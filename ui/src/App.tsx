import { useState, useCallback, useRef, useEffect } from "react";
import { Settings as SettingsIcon } from "react-feather";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import DirectoryPicker from "./components/DirectoryPicker";
import UploadZone from "./components/UploadZone";
import SettingsModal from "./components/SettingsModal";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useHistory } from "./hooks/useHistory";
import { useTauriEvents } from "./hooks/useTauriEvents";
import { api, source, isTauri } from "./services";
import type { DesktopSourceApi, WebSourceApi } from "./services/api";

export default function App() {
  useTauriEvents();

  const loadSettings = useSettingsStore((s) => s.load);
  const directory = useSettingsStore((s) => s.directory);
  const bookmarks = useSettingsStore((s) => s.bookmarks);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const setDirectory = useSettingsStore((s) => s.setDirectory);
  const addBookmark = useSettingsStore((s) => s.addBookmark);
  const removeBookmark = useSettingsStore((s) => s.removeBookmark);
  const refreshSemanticReady = useSettingsStore((s) => s.refreshSemanticReady);
  const applySettingsPatch = useSettingsStore((s) => s.applySettingsPatch);
  const setIndexing = useSettingsStore((s) => s.setIndexing);

  const { canGoBack, canGoForward, goBack, goForward, handleMatchClick, handleFileClick } =
    useHistory();

  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const isResizing = useRef(false);

  useEffect(() => {
    loadSettings().catch(() => {});
  }, [loadSettings]);

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
          if (mounted) setIndexing(false);
        });
        if (mounted) unlisteners.push(u2);
        else u2();

        const u3 = await api.onEmbedError(() => {
          if (mounted) setIndexing(false);
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
  }, [setIndexing]);

  const handleMouseMove = useCallback((e: MouseEvent) => {
    if (!isResizing.current) return;
    const newWidth = Math.max(200, Math.min(window.innerWidth * 0.8, e.clientX));
    setSidebarWidth(newWidth);
  }, []);

  const handleMouseUp = useCallback(() => {
    isResizing.current = false;
    document.removeEventListener("mousemove", handleMouseMove);
    document.removeEventListener("mouseup", handleMouseUp);
    document.body.style.cursor = "";
  }, [handleMouseMove]);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      isResizing.current = true;
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
        bookmarks={bookmarks}
        recentDirs={recentDirs}
        onChange={setDirectory}
        onPickDirectory={handlePickDirectory}
        onBookmarkAdd={addBookmark}
        onBookmarkRemove={removeBookmark}
      />
    ) : (
      <UploadZone
        source={source as WebSourceApi}
        api={api}
        root={directory}
        onRootChange={setDirectory}
      />
    );

  const settingsSlot = isTauri ? (
    <>
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
          onMouseDown={handleMouseDown}
          className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-l border-[var(--border-main)] transition-colors"
        />

        <div className="flex-1 overflow-hidden bg-[var(--bg-app)]">
          <PreviewPane
            canGoBack={canGoBack}
            canGoForward={canGoForward}
            onGoBack={goBack}
            onGoForward={goForward}
          />
        </div>
      </div>
    </div>
  );
}
