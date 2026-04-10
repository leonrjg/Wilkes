import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type { IndexStatus } from "../lib/types";
import { useSearchStore } from "./useSearchStore";
import { useSettingsStore } from "./useSettingsStore";

type SemanticRootStatus = "idle" | "checking" | "missing" | "ready" | "building" | "error";

interface SemanticStore {
  indexStatus: IndexStatus | null;
  readyForCurrentRoot: boolean;
  status: SemanticRootStatus;
  buildRoot: string | null;
  error: string | null;

  refreshCurrentRootStatus: () => Promise<boolean>;
  ensureCurrentRootIndexed: () => Promise<boolean>;
  handleIndexUpdated: () => Promise<void>;
}

export const useSemanticStore = create<SemanticStore>((set, get) => ({
  indexStatus: null,
  readyForCurrentRoot: false,
  status: "idle",
  buildRoot: null,
  error: null,

  refreshCurrentRootStatus: async () => {
    const { directory } = useSettingsStore.getState();
    const { buildRoot } = get();

    if (!directory) {
      set({
        indexStatus: null,
        readyForCurrentRoot: false,
        status: "idle",
        buildRoot: null,
        error: null,
      });
      return false;
    }

    set((state) => ({
      status: state.buildRoot === directory ? "building" : "checking",
      error: null,
    }));

    try {
      const indexStatus = await api.getIndexStatus();
      const ready = isUsableSemanticIndex(indexStatus, directory);
      set({
        indexStatus,
        readyForCurrentRoot: ready,
        status: ready ? "ready" : buildRoot === directory ? "building" : "missing",
        error: null,
      });
      return ready;
    } catch (e: any) {
      set({
        indexStatus: null,
        readyForCurrentRoot: false,
        status: buildRoot === directory ? "building" : "error",
        error: e?.toString?.() ?? "Failed to read semantic index status",
      });
      return false;
    }
  },

  ensureCurrentRootIndexed: async () => {
    const { directory, preferSemantic, semantic } = useSettingsStore.getState();

    if (!directory) {
      await get().refreshCurrentRootStatus();
      return false;
    }

    const ready = await get().refreshCurrentRootStatus();
    if (!preferSemantic || ready) {
      return ready;
    }

    if (!semantic || get().buildRoot === directory) {
      return false;
    }

    set({
      buildRoot: directory,
      status: "building",
      error: null,
    });

    try {
      await api.buildIndex(directory, semantic.selected);
    } catch (e: any) {
      set({
        buildRoot: null,
        status: "error",
        error: e?.toString?.() ?? "Failed to start semantic index build",
      });
      throw e;
    }

    return false;
  },

  handleIndexUpdated: async () => {
    const { directory } = useSettingsStore.getState();
    const buildRoot = get().buildRoot;
    const ready = await get().refreshCurrentRootStatus();

    if (!directory || buildRoot === directory || ready) {
      set({ buildRoot: null });
    }

    if (ready) {
      await useSearchStore.getState().replaySearch();
    }
  },
}));

useSettingsStore.subscribe(
  (state) => state.directory,
  () => {
    useSemanticStore.getState().ensureCurrentRootIndexed().catch(console.error);
  },
);

useSettingsStore.subscribe(
  (state) => state.preferSemantic,
  (preferSemantic) => {
    if (preferSemantic) {
      useSemanticStore.getState().ensureCurrentRootIndexed().catch(console.error);
    }
  },
);
