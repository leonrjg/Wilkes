import { create } from "zustand";
import { api } from "../services";
import type { Bookmark, NewBookmark } from "../lib/types";

export type BookmarkScope = "current" | "all";

interface BookmarksStore {
  bookmarks: Bookmark[];
  filterText: string;
  scope: BookmarkScope;
  paneOpen: boolean;
  load: () => Promise<void>;
  add: (bookmark: NewBookmark) => Promise<Bookmark>;
  remove: (id: string) => Promise<void>;
  updateNote: (id: string, note: string | null) => Promise<void>;
  setFilter: (text: string) => void;
  setScope: (scope: BookmarkScope) => void;
  openPane: (scope: BookmarkScope) => void;
  closePane: () => void;
}

export const useBookmarksStore = create<BookmarksStore>((set) => ({
  bookmarks: [],
  filterText: "",
  scope: "all",
  paneOpen: false,

  load: async () => {
    const bookmarks = await api.listBookmarks();
    set({ bookmarks });
  },

  add: async (newBookmark) => {
    const bookmark = await api.addBookmark(newBookmark);
    set((state) => ({ bookmarks: [...state.bookmarks, bookmark] }));
    return bookmark;
  },

  remove: async (id) => {
    await api.removeBookmark(id);
    set((state) => ({ bookmarks: state.bookmarks.filter((bookmark) => bookmark.id !== id) }));
  },

  updateNote: async (id, note) => {
    const updated = await api.updateBookmarkNote(id, note);
    set((state) => ({
      bookmarks: state.bookmarks.map((bookmark) => (bookmark.id === id ? updated : bookmark)),
    }));
  },

  setFilter: (filterText) => set({ filterText }),
  setScope: (scope) => set({ scope }),
  openPane: (scope) => set({ paneOpen: true, scope }),
  closePane: () => set({ paneOpen: false }),
}));
