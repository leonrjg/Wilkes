import { create } from "zustand";
import { api } from "../services";
import type { Bookmark, NewBookmark } from "../lib/types";

interface BookmarksStore {
  bookmarks: Bookmark[];
  filterText: string;
  scopePath: string | null;
  paneOpen: boolean;
  load: () => Promise<void>;
  add: (bookmark: NewBookmark) => Promise<Bookmark>;
  remove: (id: string) => Promise<void>;
  updateNote: (id: string, note: string | null) => Promise<void>;
  setFilter: (text: string) => void;
  setScope: (path: string | null) => void;
  togglePane: () => void;
}

export const useBookmarksStore = create<BookmarksStore>((set) => ({
  bookmarks: [],
  filterText: "",
  scopePath: null,
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
  setScope: (scopePath) => set({ scopePath }),
  togglePane: () => set((state) => ({ paneOpen: !state.paneOpen })),
}));
