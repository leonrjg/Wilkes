import { create } from "zustand";
import { api } from "../services";
import type { CitationGraph } from "../lib/types";

/** The current folder, or every library root of the workspace. */
export type CitationScope = "root" | "library";
export type CitationView = "list" | "graph";

export const MIN_EXTERNAL_CITATION_OPTIONS = [1, 2, 3, 5] as const;

interface CitationsStore {
  paneOpen: boolean;
  scope: CitationScope;
  view: CitationView;
  minExternalCitations: number;
  loading: boolean;
  error: string | null;
  graph: CitationGraph | null;
  /** What `graph` was built for, so a change of folder shows as loading
   *  rather than as the previous folder's graph. */
  loadedKey: string | null;
  fetching: boolean;
  openPane: () => void;
  closePane: () => void;
  setScope: (scope: CitationScope) => void;
  setView: (view: CitationView) => void;
  setMinExternalCitations: (min: number) => void;
  /** Build the graph for `directory` (the current folder) under the current
   *  scope. A response that arrives after a newer request is dropped. */
  load: (directory: string) => Promise<void>;
  /** Queue reference fetches for documents no provider has answered for.
   *  Resolves to how many were queued. */
  fetchMissing: (directory: string) => Promise<number>;
  resetForWorkspace: () => void;
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

let latestRequest = 0;

export function citationQueryKey(
  scope: CitationScope,
  directory: string,
  minExternalCitations: number,
) {
  return `${scope}\u0000${scope === "root" ? directory : ""}\u0000${minExternalCitations}`;
}

export const useCitationsStore = create<CitationsStore>((set, get) => ({
  paneOpen: false,
  scope: "root",
  view: "list",
  minExternalCitations: 2,
  loading: false,
  error: null,
  graph: null,
  loadedKey: null,
  fetching: false,

  openPane: () => set({ paneOpen: true }),
  closePane: () => set({ paneOpen: false }),
  setScope: (scope) => set({ scope }),
  setView: (view) => set({ view }),
  setMinExternalCitations: (minExternalCitations) => set({ minExternalCitations }),

  load: async (directory) => {
    const { scope, minExternalCitations } = get();
    const request = ++latestRequest;
    set({ loading: true, error: null });
    try {
      const graph = await api.citationGraph({
        root: scope === "root" ? directory : null,
        min_external_citations: minExternalCitations,
      });
      if (request !== latestRequest) return;
      set({
        graph,
        loading: false,
        loadedKey: citationQueryKey(scope, directory, minExternalCitations),
      });
    } catch (error) {
      if (request !== latestRequest) return;
      set({ loading: false, error: errorText(error) });
      throw error;
    }
  },

  fetchMissing: async (directory) => {
    const { scope, minExternalCitations } = get();
    set({ fetching: true });
    try {
      return await api.fetchMissingCitations({
        root: scope === "root" ? directory : null,
        min_external_citations: minExternalCitations,
      });
    } finally {
      set({ fetching: false });
    }
  },

  resetForWorkspace: () => {
    latestRequest += 1;
    set({
      paneOpen: false,
      loading: false,
      error: null,
      graph: null,
      loadedKey: null,
      fetching: false,
    });
  },
}));
