import { create } from "zustand";
import { api } from "../services";
import { isUsableSemanticIndex } from "../lib/semantic";
import type { IndexStatus, RootCoverage } from "../lib/types";
import { useSearchStore } from "./useSearchStore";
import { useSettingsStore } from "./useSettingsStore";

type SemanticRootStatus = "idle" | "checking" | "missing" | "ready" | "building" | "error";

interface SemanticStore {
  indexStatus: IndexStatus | null;
  readyForCurrentRoot: boolean;
  readyGlobally: boolean;
  status: SemanticRootStatus;
  buildRoot: string | null;
  blockedRoot: string | null;
  error: string | null;
  /** What the index covers, per root, keyed by the root as configured. Empty
   *  until a caller names the roots it wants covered — the backend walks a
   *  directory per root, so nothing asks speculatively. */
  coverage: Record<string, RootCoverage>;
  /** The roots the last `refreshCoverage` was over, so index events can
   *  re-ask without every caller having to hand the list back. */
  coverageRoots: string[];

  refreshCurrentRootStatus: () => Promise<boolean>;
  refreshGlobalStatus: () => Promise<boolean>;
  refreshCoverage: (roots?: string[]) => Promise<void>;
  ensureCurrentRootIndexed: (freshAttempt?: boolean) => Promise<boolean>;
  handleIndexUpdated: () => Promise<void>;
  handleIndexTerminated: () => Promise<void>;
  handleCurrentRootIndexRemoved: () => Promise<void>;
}

export const useSemanticStore = create<SemanticStore>((set, get) => ({
  indexStatus: null,
  readyForCurrentRoot: false,
  readyGlobally: false,
  status: "idle",
  buildRoot: null,
  blockedRoot: null,
  error: null,
  coverage: {},
  coverageRoots: [],

  refreshCurrentRootStatus: async () => {
    const { directory } = useSettingsStore.getState();
    const { buildRoot } = get();

    if (!directory) {
      set({
        indexStatus: null,
        readyForCurrentRoot: false,
        status: "idle",
        buildRoot: null,
        blockedRoot: null,
        error: null,
      });
      return false;
    }

    set((state) => ({
      status: state.buildRoot === directory ? "building" : "checking",
      error: null,
    }));

    try {
      const indexStatus = await api.getIndexStatus(directory);
      const ready = isUsableSemanticIndex(indexStatus, directory);
      set((state) => ({
        indexStatus,
        readyForCurrentRoot: ready,
        status: ready ? "ready" : buildRoot === directory ? "building" : "missing",
        blockedRoot: ready ? null : state.blockedRoot,
        error: null,
      }));
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

  refreshGlobalStatus: async () => {
    try {
      const indexStatus = await api.getIndexStatus();
      const ready = isUsableSemanticIndex(indexStatus);
      set({ readyGlobally: ready });
      return ready;
    } catch {
      set({ readyGlobally: false });
      return false;
    }
  },

  /**
   * Ask what the index holds for each root, without building anything.
   *
   * This is the detection that used to be inseparable from starting a build:
   * switching roots discovered the root was not indexed and began hours of
   * inference on the strength of that discovery. The discovery was the useful
   * half, so it lives here on its own and the build stays an act the user
   * performs.
   */
  refreshCoverage: async (roots) => {
    const requested = roots ?? get().coverageRoots;
    if (roots) set({ coverageRoots: roots });
    if (requested.length === 0) {
      set({ coverage: {} });
      return;
    }
    // Coverage is a claim about an index, so there has to be one. Asking first
    // also keeps a directory walk per root from running for a workspace that
    // has never been indexed, where the answer is known without looking.
    if (!(await get().refreshGlobalStatus())) {
      set({ coverage: {} });
      return;
    }
    try {
      // Each row carries back the root it was asked about, verbatim, so the
      // map keys match what a caller holds without re-normalising a path here.
      const rows = await api.indexCoverage(requested);
      if (rows.length !== requested.length) {
        console.error(
          `indexCoverage returned ${rows.length} row(s) for ${requested.length} root(s)`,
        );
      }
      set({ coverage: Object.fromEntries(rows.map((row) => [row.root, row])) });
    } catch (e) {
      // No index at all is the ordinary case here, not a fault: there is
      // nothing to report coverage against and the roots simply go unmarked.
      console.error("indexCoverage failed:", e);
      set({ coverage: {} });
    }
  },

  ensureCurrentRootIndexed: async (freshAttempt = false) => {
    const { directory, preferSemantic, semantic } = useSettingsStore.getState();

    if (!directory) {
      await get().refreshCurrentRootStatus();
      return false;
    }

    if (get().blockedRoot === directory && !freshAttempt) {
      return false;
    }

    const ready = await get().refreshCurrentRootStatus();
    if (!preferSemantic || ready) {
      return ready;
    }

    if (get().blockedRoot === directory && !freshAttempt) {
      return false;
    }

    if (get().blockedRoot === directory && freshAttempt) {
      set({ blockedRoot: null });
    }

    if (!semantic || get().buildRoot === directory) {
      return false;
    }

    set({
      buildRoot: directory,
      status: "building",
      blockedRoot: null,
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
    await get().refreshGlobalStatus();

    if (!directory || buildRoot === directory || ready) {
      set({ buildRoot: null, status: ready ? "ready" : directory ? "missing" : "idle" });
    }

    await get().refreshCoverage();

    if (ready) {
      await useSearchStore.getState().replaySearch();
    }
  },

  handleIndexTerminated: async () => {
    const { directory } = useSettingsStore.getState();
    set({
      buildRoot: null,
      readyForCurrentRoot: false,
      status: directory ? "checking" : "idle",
    });
    await get().refreshCurrentRootStatus();
    await get().refreshCoverage();
  },

  handleCurrentRootIndexRemoved: async () => {
    const { directory } = useSettingsStore.getState();

    set({
      indexStatus: null,
      readyForCurrentRoot: false,
      status: directory ? "missing" : "idle",
      buildRoot: null,
      blockedRoot: directory,
      error: null,
    });

    if (directory) {
      useSearchStore.getState().invalidateSemanticResultsForRoot(directory);
    }
    await get().refreshCoverage();
  },
}));

// Switching roots reads the new root's index state and stops there. It used to
// call `ensureCurrentRootIndexed`, which starts a build when the root has no
// index — so moving between roots to look at a file committed the machine to
// hours of inference nobody asked for. The reading is what the interface needs
// in order to mark the root as unindexed; starting the build is the user's.
useSettingsStore.subscribe(
  (state) => state.directory,
  () => {
    useSemanticStore.getState().refreshCurrentRootStatus().catch(console.error);
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
