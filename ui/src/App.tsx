import { useState, useCallback, useRef, useEffect, useTransition } from "react";
import { Settings as SettingsIcon } from "react-feather";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import DirectoryPicker from "./components/DirectoryPicker";
import UploadZone from "./components/UploadZone";
import { TauriSearchApi, TauriSourceApi } from "./services/tauri";
import { HttpSearchApi, HttpSourceApi } from "./services/http";
import SettingsModal from "./components/SettingsModal";
import { useToasts } from "./components/Toast";
import type { SearchApi, SourceApi, DesktopSourceApi, WebSourceApi } from "./services/api";
import type { FileEntry, FileMatches, MatchRef, PreviewData, SearchQuery, SearchStats, Theme } from "./lib/types";

const isTauri = "__TAURI_INTERNALS__" in window;

const api: SearchApi = isTauri ? new TauriSearchApi() : new HttpSearchApi();
const source: SourceApi = isTauri ? new TauriSourceApi() : new HttpSourceApi();

function useTheme() {
  const [theme, setTheme] = useState<Theme>("System");

  useEffect(() => {
    const applyTheme = (t: Theme) => {
      const root = window.document.documentElement;
      root.classList.remove("light", "dark");

      if (t === "Light") {
        root.classList.add("light");
      } else if (t === "Dark") {
        root.classList.add("dark");
      } else {
        // System
        const systemDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
        root.classList.add(systemDark ? "dark" : "light");
      }
    };

    applyTheme(theme);

    if (theme === "System") {
      const media = window.matchMedia("(prefers-color-scheme: dark)");
      const listener = () => applyTheme("System");
      media.addEventListener("change", listener);
      return () => media.removeEventListener("change", listener);
    }
  }, [theme]);

  return { theme, setTheme };
}

