import { create } from "zustand";
import { api } from "../services";
import type { LiteratureSearchResult, SavedWork } from "../lib/types";

interface SavedWorksStore {
  works: SavedWork[];
  load: () => Promise<void>;
  save: (provider: string, work: LiteratureSearchResult) => Promise<SavedWork>;
  remove: (id: string) => Promise<void>;
  updateNote: (id: string, note: string | null) => Promise<void>;
  /** Records where a result landed once it is in the library. Nothing is
   *  recorded for a result that was never saved. */
  markAcquired: (provider: string, work: LiteratureSearchResult, path: string) => Promise<void>;
}

/** Puts `saved` in the list in place of the entry with its id, or at the end. */
function upsert(works: SavedWork[], saved: SavedWork): SavedWork[] {
  return works.some((held) => held.id === saved.id)
    ? works.map((held) => (held.id === saved.id ? saved : held))
    : [...works, saved];
}

export const useSavedWorksStore = create<SavedWorksStore>((set) => ({
  works: [],

  load: async () => {
    const works = await api.listSavedWorks();
    set({ works });
  },

  save: async (provider, work) => {
    const saved = await api.saveWork({ provider, work });
    set((state) => ({ works: upsert(state.works, saved) }));
    return saved;
  },

  remove: async (id) => {
    await api.removeSavedWork(id);
    set((state) => ({ works: state.works.filter((saved) => saved.id !== id) }));
  },

  updateNote: async (id, note) => {
    const updated = await api.updateSavedWorkNote(id, note);
    set((state) => ({ works: upsert(state.works, updated) }));
  },

  markAcquired: async (provider, work, path) => {
    const marked = await api.markSavedWorkAcquired(provider, work, path);
    if (marked !== null) set((state) => ({ works: upsert(state.works, marked) }));
  },
}));

/**
 * The saved work a result is, if it is one: the same provider's result by its
 * id, or another provider's record of the same DOI. DOIs are compared without
 * regard to case, which is how they are defined; the backend's own match on
 * save is the canonical one, and this only has to agree with it for the DOIs
 * providers report in their canonical form.
 */
export function findSavedWork(
  works: SavedWork[],
  provider: string,
  work: LiteratureSearchResult,
): SavedWork | undefined {
  const doi = work.doi?.toLowerCase() ?? null;
  return works.find(
    (saved) =>
      (saved.provider === provider && saved.work.id === work.id) ||
      (doi !== null && saved.work.doi?.toLowerCase() === doi),
  );
}
