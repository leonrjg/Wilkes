import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import { api } from "../services";
import { useResearchStore } from "./useResearchStore";
import type {
  AgentBackend,
  BookmarkDock,
  FileDisplayField,
  FileEntry,
  FileMetadataUpdate,
  FileSortDirection,
  FileSortKey,
  OmittedFileEntry,
  SemanticSettings,
  Settings,
  Theme,
} from "../lib/types";

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

function listFilesForActiveFilters(directory: string) {
  const { selectedCollectionId, selectedTagId, draftCollectionExpression } = useResearchStore.getState();
  if (!selectedCollectionId && !selectedTagId && !draftCollectionExpression) return api.listFiles(directory);
  return api.listFiles(
    directory,
    selectedCollectionId,
    selectedTagId ? [selectedTagId] : [],
    draftCollectionExpression,
  );
}

interface SettingsStore {
  favorites: string[];
  settings: Settings | null;
  recentDirs: string[];
  directory: string;
  semantic: SemanticSettings | null;
  respectGitignore: boolean;
  maxFileSize: number;
  contextLines: number;
  supportedExtensions: string[];
  fileList: FileEntry[];
  omittedFileList: OmittedFileEntry[];
  preferSemantic: boolean;
  indexing: boolean;
  theme: Theme;
  maxResults: number;
  bookmarksDock: BookmarkDock;
  fileSortKey: FileSortKey;
  fileSortDirection: FileSortDirection;
  fileDisplayFields: FileDisplayField[];
  chatBackend: AgentBackend;

  load: () => Promise<void>;
  setDirectory: (dir: string) => void;
  addFavorite: (dir: string) => void;
  removeFavorite: (dir: string) => void;
  forgetDirectory: (dir: string) => void;
  renameDirectory: (oldPath: string, newPath: string) => void;
  refreshFileList: () => Promise<void>;
  applyMetadataUpdates: (updates: FileMetadataUpdate[]) => void;
  setPreferSemantic: (active: boolean) => void;
  setIndexing: (indexing: boolean) => void;
  applySettingsPatch: (patch: Partial<Settings>) => void;
  setBookmarksDock: (dock: BookmarkDock) => void;
  setTopicCloudInputCap: (cap: number) => void;
  setFileSortKey: (key: FileSortKey) => void;
  setFileSortDirection: (direction: FileSortDirection) => void;
  toggleFileDisplayField: (field: FileDisplayField) => void;
  setChatBackend: (backend: AgentBackend) => void;
  replaceSettings: (settings: Settings) => void;
  refreshSettings: () => Promise<Settings>;
}

