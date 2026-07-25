import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type { FileMatches, SearchQuery, SearchStats } from "../lib/types";

interface SearchStore {
  results: FileMatches[];
  stats: SearchStats | null;
  searching: boolean;
  hasQuery: boolean;
  currentSearchId: string | null;
  lastQuery: SearchQuery | null;

  search: (query: SearchQuery) => Promise<void>;
  deferSemanticSearch: (query: SearchQuery) => void;
  replaySearch: () => Promise<void>;
  invalidateSemanticResultsForRoot: (root: string) => void;
  setHasQuery: (hasQuery: boolean) => void;
  clearResults: () => void;
}

export const useSearchStore = create<SearchStore>((set, get) => ({
  results: [],
  stats: null,
  searching: false,
  hasQuery: false,
  currentSearchId: null,
  lastQuery: null,

  search: async (query: SearchQuery) => {
    const { currentSearchId, results } = get();
    if (currentSearchId) {
      await api.cancelSearch(currentSearchId).catch(() => {});
    }

    // Keep existing results visible until the first new result arrives. Open
    // viewer tabs are intentionally independent from the search lifecycle.
    const hasStale = results.length > 0;
    set({ stats: null, searching: true, lastQuery: query });
    if (!hasStale) set({ results: [] });

    let firstResult = true;

    try {
      const searchId = await api.search(
        query,
        (fileMatches) => {
          if (firstResult) {
            firstResult = false;
            set({ results: [fileMatches] });
          } else {
            set((state) => ({ results: [...state.results, fileMatches] }));
          }
        },
        (stats) =>
          set({
            results: firstResult ? [] : get().results,
            stats,
            searching: false,
            currentSearchId: null,
          }),
      );
      set({ currentSearchId: searchId });
    } catch (error: any) {
      const message = error?.toString() ?? "Search failed";
      console.error("Search failed:", error);
      set({
        stats: {
          files_scanned: 0,
          total_matches: 0,
          elapsed_ms: 0,
          errors: [message],
        },
        searching: false,
      });
    }
  },

  deferSemanticSearch: (query) =>
    set({
      lastQuery: query,
      stats: null,
      searching: false,
      currentSearchId: null,
    }),

  replaySearch: async () => {
    const { lastQuery, search } = get();
    if (!lastQuery) return;

    if (lastQuery.mode === "Semantic") {
      try {
        const all = lastQuery.scope?.type === "all";
        const indexStatus = await api.getIndexStatus(all ? undefined : lastQuery.root);
        const usable = isUsableSemanticIndex(indexStatus, all ? undefined : lastQuery.root);
        if (!usable) return;
      } catch {
        return;
      }
    }

    await search(lastQuery);
  },

  setHasQuery: (hasQuery) => set({ hasQuery }),

  invalidateSemanticResultsForRoot: (root) =>
    set((state) => {
      if (
        state.lastQuery?.mode !== "Semantic" ||
        (state.lastQuery.scope?.type !== "all" && state.lastQuery.root !== root)
      ) {
        return {};
      }
      return {
        results: [],
        stats: null,
        searching: false,
        currentSearchId: null,
      };
    }),

  clearResults: () => set({ results: [], stats: null }),
}));
