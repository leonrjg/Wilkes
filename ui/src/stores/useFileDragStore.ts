import { create } from "zustand";

/**
 * What a file drag is over, for the components that have to draw it.
 *
 * A drag that starts on a row in the sidebar can end on a folder of the tree
 * or on a root in the strip at the top of the window, and those are drawn by
 * two components that do not contain one another. So the drag's state cannot
 * live inside either. It lives here: written only by the tree, which owns the
 * gesture and the one hit test that decides where the drag is pointing, and
 * read by everything that has to look like a destination.
 */
interface FileDragStore {
  /** The file being dragged out of the tree, or null — meaning either that no
   *  drag is in progress, or that this is a drag from outside the application,
   *  which has no row of ours to grey out and no folder it is already in. */
  path: string | null;
  /** The directory the drag would drop into: a folder of the tree, or a root
   *  of the strip. Null when it is over neither. */
  target: string | null;
  setPath(path: string | null): void;
  setTarget(target: string | null): void;
}

export const useFileDragStore = create<FileDragStore>((set) => ({
  path: null,
  target: null,
  setPath: (path) => set({ path }),
  setTarget: (target) => set({ target }),
}));
