import { create } from "zustand";
import { api } from "../services";
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
  createAndSwitch: (name: string) => Promise<void>;
  rename: (workspaceId: string, name: string) => Promise<void>;
  switchTo: (workspaceId: string) => Promise<void>;
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

  switchTo: async (workspaceId) => {
    if (workspaceId === get().activeWorkspaceId || get().switching) return;
    const hasDirtyEditors = Object.values(useEditorStore.getState().buffers)
      .some((buffer) => buffer.dirty);
    if (
      hasDirtyEditors
      && !window.confirm("Switch workspaces and discard unsaved editor changes?")
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
