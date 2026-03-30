import { useState, useCallback, useRef, useEffect, useTransition } from "react";
import SearchBar from "./components/SearchBar";
import ResultList from "./components/ResultList";
import PreviewPane from "./components/PreviewPane";
import DirectoryPicker from "./components/DirectoryPicker";
import UploadZone from "./components/UploadZone";
import { TauriSearchApi, TauriSourceApi } from "./services/tauri";
import { HttpSearchApi, HttpSourceApi } from "./services/http";
import type { SearchApi, SourceApi, DesktopSourceApi, WebSourceApi } from "./services/api";
import type { FileEntry, FileMatches, MatchRef, PreviewData, SearchQuery, SearchStats } from "./lib/types";

const isTauri = "__TAURI_INTERNALS__" in window;

const api: SearchApi = isTauri ? new TauriSearchApi() : new HttpSearchApi();
const source: SourceApi = isTauri ? new TauriSourceApi() : new HttpSourceApi();

export default function App() {
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

  useEffect(() => {
    api.getSettings().then((s) => {
      setBookmarks(s.bookmarked_dirs);
      const dir = s.last_directory ?? "";
      setDirectory(dir);
      setRespectGitignore(s.respect_gitignore);
      setMaxFileSize(s.max_file_size);
      setContextLines(s.context_lines);
    }).catch(() => {});
  }, []);

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

  const handleSearch = useCallback(async (query: SearchQuery) => {
    if (currentSearchId.current) {
      await api.cancelSearch(currentSearchId.current).catch(() => {});
    }

    setResults([]);
    setStats(null);
    setSearching(true);
    setSelectedMatch(null);
    setPreviewData(null);

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

  return (
    <div className="flex flex-col h-screen bg-neutral-900 text-neutral-100 select-none">
      <SearchBar
        onSearch={handleSearch}
        searching={searching}
        sourceSlot={sourceSlot}
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
        <div className="w-[420px] min-w-[320px] flex-shrink-0 border-r border-neutral-800 flex flex-col">
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