export default function App() {
  const { setTheme } = useTheme();
  const { addToast, removeToast } = useToasts();
  const [results, setResults] = useState<FileMatches[]>([]);
  const [stats, setStats] = useState<SearchStats | null>(null);
  const [searching, setSearching] = useState(false);
  const [hasQuery, setHasQuery] = useState(false);
  const [selectedMatch, setSelectedMatch] = useState<MatchRef | null>(null);
  const [previewData, setPreviewData] = useState<PreviewData | null>(null);
  const [previewPending, startPreviewTransition] = useTransition();
  const [history, setHistory] = useState<MatchRef[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const isNavigatingHistory = useRef(false);
  const currentSearchId = useRef<string | null>(null);
  const reindexToastId = useRef<string | null>(null);

  const [bookmarks, setBookmarks] = useState<string[]>([]);
  const [recentDirs, setRecentDirs] = useState<string[]>([]);
  const [directory, setDirectory] = useState<string>("");
  const [respectGitignore, setRespectGitignore] = useState(true);
  const [maxFileSize, setMaxFileSize] = useState(10 * 1024 * 1024);
  const [contextLines, setContextLines] = useState(2);
  const [supportedExtensions, setSupportedExtensions] = useState<string[]>([]);
  const [fileList, setFileList] = useState<FileEntry[]>([]);
  const [filterText, setFilterText] = useState("");
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [semanticIndexBuilt, setSemanticIndexBuilt] = useState(false);
  const [preferSemantic, setPreferSemantic] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(320);
  const isResizing = useRef(false);

  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    isResizing.current = true;
    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
    document.body.style.cursor = "col-resize";
  }, []);

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
  }, []);

  useEffect(() => {
    return () => {
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
    };
  }, [handleMouseMove, handleMouseUp]);

  useEffect(() => {
    if (!isTauri) return;
    
    let unlisten: (() => void) | undefined;
    let mounted = true;
    
    import("@tauri-apps/api/event").then(({ listen }) => {
      if (!mounted) return;
      listen<string>("manager-event", (event) => {
        if (event.payload === "WorkerStarting") {
          addToast("Starting worker... Next queries will be faster", "info");
        } else if (event.payload === "Reindexing") {
          if (!reindexToastId.current) {
            reindexToastId.current = addToast(
              "Reindexing... Semantic search is temporarily unavailable",
              "info",
              0
            );
          }
        } else if (event.payload === "ReindexingDone") {
          if (reindexToastId.current) {
            removeToast(reindexToastId.current);
            reindexToastId.current = null;
          }
        }
      }).then(u => {
        if (!mounted) {
          u();
        } else {
          unlisten = u;
        }
      });
    });

    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [addToast, removeToast]);

  useEffect(() => {
    api.getSettings().then((s) => {
      setBookmarks(s.bookmarked_dirs);
      setRecentDirs(s.recent_dirs || []);
      const dir = s.last_directory ?? "";
      setDirectory(dir);
      setRespectGitignore(s.respect_gitignore);
      setMaxFileSize(s.max_file_size);
      setContextLines(s.context_lines);
      setSupportedExtensions(s.supported_extensions || []);
      setSemanticIndexBuilt(s.semantic.enabled && s.semantic.index_path !== null);
      setPreferSemantic(s.search_prefer_semantic);
      setTheme(s.theme);
    }).catch(() => {});
  }, [setTheme]);

  useEffect(() => {
    if (!directory) return;
    api.listFiles(directory).then((files) => {
      setFileList(files);
      setExcluded(new Set());
    }).catch(() => {});
  }, [directory]);

  const handleDirectoryChange = useCallback((dir: string) => {
    setDirectory(dir);
    setFilterText("");
    setRecentDirs((prev) => {
      // Remove if already exists to move to front, and limit to 10
      const next = [dir, ...prev.filter((d) => d !== dir)].slice(0, 10);
      api.updateSettings({ last_directory: dir, recent_dirs: next }).catch(() => {});
      return next;
    });
  }, []);

  const handlePickDirectory = useCallback(async () => {
    const picked = await (source as DesktopSourceApi).pickDirectory();
    if (picked) handleDirectoryChange(picked);
  }, [handleDirectoryChange]);

  const handleBookmarkAdd = useCallback((dir: string) => {
    setBookmarks((prev) => {
      if (prev.includes(dir)) return prev;
      const next = [...prev, dir];
      api.updateSettings({ bookmarked_dirs: next }).catch(() => {});
      return next;
    });
  }, []);

  const handleBookmarkRemove = useCallback((dir: string) => {
    setBookmarks((prev) => {
      const next = prev.filter((b) => b !== dir);
      api.updateSettings({ bookmarked_dirs: next }).catch(() => {});
      return next;
    });
  }, []);

  const refreshSemanticReady = useCallback(async () => {
    if (!isTauri) return;
    try {
      const s = await api.getSettings();
      setSemanticIndexBuilt(s.semantic.enabled && s.semantic.index_path !== null);
    } catch (e) {
      console.error("getSettings failed in refreshSemanticReady:", e);
    }
  }, []);

  const handleSearch = useCallback(async (query: SearchQuery) => {
    if (currentSearchId.current) {
      await api.cancelSearch(currentSearchId.current).catch(() => {});
    }

    setResults([]);
    setStats(null);
    setSearching(true);
    setSelectedMatch(null);
    setPreviewData(null);

    try {
      const searchId = await api.search(
        query,
        (fm) => {
          setResults((prev) => [...prev, fm]);
        },
        (s) => {
          setStats(s);
          setSearching(false);
          currentSearchId.current = null;
        },
      );
      currentSearchId.current = searchId;
    } catch (e: any) {
      const msg = e?.toString() ?? "Search failed";
      console.error("Search failed:", e);
      setStats({ files_scanned: 0, total_matches: 0, elapsed_ms: 0, errors: [msg] });
      setSearching(false);
    }
  }, []);

  const addToHistory = useCallback((matchRef: MatchRef) => {
    if (isNavigatingHistory.current) return;
    setHistory((prev) => {
      const next = prev.slice(0, historyIndex + 1);
      // Don't add if it's the same as current
      if (next.length > 0 && 
          next[next.length - 1].path === matchRef.path && 
          JSON.stringify(next[next.length - 1].origin) === JSON.stringify(matchRef.origin)) {
        return prev;
      }
      return [...next, matchRef];
    });
    setHistoryIndex((prev) => prev + 1);
  }, [historyIndex]);

  const handleSelectMatchInternal = useCallback((matchRef: MatchRef) => {
    setSelectedMatch(matchRef);
    startPreviewTransition(async () => {
      try {
        const data = await api.preview(matchRef);
        setPreviewData(data);
      } catch (e) {
        console.error("Preview failed:", e);
        setPreviewData(null);
      }
    });
  }, []);

  const goBack = useCallback(() => {
    if (historyIndex > 0) {
      isNavigatingHistory.current = true;
      const nextIndex = historyIndex - 1;
      const matchRef = history[nextIndex];
      setHistoryIndex(nextIndex);
      handleSelectMatchInternal(matchRef);
      setTimeout(() => { isNavigatingHistory.current = false; }, 0);
    }
  }, [history, historyIndex, handleSelectMatchInternal]);

  const goForward = useCallback(() => {
    if (historyIndex < history.length - 1) {
      isNavigatingHistory.current = true;
      const nextIndex = historyIndex + 1;
      const matchRef = history[nextIndex];
      setHistoryIndex(nextIndex);
      handleSelectMatchInternal(matchRef);
      setTimeout(() => { isNavigatingHistory.current = false; }, 0);
    }
  }, [history, historyIndex, handleSelectMatchInternal]);

  const handleMatchClick = useCallback((matchRef: MatchRef) => {
    addToHistory(matchRef);
    handleSelectMatchInternal(matchRef);
  }, [addToHistory, handleSelectMatchInternal]);

  const handleFileClick = useCallback((path: string) => {
    const matchRef: MatchRef = { path, origin: { PdfPage: { page: 1, bbox: null } } };
    addToHistory(matchRef);
    handleSelectMatchInternal(matchRef);
  }, [addToHistory, handleSelectMatchInternal]);

  const sourceSlot = source.type === "desktop" ? (
    <DirectoryPicker
      directory={directory}
      bookmarks={bookmarks}
      recentDirs={recentDirs}
      onChange={handleDirectoryChange}
      onPickDirectory={handlePickDirectory}
      onBookmarkAdd={handleBookmarkAdd}
      onBookmarkRemove={handleBookmarkRemove}
    />
  ) : (
    <UploadZone
      source={source as WebSourceApi}
      api={api}
      root={directory}
      onRootChange={handleDirectoryChange}
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
        onSettingsUpdate={(patch) => {
          if (patch.theme) setTheme(patch.theme);
          if (patch.supported_extensions) setSupportedExtensions(patch.supported_extensions);
        }}
      />
    </>
  ) : null;

  const handleSemanticModeChange = useCallback((active: boolean) => {
    setPreferSemantic(active);
    api.updateSettings({ search_prefer_semantic: active }).catch(console.error);
  }, []);

  return (
    <div className="flex flex-col h-screen bg-[var(--bg-app)] text-[var(--text-main)]">
      <SearchBar
        onSearch={handleSearch}
        searching={searching}
        sourceSlot={sourceSlot}
        settingsSlot={settingsSlot}
        semanticReady={semanticIndexBuilt}
        directory={directory}
        respectGitignore={respectGitignore}
        maxFileSize={maxFileSize}
        contextLines={contextLines}
        supportedExtensions={supportedExtensions}
        fileList={fileList}
        excluded={excluded}
        onExcludedChange={setExcluded}
        onQueryChange={setHasQuery}
        initialSemanticMode={preferSemantic}
        onSemanticModeChange={handleSemanticModeChange}
      />

      <div className="flex flex-1 overflow-hidden">
        <div 
          className="flex-shrink-0 flex flex-col bg-[var(--bg-sidebar)]"
          style={{ width: `${sidebarWidth}px`, minWidth: "200px" }}
        >
          <ResultList
            results={results}
            stats={stats}
            searching={searching}
            hasQuery={hasQuery}
            fileList={fileList.filter((f) => !excluded.has(f.extension))}
            filterText={filterText}
            onFilterChange={setFilterText}
            onMatchClick={handleMatchClick}
            onFileClick={handleFileClick}
            selectedMatch={selectedMatch}
          />
        </div>

        <div
          onMouseDown={handleMouseDown}
          className="w-1 cursor-col-resize flex-shrink-0 bg-transparent hover:bg-[var(--accent-blue)]/30 border-l border-[var(--border-main)] transition-colors"
        />

        <div className="flex-1 overflow-hidden bg-[var(--bg-app)]">
          <PreviewPane
            previewData={previewData}
            loading={previewPending}
            selectedMatch={selectedMatch}
            api={api}
            onClose={() => setSelectedMatch(null)}
            canGoBack={historyIndex > 0}
            canGoForward={historyIndex < history.length - 1}
            onGoBack={goBack}
            onGoForward={goForward}
          />
        </div>
      </div>
    </div>
  );
}
