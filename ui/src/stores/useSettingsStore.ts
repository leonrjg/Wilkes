import { create } from "zustand";
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

  load: () => Promise<void>;
  setDirectory: (dir: string) => void;
  addBookmark: (dir: string) => void;
  removeBookmark: (dir: string) => void;
  setExcluded: (excluded: Set<string>) => void;
  setFilterText: (text: string) => void;
  setPreferSemantic: (active: boolean) => void;
  setIndexing: (indexing: boolean) => void;
  refreshSemanticReady: () => Promise<void>;
  applySettingsPatch: (patch: { theme?: Theme; supported_extensions?: string[] }) => void;
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
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

  load: async () => {
    const s = await api.getSettings();
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
      semanticIndexBuilt: s.semantic.enabled && s.semantic.index_path !== null,
      preferSemantic: s.search_prefer_semantic,
      theme: s.theme,
    });

    if (s.last_directory) {
      api
        .listFiles(s.last_directory)
        .then((files) => set({ fileList: files, excluded: EMPTY_EXCLUDED }))
        .catch(() => {});
    }
  },

  setDirectory: (dir: string) => {
    const { recentDirs } = get();
    const next = [dir, ...recentDirs.filter((d) => d !== dir)].slice(0, 10);
    api.updateSettings({ last_directory: dir, recent_dirs: next }).catch(() => {});
    set({ directory: dir, recentDirs: next, filterText: "" });
    api
      .listFiles(dir)
      .then((files) => set({ fileList: files, excluded: EMPTY_EXCLUDED }))
      .catch(() => {});
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

  setExcluded: (excluded: Set<string>) => set({ excluded }),
  setFilterText: (text: string) => set({ filterText: text }),
  setIndexing: (indexing: boolean) => set({ indexing }),

  setPreferSemantic: (active: boolean) => {
    set({ preferSemantic: active });
    api.updateSettings({ search_prefer_semantic: active }).catch(console.error);
  },

  refreshSemanticReady: async () => {
    try {
      const s = await api.getSettings();
      set({ semanticIndexBuilt: s.semantic.enabled && s.semantic.index_path !== null });
    } catch (e) {
      console.error("getSettings failed in refreshSemanticReady:", e);
    }
  },

  applySettingsPatch: (patch) => {
    if (patch.theme) {
      applyTheme(patch.theme);
      set({ theme: patch.theme });
    }
    if (patch.supported_extensions) {
      set({ supportedExtensions: patch.supported_extensions });
    }
  },
}));
