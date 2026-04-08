import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import { api } from "../services";
import type { FileEntry, Theme } from "../lib/types";

const EMPTY_EXCLUDED: Set<string> = new Set();

function applyTheme(theme: Theme) {
  const root = window.document.documentElement;
  root.classList.remove("light", "dark");
  if (theme === "Light") root.classList.add("light");
  else if (theme === "Dark") root.classList.add("dark");
  else {
    const systemDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    root.classList.add(systemDark ? "dark" : "light");
  }
}

interface SettingsStore {
  bookmarks: string[];
  recentDirs: string[];
  directory: string;
  respectGitignore: boolean;
  maxFileSize: number;
  contextLines: number;
  supportedExtensions: string[];
  fileList: FileEntry[];
  filterText: string;
  excluded: Set<string>;
  semanticIndexBuilt: boolean;
  preferSemantic: boolean;
  indexing: boolean;
  theme: Theme;
  maxResults: number;

  load: () => Promise<void>;
  setDirectory: (dir: string) => void;
  addBookmark: (dir: string) => void;
  removeBookmark: (dir: string) => void;
  forgetDirectory: (dir: string) => void;
  refreshFileList: () => void;
  setExcluded: (excluded: Set<string>) => void;
  setFilterText: (text: string) => void;
  setPreferSemantic: (active: boolean) => void;
  setIndexing: (indexing: boolean) => void;
  refreshSemanticReady: () => Promise<void>;
  startSemanticIndex: () => Promise<void>;
  applySettingsPatch: (patch: { theme?: Theme; supported_extensions?: string[]; max_results?: number }) => void;
}

export const useSettingsStore = create<SettingsStore>()(
  subscribeWithSelector((set, get) => ({
    bookmarks: [],
    recentDirs: [],
    directory: "",
    respectGitignore: true,
    maxFileSize: 10 * 1024 * 1024,
    contextLines: 2,
    supportedExtensions: [],
    fileList: [],
    filterText: "",
    excluded: EMPTY_EXCLUDED,
    semanticIndexBuilt: false,
    preferSemantic: false,
    indexing: false,
    theme: "System",
    maxResults: 50,

    load: async () => {
      const s = await api.getSettings();
      document.body.classList.toggle("demo-mode", !!s.is_demo);
      applyTheme(s.theme);

      // Set up system theme listener if needed
      if (s.theme === "System") {
        const media = window.matchMedia("(prefers-color-scheme: dark)");
        const listener = () => applyTheme("System");
        media.addEventListener("change", listener);
      }

      set({
        bookmarks: s.bookmarked_dirs,
        recentDirs: s.recent_dirs || [],
        directory: s.last_directory ?? "",
        respectGitignore: s.respect_gitignore,
        maxFileSize: s.max_file_size,
        supportedExtensions: s.supported_extensions || [],
        semanticIndexBuilt: false, // will be confirmed below
        preferSemantic: s.search_prefer_semantic,
        theme: s.theme,
        maxResults: s.max_results ?? 0,
      });

      const ready = await api.isSemanticReady();
      set({ semanticIndexBuilt: ready });

      if (!ready && s.search_prefer_semantic) {
        get().startSemanticIndex().catch(console.error);
      }
    },

    setDirectory: (dir: string) => {
      const { recentDirs, directory } = get();
      const next = [dir, ...recentDirs.filter((d) => d !== dir)].slice(0, 10);
      api.updateSettings({ last_directory: dir, recent_dirs: next }).catch(() => {});
      if (dir === directory) {
        // Subscription only fires on value change; refresh explicitly when directory is unchanged.
        get().refreshFileList();
      } else {
        set({ directory: dir, recentDirs: next });
      }
    },

    addBookmark: (dir: string) => {
      const { bookmarks } = get();
      if (bookmarks.includes(dir)) return;
      const next = [...bookmarks, dir];
      api.updateSettings({ bookmarked_dirs: next }).catch(() => {});
      set({ bookmarks: next });
    },

    removeBookmark: (dir: string) => {
      const { bookmarks } = get();
      const next = bookmarks.filter((b) => b !== dir);
      api.updateSettings({ bookmarked_dirs: next }).catch(() => {});
      set({ bookmarks: next });
    },

    forgetDirectory: (dir: string) => {
      const { bookmarks, recentDirs, directory } = get();
      const nextBookmarks = bookmarks.filter((b) => b !== dir);
      const nextRecent = recentDirs.filter((d) => d !== dir);
      const nextDir = directory === dir ? "" : directory;

      api.updateSettings({
        bookmarked_dirs: nextBookmarks,
        recent_dirs: nextRecent,
        last_directory: nextDir || null,
      }).catch(() => {});

      set({ bookmarks: nextBookmarks, recentDirs: nextRecent, directory: nextDir });
    },

    refreshFileList: () => {
      const { directory } = get();
      if (!directory) return;
      api.listFiles(directory)
        .then((files) => set({ fileList: files }))
        .catch(() => {});
    },

    setExcluded: (excluded: Set<string>) => set({ excluded }),
    setFilterText: (text: string) => set({ filterText: text }),
    setIndexing: (indexing: boolean) => set({ indexing }),

    setPreferSemantic: (active: boolean) => {
      set({ preferSemantic: active });
      api.updateSettings({ search_prefer_semantic: active }).catch(console.error);
    },

    refreshSemanticReady: async () => {
      const ready = await api.isSemanticReady();
      set({ semanticIndexBuilt: ready });
    },

    startSemanticIndex: async () => {
      const { directory } = get();
      const s = await api.getSettings();
      await api.buildIndex(directory, s.semantic.model, s.semantic.engine);
    },

    applySettingsPatch: (patch) => {
      if (patch.theme) {
        applyTheme(patch.theme);
        set({ theme: patch.theme });
      }
      if (patch.supported_extensions) {
        set({ supportedExtensions: patch.supported_extensions });
      }
      if (patch.max_results !== undefined) {
        set({ maxResults: patch.max_results });
      }
    },
  }))
);

// fileList is derived from directory: whenever directory changes, reload (or clear) the file list.
// This subscription is the single owner of the directory → fileList transition, so any code path
// that sets directory automatically gets the correct fileList without needing to know about it.
useSettingsStore.subscribe(
  (state) => state.directory,
  (directory) => {
    if (directory) {
      api
        .listFiles(directory)
        .then((files) => useSettingsStore.setState({ fileList: files, excluded: EMPTY_EXCLUDED, filterText: "" }))
        .catch(() => {});
    } else {
      useSettingsStore.setState({ fileList: [], excluded: EMPTY_EXCLUDED, filterText: "" });
    }
  }
);
