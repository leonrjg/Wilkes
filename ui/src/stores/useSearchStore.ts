import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type {
  DocumentMetadata,
  FileMatches,
  MatchRef,
  PreviewData,
  SearchQuery,
  SearchStats,
  ViewerMetadataStatus,
} from "../lib/types";

interface SearchStore {
  results: FileMatches[];
  stats: SearchStats | null;
  searching: boolean;
  hasQuery: boolean;
  selectedMatch: MatchRef | null;
  previewData: PreviewData | null;
  previewLoading: boolean;
  viewerMetadata: DocumentMetadata | null;
  viewerMetadataStatus: ViewerMetadataStatus;
  currentSearchId: string | null;
  lastQuery: SearchQuery | null;

  search: (query: SearchQuery) => Promise<void>;
  deferSemanticSearch: (query: SearchQuery) => void;
  replaySearch: () => Promise<void>;
  invalidateSemanticResultsForRoot: (root: string) => void;
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
  viewerMetadata: null,
  viewerMetadataStatus: "idle",
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
    set({
      stats: null,
      searching: true,
      lastQuery: query,
      selectedMatch: null,
      previewData: null,
      viewerMetadata: null,
      viewerMetadataStatus: "idle",
    });
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

  deferSemanticSearch: (query: SearchQuery) =>
    set({
      lastQuery: query,
      stats: null,
      searching: false,
      currentSearchId: null,
      selectedMatch: null,
      previewData: null,
      previewLoading: false,
      viewerMetadata: null,
      viewerMetadataStatus: "idle",
    }),

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

  invalidateSemanticResultsForRoot: (root: string) =>
    set((state) => {
      if (state.lastQuery?.mode !== "Semantic" || state.lastQuery.root !== root) {
        return {};
      }
      return {
        results: [],
        stats: null,
        searching: false,
        currentSearchId: null,
        selectedMatch: null,
        previewData: null,
        previewLoading: false,
        viewerMetadata: null,
        viewerMetadataStatus: "idle",
      };
    }),

  selectMatch: (matchRef: MatchRef) => {
    const previousPath = get().selectedMatch?.path;
    const selectedPath = matchRef.path;
    const sameFile = previousPath === selectedPath;
    set({
      selectedMatch: matchRef,
      previewLoading: true,
      viewerMetadata: sameFile ? get().viewerMetadata : null,
      viewerMetadataStatus: sameFile ? get().viewerMetadataStatus : "loading",
    });
    api
      .preview(matchRef)
      .then((data) => set({ previewData: data, previewLoading: false }))
      .catch((e) => {
        console.error("Preview failed:", e);
        set({ previewData: null, previewLoading: false });
      });

    if (sameFile) {
      return;
    }

    api
      .getFileMetadata(selectedPath)
      .then((metadata) => {
        if (get().selectedMatch?.path !== selectedPath) return;
        set({ viewerMetadata: metadata, viewerMetadataStatus: "ready" });
      })
      .catch((e) => {
        console.error("Metadata fetch failed:", e);
        if (get().selectedMatch?.path !== selectedPath) return;
        set({ viewerMetadata: null, viewerMetadataStatus: "failed" });
      });
  },

  clearPreview: () =>
    set({
      selectedMatch: null,
      previewData: null,
      viewerMetadata: null,
      viewerMetadataStatus: "idle",
    }),
  clearResults: () =>
    set({
      results: [],
      stats: null,
      selectedMatch: null,
      previewData: null,
      viewerMetadata: null,
      viewerMetadataStatus: "idle",
    }),
}));
