import { create } from "zustand";

/**
 * Which folder the tidy-up dialog is open for.
 *
 * A tidy-up is asked for from a root in the strip at the top of the window or
 * from a folder of the sidebar tree, and those are drawn by components that do
 * not contain one another. The one dialog that answers both reads this.
 */
interface TidyStore {
  folder: string | null;
  open(folder: string): void;
  close(): void;
}

export const useTidyStore = create<TidyStore>((set) => ({
  folder: null,
  open: (folder) => set({ folder }),
  close: () => set({ folder: null }),
}));