export const useSettingsStore = create<SettingsStore>()(
  subscribeWithSelector((set, get) => ({
    favorites: [],
    settings: null,
    recentDirs: [],
    directory: "",
    semantic: null,
    respectGitignore: true,
    maxFileSize: 10 * 1024 * 1024,
    contextLines: 2,
    supportedExtensions: [],
    fileList: [],
    omittedFileList: [],
    preferSemantic: false,
    indexing: false,
    theme: "System",
    maxResults: 50,
    bookmarksDock: "Right",
    fileSortKey: "filename",
    fileSortDirection: "asc",
    fileDisplayFields: ["size"],
    chatBackend: "ClaudeCode",

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
        settings: s,
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
        fileSortKey: s.file_sort_key ?? "filename",
        fileSortDirection: s.file_sort_direction ?? "asc",
        fileDisplayFields: s.file_display_fields ?? ["size"],
        chatBackend: s.chat_backend ?? "ClaudeCode",
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

    renameDirectory: (oldPath: string, newPath: string) => {
      if (oldPath === newPath) return;
      // A rename invalidates every stored path equal to, or nested under, the
      // renamed folder. Rewrite the prefix so favorites, history and the active
      // directory keep pointing at the same folders.
      const remap = (path: string): string => {
        if (path === oldPath) return newPath;
        if (path.startsWith(`${oldPath}/`) || path.startsWith(`${oldPath}\\`)) {
          return newPath + path.slice(oldPath.length);
        }
        return path;
      };
      const { favorites, recentDirs, directory } = get();
      const nextFavorites = favorites.map(remap);
      const nextRecent = recentDirs.map(remap);
      const nextDir = remap(directory);

      api.updateSettings({
        favorites: nextFavorites,
        recent_dirs: nextRecent,
        last_directory: nextDir || null,
      }).catch(() => {});

      set({ favorites: nextFavorites, recentDirs: nextRecent, directory: nextDir });
    },

    refreshFileList: async () => {
      const { directory } = get();
      if (!directory) return;
      try {
        const response = await listFilesForActiveFilters(directory);
        set({ fileList: response.files, omittedFileList: response.omitted });
      } catch (error) {
        // A failed filter must never leave the previous, now-misleading list visible.
        set({ fileList: [], omittedFileList: [] });
        console.error("Failed to refresh file list:", error);
      }
    },

    applyMetadataUpdates: (updates: FileMetadataUpdate[]) => {
      if (updates.length === 0) return;
      if (
        useResearchStore.getState().selectedCollectionId ||
        useResearchStore.getState().selectedTagId
      ) {
        get().refreshFileList();
        return;
      }
      const byPath = new Map(updates.map((u) => [u.path, u]));
      const patch = <T extends FileEntry>(entry: T): T => {
        const update = byPath.get(entry.path);
        if (!update) return entry;
        return {
          ...entry,
          title: Object.prototype.hasOwnProperty.call(update, "title")
            ? update.title ?? null
            : entry.title,
          author: Object.prototype.hasOwnProperty.call(update, "author")
            ? update.author ?? null
            : entry.author,
          doi: Object.prototype.hasOwnProperty.call(update, "doi")
            ? update.doi ?? null
            : entry.doi,
          publication_date: update.publication_date ?? null,
          citation_count: update.citation_count ?? null,
          metadata_conflicts: update.metadata_conflicts ?? entry.metadata_conflicts,
        };
      };
      set((state) => ({
        fileList: state.fileList.map(patch),
        omittedFileList: state.omittedFileList.map(patch),
      }));
    },

    setIndexing: (indexing: boolean) => set({ indexing }),

    setPreferSemantic: (active: boolean) => {
      set({ preferSemantic: active });
      api.updateSettings({ search_prefer_semantic: active }).catch(console.error);
    },

    applySettingsPatch: (patch) => {
      const settings = get().settings;
      if (settings) {
        set({
          settings: {
            ...settings,
            ...patch,
            semantic: patch.semantic ? { ...settings.semantic, ...patch.semantic } : settings.semantic,
            integrations: patch.integrations
              ? {
                  ...settings.integrations,
                  ...patch.integrations,
                  zotero: {
                    ...settings.integrations.zotero,
                    ...patch.integrations.zotero,
                  },
                  semantic_scholar: {
                    ...settings.integrations.semantic_scholar,
                    ...patch.integrations.semantic_scholar,
                  },
                  openalex: {
                    ...settings.integrations.openalex,
                    ...patch.integrations.openalex,
                  },
                }
              : settings.integrations,
          },
        });
      }
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
      if (patch.file_sort_key) {
        set({ fileSortKey: patch.file_sort_key });
      }
      if (patch.file_sort_direction) {
        set({ fileSortDirection: patch.file_sort_direction });
      }
      if (patch.file_display_fields) {
        set({ fileDisplayFields: patch.file_display_fields });
      }
      if (patch.chat_backend) {
        set({ chatBackend: patch.chat_backend });
      }
      if (patch.primary_metadata_source) {
        get().refreshFileList();
      }
    },

    setBookmarksDock: (dock) => {
      set({ bookmarksDock: dock });
      api.updateSettings({ bookmarks_dock: dock }).catch(console.error);
    },

    setTopicCloudInputCap: (cap) => {
      const semantic = get().settings?.semantic;
      if (!semantic) return;
      const topic_cloud_input_cap = Math.max(3, Math.round(cap));
      const nextSemantic = { ...semantic, topic_cloud_input_cap };
      set((state) => ({
        semantic: nextSemantic,
        settings: state.settings
          ? { ...state.settings, semantic: nextSemantic }
          : state.settings,
      }));
      api.updateSettings({ semantic: nextSemantic }).catch(console.error);
    },

    setFileSortKey: (key) => {
      set({ fileSortKey: key });
      api.updateSettings({ file_sort_key: key }).catch(console.error);
    },

    setFileSortDirection: (direction) => {
      set({ fileSortDirection: direction });
      api.updateSettings({ file_sort_direction: direction }).catch(console.error);
    },

    toggleFileDisplayField: (field) => {
      const current = get().fileDisplayFields;
      const next = current.includes(field)
        ? current.filter((f) => f !== field)
        : [...current, field];
      set({ fileDisplayFields: next });
      api.updateSettings({ file_display_fields: next }).catch(console.error);
    },

    setChatBackend: (backend) => {
      set({ chatBackend: backend });
      api.updateSettings({ chat_backend: backend }).catch(console.error);
    },

    replaceSettings: (settings) => {
      applyTheme(settings.theme);
      set({
        settings,
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
        fileSortKey: settings.file_sort_key ?? "filename",
        fileSortDirection: settings.file_sort_direction ?? "asc",
        fileDisplayFields: settings.file_display_fields ?? ["size"],
        chatBackend: settings.chat_backend ?? "ClaudeCode",
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
      listFilesForActiveFilters(directory)
        .then((response) =>
          useSettingsStore.setState({
            fileList: response.files,
            omittedFileList: response.omitted,
          }))
        .catch((error) => {
          console.error("Failed to refresh file list:", error);
          useSettingsStore.setState({ fileList: [], omittedFileList: [] });
        });
    } else {
      useSettingsStore.setState({ fileList: [], omittedFileList: [] });
    }
  }
);
