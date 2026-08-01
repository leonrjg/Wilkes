import { create } from "zustand";
import { api } from "../services";
import type {
  BookmarkClusterGranularity,
  ChunkTopicLabelled,
  ChunkTopicsResult,
} from "../lib/types";

interface TopicsStore {
  paneOpen: boolean;
  loading: boolean;
  requestId: number;
  result: ChunkTopicsResult | null;
  root: string | null;
  granularity: BookmarkClusterGranularity;
  selectedTopicKey: string | null;
  openPane: () => void;
  closePane: () => void;
  setGranularity: (granularity: BookmarkClusterGranularity) => void;
  selectTopic: (clusterKey: string | null) => void;
  load: (root: string) => Promise<void>;
  applyLabel: (event: ChunkTopicLabelled) => void;
}

export const useTopicsStore = create<TopicsStore>((set, get) => ({
  paneOpen: false,
  loading: false,
  requestId: 0,
  result: null,
  root: null,
  granularity: "much_fewer",
  selectedTopicKey: null,

  openPane: () => set({ paneOpen: true }),
  closePane: () => set({ paneOpen: false }),
  setGranularity: (granularity) =>
    set({ granularity, selectedTopicKey: null }),
  selectTopic: (selectedTopicKey) => set({ selectedTopicKey }),

  load: async (root) => {
    const requestId = get().requestId + 1;
    const granularity = get().granularity;
    set({
      requestId,
      loading: true,
      root,
      selectedTopicKey: null,
      ...(get().root === root ? {} : { result: null }),
    });
    try {
      const result = await api.chunkTopics({ root, granularity });
      if (get().requestId === requestId) {
        set({ result, loading: false });
      }
    } catch (error) {
      if (get().requestId === requestId) {
        set({ loading: false });
      }
      throw error;
    }
  },

  applyLabel: ({ cluster_key, label }) =>
    set((state) => {
      if (!state.result) return state;
      let changed = false;
      const topics = state.result.topics.map((topic) => {
        if (topic.cluster_key !== cluster_key) return topic;
        changed = true;
        return { ...topic, label };
      });
      return changed ? { result: { ...state.result, topics } } : state;
    }),
}));
