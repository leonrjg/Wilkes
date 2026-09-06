import { create } from "zustand";
import { randomId } from "../lib/types";
import { api } from "../services";
import type {
  BookmarkClusterGranularity,
  ChunkTopicLabelled,
  ChunkTopicsResult,
} from "../lib/types";

interface DocumentTopicsState {
  loading: boolean;
  requestId: string | null;
  error: string | null;
  result: ChunkTopicsResult | null;
  root: string | null;
  path: string | null;
  granularity: BookmarkClusterGranularity;
  selectedTopicKey: string | null;
}

interface TopicsStore {
  paneOpen: boolean;
  loading: boolean;
  requestId: string | null;
  error: string | null;
  result: ChunkTopicsResult | null;
  root: string | null;
  granularity: BookmarkClusterGranularity;
  selectedTopicKey: string | null;
  document: DocumentTopicsState;
  openPane: () => void;
  closePane: () => void;
  setGranularity: (granularity: BookmarkClusterGranularity) => void;
  selectTopic: (clusterKey: string | null) => void;
  load: (root: string) => Promise<void>;
  loadDocument: (root: string, path: string) => Promise<void>;
  cancelDocument: () => void;
  setDocumentGranularity: (granularity: BookmarkClusterGranularity) => void;
  selectDocumentTopic: (clusterKey: string | null) => void;
  applyLabel: (event: ChunkTopicLabelled) => void;
  resetForWorkspace: () => void;
}

/** Backend errors arrive as plain strings over the command bridge, Error
 *  objects from the fetch transport. Both have to end up on screen: a pane
 *  that is blank for no stated reason is indistinguishable from one that is
 *  still working. */
function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

function cancelRequest(requestId: string | null) {
  if (requestId) {
    void api.cancelChunkTopics(requestId).catch((error) =>
      console.debug("Could not cancel chunk-topic request:", error),
    );
  }
}

function patchLabel(
  result: ChunkTopicsResult | null,
  clusterKey: string,
  label: string,
) {
  if (!result) return result;
  let changed = false;
  const topics = result.topics.map((topic) => {
    if (topic.cluster_key !== clusterKey) return topic;
    changed = true;
    return { ...topic, label };
  });
  return changed ? { ...result, topics } : result;
}

const emptyDocumentState: DocumentTopicsState = {
  loading: false,
  requestId: null,
  error: null,
  result: null,
  root: null,
  path: null,
  granularity: "much_fewer",
  selectedTopicKey: null,
};

export const useTopicsStore = create<TopicsStore>((set, get) => ({
  paneOpen: false,
  loading: false,
  requestId: null,
  error: null,
  result: null,
  root: null,
  granularity: "much_fewer",
  selectedTopicKey: null,
  document: emptyDocumentState,

  openPane: () => set({ paneOpen: true }),
  closePane: () => {
    cancelRequest(get().requestId);
    set({ paneOpen: false, loading: false, requestId: null });
  },
  setGranularity: (granularity) =>
    set({ granularity, selectedTopicKey: null }),
  selectTopic: (selectedTopicKey) => set({ selectedTopicKey }),

  load: async (root) => {
    cancelRequest(get().requestId);
    const requestId = randomId();
    const granularity = get().granularity;
    set({
      requestId,
      loading: true,
      root,
      selectedTopicKey: null,
      error: null,
      ...(get().root === root ? {} : { result: null }),
    });
    try {
      const result = await api.chunkTopics(requestId, { root, granularity });
      if (get().requestId === requestId && get().paneOpen) {
        set({ result, loading: false, error: null });
      }
    } catch (error) {
      if (get().requestId === requestId && get().paneOpen) {
        set({ loading: false, error: errorText(error) });
        throw error;
      }
      // A superseded or closed-pane request still failed for a reason. It has
      // nowhere to be shown, so it goes to the log rather than nowhere.
      console.debug("Superseded topic request failed:", error);
    }
  },

  loadDocument: async (root, path) => {
    cancelRequest(get().document.requestId);
    const requestId = randomId();
    const granularity = get().document.granularity;
    const sameDocument =
      get().document.root === root && get().document.path === path;
    set((state) => ({
      document: {
        ...state.document,
        requestId,
        loading: true,
        root,
        path,
        selectedTopicKey: null,
        error: null,
        ...(sameDocument ? {} : { result: null }),
      },
    }));
    try {
      const result = await api.chunkTopics(requestId, {
        root,
        path,
        granularity,
      });
      if (get().document.requestId === requestId) {
        set((state) => ({
          document: { ...state.document, result, loading: false, error: null },
        }));
      }
    } catch (error) {
      if (get().document.requestId === requestId) {
        set((state) => ({
          document: {
            ...state.document,
            loading: false,
            error: errorText(error),
          },
        }));
        throw error;
      }
      console.debug("Superseded document topic request failed:", error);
    }
  },

  cancelDocument: () => {
    cancelRequest(get().document.requestId);
    set((state) => ({
      document: {
        ...state.document,
        loading: false,
        requestId: null,
      },
    }));
  },
  setDocumentGranularity: (granularity) =>
    set((state) => ({
      document: {
        ...state.document,
        granularity,
        selectedTopicKey: null,
      },
    })),
  selectDocumentTopic: (selectedTopicKey) =>
    set((state) => ({
      document: { ...state.document, selectedTopicKey },
    })),

  applyLabel: ({ request_id, cluster_key, label }) =>
    set((state) => {
      const rootMatches = state.requestId === request_id;
      const documentMatches = state.document.requestId === request_id;
      if (!rootMatches && !documentMatches) return {};

      return {
        ...(rootMatches
          ? { result: patchLabel(state.result, cluster_key, label) }
          : {}),
        ...(documentMatches
          ? {
              document: {
                ...state.document,
                result: patchLabel(
                  state.document.result,
                  cluster_key,
                  label,
                ),
              },
            }
          : {}),
      };
    }),
  resetForWorkspace: () => {
    cancelRequest(get().requestId);
    cancelRequest(get().document.requestId);
    set({
      paneOpen: false,
      loading: false,
      requestId: null,
      error: null,
      result: null,
      root: null,
      selectedTopicKey: null,
      document: emptyDocumentState,
    });
  },
}));
