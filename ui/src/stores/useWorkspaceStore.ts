import { create } from "zustand";
import { api } from "../services";
import { confirmDialog } from "../lib/utils/dialog";
import type { WorkspaceSummary } from "../lib/types";
import { useBookmarksStore } from "./useBookmarksStore";
import { useChatStore } from "./useChatStore";
import { useEditorStore } from "./useEditorStore";
import { useResearchStore } from "./useResearchStore";
import { useSearchStore } from "./useSearchStore";
import { useSemanticStore } from "./useSemanticStore";
import { useSettingsStore } from "./useSettingsStore";
import { useTopicsStore } from "./useTopicsStore";
import { useViewerStore } from "./useViewerStore";

interface WorkspaceStore {
  workspaces: WorkspaceSummary[];
  activeWorkspaceId: string | null;
  loading: boolean;
  switching: boolean;
  load: () => Promise<void>;
  /**
   * Re-reads the registry listing alone. Unlike `load`, it touches neither the
   * editor nor the viewer: a page that only shows the list must not discard
   * open buffers and tabs to refresh it.
   */
  refreshList: () => Promise<void>;
  createAndSwitch: (name: string) => Promise<void>;
  rename: (workspaceId: string, name: string) => Promise<void>;
  switchTo: (workspaceId: string) => Promise<void>;
  /**
   * Deletes a workspace and everything it owns, activating another one first
   * when the target is the active workspace. Resolves to `false` when the
   * switch that deletion required was declined.
   */
  remove: (workspaceId: string) => Promise<boolean>;
}

function clearWorkspaceUi() {
  useSearchStore.getState().clearResults();
  useSettingsStore.getState().resetForWorkspace();
  useBookmarksStore.setState({ bookmarks: [], filterText: "", paneOpen: false });
  useResearchStore.setState({
    tags: [],
    collections: [],
    history: [],
    selectedCollectionId: null,
    selectedTagId: null,
    draftCollectionExpression: null,
  });
  useSemanticStore.setState({
    indexStatus: null,
    readyForCurrentRoot: false,
    readyGlobally: false,
    status: "idle",
    buildRoot: null,
    blockedRoot: null,
    error: null,
    coverage: {},
    coverageRoots: [],
  });
  useTopicsStore.getState().resetForWorkspace();
  useChatStore.getState().resetForWorkspace();
}

/**
 * The active workspace, or null before the registry has been read.
 */
function activeWorkspace(state: WorkspaceStore): WorkspaceSummary | null {
  return state.workspaces.find((workspace) => workspace.id === state.activeWorkspaceId) ?? null;
}

/**
 * Whether the active workspace may only be read.
 *
 * One predicate, so no component decides for itself what read-only means: the
 * backend refuses the write either way, and a component that guessed
 * differently would offer a control that always fails or withhold one that
 * would have worked. `useActiveWorkspaceReadOnly` is the same answer for
 * components that must re-render when it changes.
 */
export function activeWorkspaceIsReadOnly(): boolean {
  return activeWorkspace(useWorkspaceStore.getState())?.read_only ?? false;
}

export function useActiveWorkspaceReadOnly(): boolean {
  return useWorkspaceStore((state) => activeWorkspace(state)?.read_only ?? false);
}

export const useWorkspaceStore = create<WorkspaceStore>((set, get) => ({
  workspaces: [],
  activeWorkspaceId: null,
  loading: false,
  switching: false,

  load: async () => {
    set({ loading: true });
    try {
      const state = await api.listWorkspaces();
      set({
        workspaces: state.workspaces,
        activeWorkspaceId: state.active_workspace_id,
      });
      useEditorStore.getState().switchWorkspace(state.active_workspace_id);
      await useViewerStore.getState().switchWorkspace(state.active_workspace_id);
    } finally {
      set({ loading: false });
    }
  },

  refreshList: async () => {
    const state = await api.listWorkspaces();
    set({
      workspaces: state.workspaces,
      activeWorkspaceId: state.active_workspace_id,
    });
  },

  createAndSwitch: async (name) => {
    const workspace = await api.createWorkspace(name);
    set((state) => ({ workspaces: [...state.workspaces, workspace] }));
    await get().switchTo(workspace.id);
  },

  rename: async (workspaceId, name) => {
    const workspace = await api.renameWorkspace(workspaceId, name);
    set((state) => ({
      workspaces: state.workspaces.map((item) => item.id === workspaceId ? workspace : item),
    }));
  },

  remove: async (workspaceId) => {
    // The backend refuses to delete the active workspace: activating one moves
    // the window and reloads every store, and that is this store's job rather
    // than a side effect of a delete. So the switch happens here, through the
    // one path that already does all of it — and if the user declines it (the
    // unsaved-editor prompt), nothing is deleted.
    if (workspaceId === get().activeWorkspaceId) {
      const successor = get().workspaces.find((workspace) => workspace.id !== workspaceId);
      if (!successor) {
        throw new Error("The last workspace cannot be deleted.");
      }
      await get().switchTo(successor.id);
      if (get().activeWorkspaceId === workspaceId) return false;
    }
    const state = await api.deleteWorkspace(workspaceId);
    useEditorStore.getState().forgetWorkspace(workspaceId);
    useViewerStore.getState().forgetWorkspace(workspaceId);
    set({
      workspaces: state.workspaces,
      activeWorkspaceId: state.active_workspace_id,
    });
    return true;
  },

  switchTo: async (workspaceId) => {
    if (workspaceId === get().activeWorkspaceId || get().switching) return;
    const hasDirtyEditors = Object.values(useEditorStore.getState().buffers)
      .some((buffer) => buffer.dirty);
    if (
      hasDirtyEditors
      && !await confirmDialog("Switch workspaces and discard unsaved editor changes?")
    ) {
      return;
    }
    set({ switching: true });
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    clearWorkspaceUi();
    try {
      const state = await api.switchWorkspace(workspaceId);
      useEditorStore.getState().switchWorkspace(workspaceId);
      await useViewerStore.getState().switchWorkspace(workspaceId);
      set({
        workspaces: state.workspaces,
        activeWorkspaceId: state.active_workspace_id,
      });
      await Promise.all([
        useSettingsStore.getState().load(),
        useBookmarksStore.getState().load(),
        useResearchStore.getState().load(),
      ]);
    } catch (error) {
      // The response can fail after the backend has committed its atomic
      // switch. Re-read the registry so the UI never guesses which workspace
      // owns subsequent operations.
      const state = await api.listWorkspaces().catch(() => null);
      if (state) {
        useEditorStore.getState().switchWorkspace(state.active_workspace_id);
        await useViewerStore.getState().switchWorkspace(state.active_workspace_id);
        set({
          workspaces: state.workspaces,
          activeWorkspaceId: state.active_workspace_id,
        });
        await Promise.all([
          useSettingsStore.getState().load(),
          useBookmarksStore.getState().load(),
          useResearchStore.getState().load(),
        ]);
      }
      throw error;
    } finally {
      set({ switching: false });
    }
  },
}));
