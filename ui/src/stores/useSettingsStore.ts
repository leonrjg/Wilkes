import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import { api } from "../services";
import type { BookmarkDock, FileEntry, OmittedFileEntry, SemanticSettings, Settings, Theme } from "../lib/types";

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
  favorites: string[];
  recentDirs: string[];
  directory: string;
  semantic: SemanticSettings | null;
  respectGitignore: boolean;
  maxFileSize: number;
  contextLines: number;
  supportedExtensions: string[];
  fileList: FileEntry[];
  omittedFileList: OmittedFileEntry[];
  filterText: string;
  preferSemantic: boolean;
  indexing: boolean;
  theme: Theme;
  maxResults: number;
  bookmarksDock: BookmarkDock;

  load: () => Promise<void>;
  setDirectory: (dir: string) => void;
  addFavorite: (dir: string) => void;
  removeFavorite: (dir: string) => void;
  forgetDirectory: (dir: string) => void;
  refreshFileList: () => void;
  setFilterText: (text: string) => void;
  setPreferSemantic: (active: boolean) => void;
  setIndexing: (indexing: boolean) => void;
  applySettingsPatch: (patch: { theme?: Theme; supported_extensions?: string[]; max_results?: number; bookmarks_dock?: BookmarkDock }) => void;
  setBookmarksDock: (dock: BookmarkDock) => void;
  replaceSettings: (settings: Settings) => void;
  refreshSettings: () => Promise<Settings>;
}

export const useSettingsStore = create<SettingsStore>()(
  subscribeWithSelector((set, get) => ({
    favorites: [],
    recentDirs: [],
    directory: "",
    semantic: null,
    respectGitignore: true,
    maxFileSize: 10 * 1024 * 1024,
    contextLines: 2,
    supportedExtensions: [],
    fileList: [],
    omittedFileList: [],
    filterText: "",
    preferSemantic: false,
    indexing: false,
    theme: "System",
    maxResults: 50,
    bookmarksDock: "Right",

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
        favorites: s.favorites,
        recentDirs: s.recent_dirs || [],
        directory: s.last_directory ?? "",
        semantic: s.semantic,
        respectGitignore: s.respect_gitignore,
        maxFileSize: s.max_file_size,
        supportedExtensions: s.supported_extensions || [],
        preferSemantic: s.search_prefer_semantic,
        theme: s.theme,
        maxResults: s.max_results ?? 0,
        bookmarksDock: s.bookmarks_dock ?? "Right",
        omittedFileList: [],
      });
    },

    setDirectory: (dir: string) => {
      const { recentDirs, directory } = get();
      const next = recentDirs.includes(dir) ? recentDirs : [...recentDirs, dir].slice(-10);
      api.updateSettings({ last_directory: dir, recent_dirs: next }).catch(() => {});
      if (dir === directory) {
        // Subscription only fires on value change; refresh explicitly when directory is unchanged.
        get().refreshFileList();
      } else {
        set({ directory: dir, recentDirs: next });
      }
    },

    addFavorite: (dir: string) => {
      const { favorites } = get();
      if (favorites.includes(dir)) return;
      const next = [...favorites, dir];
      api.updateSettings({ favorites: next }).catch(() => {});
      set({ favorites: next });
    },

    removeFavorite: (dir: string) => {
      const { favorites } = get();
      const next = favorites.filter((b) => b !== dir);
      api.updateSettings({ favorites: next }).catch(() => {});
      set({ favorites: next });
    },

    forgetDirectory: (dir: string) => {
      const { favorites, recentDirs, directory } = get();
      const nextBookmarks = favorites.filter((b) => b !== dir);
      const nextRecent = recentDirs.filter((d) => d !== dir);
      const nextDir = directory === dir ? "" : directory;

      api.updateSettings({
        favorites: nextBookmarks,
        recent_dirs: nextRecent,
        last_directory: nextDir || null,
      }).catch(() => {});

      set({ favorites: nextBookmarks, recentDirs: nextRecent, directory: nextDir });
    },

    refreshFileList: () => {
      const { directory } = get();
      if (!directory) return;
      api.listFiles(directory)
        .then((response) => set({ fileList: response.files, omittedFileList: response.omitted }))
        .catch(() => {});
    },

    setFilterText: (text: string) => set({ filterText: text }),
    setIndexing: (indexing: boolean) => set({ indexing }),

    setPreferSemantic: (active: boolean) => {
      set({ preferSemantic: active });
      api.updateSettings({ search_prefer_semantic: active }).catch(console.error);
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
      if (patch.bookmarks_dock) {
        set({ bookmarksDock: patch.bookmarks_dock });
      }
    },

    setBookmarksDock: (dock) => {
      set({ bookmarksDock: dock });
      api.updateSettings({ bookmarks_dock: dock }).catch(console.error);
    },

    replaceSettings: (settings) => {
      applyTheme(settings.theme);
      set({
        favorites: settings.favorites,
        recentDirs: settings.recent_dirs || [],
        directory: settings.last_directory ?? "",
        semantic: settings.semantic,
        respectGitignore: settings.respect_gitignore,
        maxFileSize: settings.max_file_size,
        supportedExtensions: settings.supported_extensions || [],
        preferSemantic: settings.search_prefer_semantic,
        theme: settings.theme,
        maxResults: settings.max_results ?? 0,
        bookmarksDock: settings.bookmarks_dock ?? "Right",
        omittedFileList: [],
      });
    },

    refreshSettings: async () => {
      const settings = await api.getSettings();
      get().replaceSettings(settings);
      return settings;
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
        .then((response) =>
          useSettingsStore.setState({
            fileList: response.files,
            omittedFileList: response.omitted,
            filterText: "",
          }))
        .catch(() => {});
    } else {
      useSettingsStore.setState({ fileList: [], omittedFileList: [], filterText: "" });
    }
  }
);
