import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type { FileMatches, SearchQuery, SearchStats } from "../lib/types";

export type ResultContext =
  | { kind: "search"; subject: string }
  | { kind: "topic"; topicKey: string; subject: string | null };

interface SearchStore {
  results: FileMatches[];
  stats: SearchStats | null;
  searching: boolean;
  hasQuery: boolean;
  currentSearchId: string | null;
  lastQuery: SearchQuery | null;
  resultContext: ResultContext | null;

  search: (query: SearchQuery) => Promise<void>;
  deferSemanticSearch: (query: SearchQuery) => void;
  replaySearch: () => Promise<void>;
  invalidateSemanticResultsForRoot: (root: string) => void;
  showResultSet: (
    results: FileMatches[],
    context: ResultContext,
  ) => Promise<void>;
  updateTopicResultSubject: (topicKey: string, subject: string) => void;
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
  resultContext: null,

  search: async (query: SearchQuery) => {
    const { currentSearchId, results } = get();
    if (currentSearchId) {
      await api.cancelSearch(currentSearchId).catch(() => {});
    }

    // Keep existing results visible until the first new result arrives. Open
    // viewer tabs are intentionally independent from the search lifecycle.
    const hasStale = results.length > 0;
    set({
      stats: null,
      searching: true,
      lastQuery: query,
      resultContext: { kind: "search", subject: query.pattern },
    });
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
      resultContext: { kind: "search", subject: query.pattern },
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

  showResultSet: async (results, resultContext) => {
    const currentSearchId = get().currentSearchId;
    if (currentSearchId) {
      await api.cancelSearch(currentSearchId).catch(() => {});
    }
    set({
      results,
      stats: {
        files_scanned: results.length,
        total_matches: results.reduce(
          (count, file) => count + file.matches.length + (file.field_matches?.length ?? 0),
          0,
        ),
        elapsed_ms: 0,
        errors: [],
      },
      searching: false,
      hasQuery: true,
      currentSearchId: null,
      lastQuery: null,
      resultContext,
    });
  },

  updateTopicResultSubject: (topicKey, subject) =>
    set((state) => {
      if (
        state.resultContext?.kind !== "topic" ||
        state.resultContext.topicKey !== topicKey ||
        state.resultContext.subject === subject
      ) {
        return {};
      }
      return {
        resultContext: { ...state.resultContext, subject },
      };
    }),

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
        resultContext: null,
      };
    }),

  clearResults: () => {
    const currentSearchId = get().currentSearchId;
    if (currentSearchId) void api.cancelSearch(currentSearchId).catch(() => {});
    set({
      results: [],
      stats: null,
      searching: false,
      currentSearchId: null,
      hasQuery: false,
      lastQuery: null,
      resultContext: null,
    });
  },
}));
