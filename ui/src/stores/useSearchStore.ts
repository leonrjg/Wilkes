import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type { FileMatches, MatchRef, PreviewData, SearchQuery, SearchStats } from "../lib/types";

interface SearchStore {
  results: FileMatches[];
  stats: SearchStats | null;
  searching: boolean;
  hasQuery: boolean;
  selectedMatch: MatchRef | null;
  previewData: PreviewData | null;
  previewLoading: boolean;
  currentSearchId: string | null;
  lastQuery: SearchQuery | null;

  search: (query: SearchQuery) => Promise<void>;
  replaySearch: () => Promise<void>;
  setHasQuery: (hasQuery: boolean) => void;
  selectMatch: (matchRef: MatchRef) => void;
  clearPreview: () => void;
  clearResults: () => void;
}

export const useSearchStore = create<SearchStore>((set, get) => ({
  results: [],
  stats: null,
  searching: false,
  hasQuery: false,
  selectedMatch: null,
  previewData: null,
  previewLoading: false,
  currentSearchId: null,
  lastQuery: null,

  search: async (query: SearchQuery) => {
    const { currentSearchId, results } = get();
    if (currentSearchId) {
      await api.cancelSearch(currentSearchId).catch(() => {});
    }

    // Keep existing results visible until the first new result arrives.
    // Clear selected match/preview immediately since they belong to the old query.
    const hasStale = results.length > 0;
    set({ stats: null, searching: true, lastQuery: query, selectedMatch: null, previewData: null });
    if (!hasStale) set({ results: [] });

    let firstResult = true;

    try {
      const searchId = await api.search(
        query,
        (fm) => {
          if (firstResult) {
            firstResult = false;
            set({ results: [fm] });
          } else {
            set((state) => ({ results: [...state.results, fm] }));
          }
        },
        (s) => set({ results: firstResult ? [] : get().results, stats: s, searching: false, currentSearchId: null }),
      );
      set({ currentSearchId: searchId });
    } catch (e: any) {
      const msg = e?.toString() ?? "Search failed";
      console.error("Search failed:", e);
      set({
        stats: { files_scanned: 0, total_matches: 0, elapsed_ms: 0, errors: [msg] },
        searching: false,
      });
    }
  },

  replaySearch: async () => {
    const { lastQuery, search } = get();
    if (!lastQuery) return;

    if (lastQuery.mode === "Semantic") {
      try {
        const indexStatus = await api.getIndexStatus();
        const usable = isUsableSemanticIndex(indexStatus, lastQuery.root);
        if (!usable) return;
      } catch {
        return;
      }
    }

    await search(lastQuery);
  },

  setHasQuery: (hasQuery: boolean) => set({ hasQuery }),

  selectMatch: (matchRef: MatchRef) => {
    set({ selectedMatch: matchRef, previewLoading: true });
    api
      .preview(matchRef)
      .then((data) => set({ previewData: data, previewLoading: false }))
      .catch((e) => {
        console.error("Preview failed:", e);
        set({ previewData: null, previewLoading: false });
      });
  },

  clearPreview: () => set({ selectedMatch: null, previewData: null }),
  clearResults: () => set({ results: [], stats: null, selectedMatch: null, previewData: null }),
}));
