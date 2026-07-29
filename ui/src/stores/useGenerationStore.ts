import { create } from "zustand";
import { api } from "../services";
import type { BookmarkClusterLabelled } from "../lib/types";

/**
 * The single readiness gate for every LLM-dependent affordance.
 *
 * Deliberately not `settings.generation.enabled`: enabled-but-not-installed is
 * exactly the state that produces a spinner which never resolves. `ready`
 * mirrors the backend's `is_generation_ready()`, which also requires a selected
 * model and an attached generator.
 *
 * The rule at every call site: when `ready` is false the UI renders as if the
 * feature did not exist — no spinner, no greyed-out control, no "enable
 * generation to…" placeholder. The Settings section is the one exception,
 * because it is what makes the feature ready.
 */
interface GenerationStore {
  ready: boolean;
  /** Labels that arrived after `cluster_bookmarks` returned, by cluster key. */
  clusterLabels: Record<string, string>;

  refreshReady: () => Promise<boolean>;
  applyClusterLabel: (event: BookmarkClusterLabelled) => void;
  /** Called when a new clustering run starts: labels from the previous run
   *  describe a partition that is no longer displayed. */
  clearClusterLabels: () => void;
}

export const useGenerationStore = create<GenerationStore>((set) => ({
  ready: false,
  clusterLabels: {},

  refreshReady: async () => {
    try {
      const ready = await api.isGenerationReady();
      set({ ready });
      return ready;
    } catch (e) {
      console.debug("Generation readiness check failed:", e);
      set({ ready: false });
      return false;
    }
  },

  applyClusterLabel: ({ cluster_key, label }) =>
    set((state) => ({
      clusterLabels: { ...state.clusterLabels, [cluster_key]: label },
    })),

  clearClusterLabels: () => set({ clusterLabels: {} }),
}));
