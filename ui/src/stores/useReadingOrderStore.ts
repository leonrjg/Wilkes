import { create } from "zustand";
import { api } from "../services";
import type { ReadingOrder } from "../lib/types";

let latestRequest = 0;

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * The reading order of the folder the file list shows.
 *
 * The order is the backend's: it reads every document's text, which the list
 * cannot. The list asks for it when it is sorted by reading order and again
 * when its files change, and sorts by whatever is here. A theme is part of
 * the question, so changing it asks again.
 */
interface ReadingOrderStore {
  /** The folder the order is of. */
  root: string | null;
  /** Which listing of that folder it was asked for: a different listing is a
   *  different question. */
  listing: string | null;
  /** "" for none. */
  theme: string;
  order: ReadingOrder | null;
  loading: boolean;
  error: string | null;
  /** Asks for the order of `root` unless it is already here for `listing`. */
  load: (root: string, listing: string) => Promise<void>;
  setTheme: (theme: string) => Promise<void>;
  /** Asks again: a document's text may have been read since. */
  refresh: () => Promise<void>;
  resetForWorkspace: () => void;
}

export const useReadingOrderStore = create<ReadingOrderStore>((set, get) => {
  const ask = async () => {
    const { root, theme } = get();
    if (!root) return;
    const request = ++latestRequest;
    set({ loading: true, error: null });
    try {
      const order = await api.readingOrder({ root, theme: theme.trim() || null });
      if (request !== latestRequest) return;
      set({ order, loading: false });
    } catch (error) {
      // A request overtaken by a newer one is refused by the backend too;
      // only the latest one's failure is the list's to show.
      if (request !== latestRequest) return;
      console.error("Reading order failed:", error);
      set({ loading: false, error: errorText(error) });
    }
  };

  return {
    root: null,
    listing: null,
    theme: "",
    order: null,
    loading: false,
    error: null,

    load: async (root, listing) => {
      const current = get();
      if (current.root === root && current.listing === listing) return;
      set({
        root,
        listing,
        // A theme belongs to the folder it was typed for.
        theme: current.root === root ? current.theme : "",
        order: current.root === root ? current.order : null,
      });
      await ask();
    },

    setTheme: async (theme) => {
      if (get().theme === theme) return;
      set({ theme });
      await ask();
    },

    refresh: ask,

    resetForWorkspace: () => {
      latestRequest += 1;
      set({ root: null, listing: null, theme: "", order: null, loading: false, error: null });
    },
  };
});
