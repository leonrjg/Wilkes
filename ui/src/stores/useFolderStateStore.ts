import { create } from "zustand";

/**
 * Which sidebar folders the user has closed, kept across restarts for the
 * active workspace.
 *
 * A folder is open unless it is named here, so a folder that appears after a
 * refresh opens by default and one the user closed stays closed. Folder paths
 * are absolute, which is why one set serves every root of the workspace: two
 * roots cannot both contain the same path.
 *
 * Like the tab session and the reader positions, this is a convenience and
 * never a precondition: storage that refuses a read or a write is logged and
 * the tree carries on from memory.
 *
 * A folder that is renamed, moved or deleted leaves its old path behind. The
 * tree cannot tell that apart from a listing that has not arrived yet, so
 * nothing is pruned against the tree; the record ages out under
 * `MAX_REMEMBERED_FOLDERS` instead.
 */

export const FOLDER_STATE_STORAGE_KEY = "wilkes.file-tree.collapsed";
export const MAX_REMEMBERED_FOLDERS = 2000;

let activeWorkspaceId = "default";

const storageKey = (workspaceId: string) => `${FOLDER_STATE_STORAGE_KEY}.${workspaceId}`;

function read(): Set<string> {
  if (typeof localStorage === "undefined") return new Set();
  let raw: string | null;
  try {
    raw = localStorage.getItem(storageKey(activeWorkspaceId));
  } catch (error) {
    console.error("Could not read the remembered folder state:", error);
    return new Set();
  }
  if (raw === null) return new Set();
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) {
      console.error("The remembered folder state is not a list; starting with every folder open.");
      return new Set();
    }
    return new Set(parsed.filter((item): item is string => typeof item === "string"));
  } catch (error) {
    console.error("Could not parse the remembered folder state:", error);
    return new Set();
  }
}

function persist(collapsed: Set<string>): void {
  if (typeof localStorage === "undefined") return;
  try {
    if (collapsed.size === 0) localStorage.removeItem(storageKey(activeWorkspaceId));
    else localStorage.setItem(storageKey(activeWorkspaceId), JSON.stringify([...collapsed]));
  } catch (error) {
    console.error("Could not remember the folder state:", error);
  }
}

interface FolderStateStore {
  /** Most recently closed last, so eviction takes from the front. */
  collapsed: Set<string>;
  toggle(path: string): void;
  /** Opens a folder; a no-op when it is already open. */
  expand(path: string): void;
  switchWorkspace(workspaceId: string): void;
  /** Drops the folder state a deleted workspace left behind. */
  forgetWorkspace(workspaceId: string): void;
}

export const useFolderStateStore = create<FolderStateStore>((set, get) => ({
  collapsed: read(),
  toggle(path) {
    const next = new Set(get().collapsed);
    if (!next.delete(path)) {
      next.add(path);
      for (const oldest of next) {
        if (next.size <= MAX_REMEMBERED_FOLDERS) break;
        next.delete(oldest);
      }
    }
    persist(next);
    set({ collapsed: next });
  },
  expand(path) {
    if (!get().collapsed.has(path)) return;
    const next = new Set(get().collapsed);
    next.delete(path);
    persist(next);
    set({ collapsed: next });
  },
  switchWorkspace(workspaceId) {
    activeWorkspaceId = workspaceId;
    set({ collapsed: read() });
  },
  forgetWorkspace(workspaceId) {
    if (typeof localStorage === "undefined") return;
    try {
      localStorage.removeItem(storageKey(workspaceId));
    } catch (error) {
      console.error("Could not clear the folder state of a deleted workspace:", error);
    }
  },
}));
