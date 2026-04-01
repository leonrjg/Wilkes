import { useState, useCallback, useRef, useEffect, useTransition } from "react";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import DirectoryPicker from "./components/DirectoryPicker";
import UploadZone from "./components/UploadZone";
import { TauriSearchApi, TauriSourceApi } from "./services/tauri";
import { HttpSearchApi, HttpSourceApi } from "./services/http";
import SettingsModal from "./components/SettingsModal";
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
  const [results, setResults] = useState<FileMatches[]>([]);
  const [stats, setStats] = useState<SearchStats | null>(null);
  const [searching, setSearching] = useState(false);
  const [hasQuery, setHasQuery] = useState(false);
  const [selectedMatch, setSelectedMatch] = useState<MatchRef | null>(null);
  const [previewData, setPreviewData] = useState<PreviewData | null>(null);
  const [previewPending, startPreviewTransition] = useTransition();
  const currentSearchId = useRef<string | null>(null);

  const [bookmarks, setBookmarks] = useState<string[]>([]);
  const [directory, setDirectory] = useState<string>("");
  const [respectGitignore, setRespectGitignore] = useState(true);
  const [maxFileSize, setMaxFileSize] = useState(10 * 1024 * 1024);
  const [contextLines, setContextLines] = useState(2);
  const [fileList, setFileList] = useState<FileEntry[]>([]);
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [semanticIndexBuilt, setSemanticIndexBuilt] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);

  useEffect(() => {
    api.getSettings().then((s) => {
      setBookmarks(s.bookmarked_dirs);
      const dir = s.last_directory ?? "";
      setDirectory(dir);
      setRespectGitignore(s.respect_gitignore);
      setMaxFileSize(s.max_file_size);
      setContextLines(s.context_lines);
      setSemanticIndexBuilt(s.semantic.enabled && s.semantic.index_path !== null);
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
    api.updateSettings({ last_directory: dir }).catch(() => {});
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
    } catch (e) {
      console.error("Search failed:", e);
      setSearching(false);
    }
  }, []);

  const handleMatchClick = useCallback((matchRef: MatchRef) => {
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

  const handleFileClick = useCallback((path: string) => {
    setSelectedMatch({ path, origin: { PdfPage: { page: 1, bbox: null } } });
    startPreviewTransition(async () => {
      try {
        const data = await api.openFile(path);
        setPreviewData(data);
      } catch (e) {
        console.error("Open file failed:", e);
        setPreviewData(null);
      }
    });
  }, []);

  const sourceSlot = source.type === "desktop" ? (
    <DirectoryPicker
      directory={directory}
      bookmarks={bookmarks}
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
        className="px-2 py-1 rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)] transition-colors text-xs font-mono"
      >
        ⚙
      </button>
      <SettingsModal
        api={api}
        isOpen={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        directory={directory}
        refreshSemanticReady={refreshSemanticReady}
        onSettingsUpdate={(patch) => {
          if (patch.theme) setTheme(patch.theme);
        }}
      />
    </>
  ) : null;

  return (
    <div className="flex flex-col h-screen bg-[var(--bg-app)] text-[var(--text-main)] select-none">
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
        fileList={fileList}
        excluded={excluded}
        onExcludedChange={setExcluded}
        onQueryChange={setHasQuery}
      />

      <div className="flex flex-1 overflow-hidden">
        <div className="w-[420px] min-w-[320px] flex-shrink-0 border-r border-[var(--border-main)] flex flex-col">
          <ResultList
            results={results}
            stats={stats}
            searching={searching}
            hasQuery={hasQuery}
            fileList={fileList.filter((f) => !excluded.has(f.extension))}
            onMatchClick={handleMatchClick}
            onFileClick={handleFileClick}
            selectedMatch={selectedMatch}
          />
        </div>

        <div className="flex-1 overflow-hidden">
          <PreviewPane
            previewData={previewData}
            loading={previewPending}
            selectedMatch={selectedMatch}
            api={api}
          />
        </div>
      </div>
    </div>
  );
}
