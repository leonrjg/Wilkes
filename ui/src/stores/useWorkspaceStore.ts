import { create } from "zustand";
import { api } from "../services";
import { confirmDialog } from "../lib/utils/dialog";
import type { WorkspaceState, WorkspaceSummary } from "../lib/types";
import { useBookmarksStore } from "./useBookmarksStore";
import { useSavedWorksStore } from "./useSavedWorksStore";
import { useFeedsStore } from "./useFeedsStore";
import { useChatStore } from "./useChatStore";
import { useEditorStore } from "./useEditorStore";
import { useFolderStateStore } from "./useFolderStateStore";
import { useResearchStore } from "./useResearchStore";
import { useSearchStore } from "./useSearchStore";
import { useSemanticStore } from "./useSemanticStore";
import { useSettingsStore } from "./useSettingsStore";
import { useTopicsStore } from "./useTopicsStore";
import { useCitationsStore } from "./useCitationsStore";
import { useReadingOrderStore } from "./useReadingOrderStore";
import { useViewerStore } from "./useViewerStore";

interface WorkspaceStore {
  workspaces: WorkspaceSummary[];
  activeWorkspaceId: string | null;
  /** Workspaces other windows show. A workspace is shown by one window at
   *  most, so choosing one of these brings its window forward. */
  openElsewhere: string[];
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
  /** Brings forward the window showing the workspace, or opens one on it. */
  openInWindow: (workspaceId: string) => Promise<void>;
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
  useCitationsStore.getState().resetForWorkspace();
  useReadingOrderStore.getState().resetForWorkspace();
  useChatStore.getState().resetForWorkspace();
}

/** The store's view of one listing, as this window sees it. */
function fromListing(state: WorkspaceState) {
  return {
    workspaces: state.workspaces,
    activeWorkspaceId: state.active_workspace_id,
    openElsewhere: state.open_elsewhere,
  };
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

/** The active workspace's name, or null before the registry has been read. */
export function useActiveWorkspaceName(): string | null {
  return useWorkspaceStore((state) => activeWorkspace(state)?.name ?? null);
}

export const useWorkspaceStore = create<WorkspaceStore>((set, get) => ({
  workspaces: [],
  activeWorkspaceId: null,
  openElsewhere: [],
  loading: false,
  switching: false,

  load: async () => {
    set({ loading: true });
    try {
      const state = await api.listWorkspaces();
      set(fromListing(state));
      useEditorStore.getState().switchWorkspace(state.active_workspace_id);
      useFolderStateStore.getState().switchWorkspace(state.active_workspace_id);
      await useViewerStore.getState().switchWorkspace(state.active_workspace_id);
    } finally {
      set({ loading: false });
    }
  },

  refreshList: async () => {
    const state = await api.listWorkspaces();
    set(fromListing(state));
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
      // Not one another window shows: a workspace is shown by one window.
      const successor = get().workspaces.find(
        (workspace) => workspace.id !== workspaceId && !get().openElsewhere.includes(workspace.id),
      );
      if (!successor) {
        throw new Error(
          get().workspaces.length > 1
            ? "Every other workspace is open in another window. Close one of those windows first."
            : "The last workspace cannot be deleted.",
        );
      }
      await get().switchTo(successor.id);
      if (get().activeWorkspaceId === workspaceId) return false;
    }
    const state = await api.deleteWorkspace(workspaceId);
    useEditorStore.getState().forgetWorkspace(workspaceId);
    useFolderStateStore.getState().forgetWorkspace(workspaceId);
    useViewerStore.getState().forgetWorkspace(workspaceId);
    set(fromListing(state));
    return true;
  },

  openInWindow: async (workspaceId) => {
    if (!api.openWorkspaceWindow) {
      throw new Error("Opening a workspace in a window of its own needs the desktop app");
    }
    await api.openWorkspaceWindow(workspaceId);
  },

  switchTo: async (workspaceId) => {
    if (workspaceId === get().activeWorkspaceId || get().switching) return;
    // Another window shows it: that window is where it is, and this one
    // keeps its own workspace and its unsaved work.
    if (get().openElsewhere.includes(workspaceId)) {
      await get().openInWindow(workspaceId);
      return;
    }
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
      useFolderStateStore.getState().switchWorkspace(workspaceId);
      await useViewerStore.getState().switchWorkspace(workspaceId);
      set(fromListing(state));
      await Promise.all([
        useSettingsStore.getState().load(),
        useBookmarksStore.getState().load(),
        useSavedWorksStore.getState().load(),
        useResearchStore.getState().load(),
        useFeedsStore.getState().reloadIfLoaded(),
      ]);
    } catch (error) {
      // The response can fail after the backend has committed its atomic
      // switch. Re-read the registry so the UI never guesses which workspace
      // owns subsequent operations.
      const state = await api.listWorkspaces().catch(() => null);
      if (state) {
        useEditorStore.getState().switchWorkspace(state.active_workspace_id);
        useFolderStateStore.getState().switchWorkspace(state.active_workspace_id);
        await useViewerStore.getState().switchWorkspace(state.active_workspace_id);
        set(fromListing(state));
        await Promise.all([
          useSettingsStore.getState().load(),
          useBookmarksStore.getState().load(),
          useSavedWorksStore.getState().load(),
          useResearchStore.getState().load(),
          useFeedsStore.getState().reloadIfLoaded(),
        ]);
      }
      throw error;
    } finally {
      set({ switching: false });
    }
  },
}));
