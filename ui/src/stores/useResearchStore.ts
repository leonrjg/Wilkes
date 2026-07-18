import { create } from "zustand";
import { api } from "../services";
import type {
  DocumentTagUpdate,
  NewSmartCollection,
  NewTag,
  SearchLogEntry,
  SmartCollection,
  Tag,
} from "../lib/types";

interface ResearchStore {
  tags: Tag[];
  collections: SmartCollection[];
  history: SearchLogEntry[];
  selectedCollectionId: string | null;
  selectedTagId: string | null;
  draftCollectionExpression: string | null;
  loading: boolean;
  load: () => Promise<void>;
  loadHistory: () => Promise<void>;
  setSelectedCollection: (id: string | null) => void;
  setSelectedTag: (id: string | null) => void;
  setDraftCollectionExpression: (expression: string | null) => void;
  createTag: (tag: NewTag) => Promise<Tag>;
  updateTag: (id: string, tag: NewTag) => Promise<void>;
  deleteTag: (id: string) => Promise<void>;
  updateDocumentTags: (update: DocumentTagUpdate) => Promise<void>;
  saveCollection: (id: string | null, collection: NewSmartCollection) => Promise<SmartCollection>;
  deleteCollection: (id: string) => Promise<void>;
  deleteHistory: (id: string) => Promise<void>;
  clearHistory: () => Promise<void>;
}

export const useResearchStore = create<ResearchStore>((set, get) => ({
  tags: [],
  collections: [],
  history: [],
  selectedCollectionId: null,
  selectedTagId: null,
  draftCollectionExpression: null,
  loading: false,
  load: async () => {
    set({ loading: true });
    try {
      const [tags, collections] = await Promise.all([api.listTags(), api.listSmartCollections()]);
      const selected = get().selectedCollectionId;
      const selectedTag = get().selectedTagId;
      set({
        tags,
        collections,
        selectedCollectionId: selected && collections.some((c) => c.id === selected) ? selected : null,
        selectedTagId: selectedTag && tags.some((tag) => tag.id === selectedTag) ? selectedTag : null,
      });
    } finally {
      set({ loading: false });
    }
  },
  loadHistory: async () => set({ history: await api.listSearchLog(250) }),
  setSelectedCollection: (selectedCollectionId) => set({ selectedCollectionId }),
  setSelectedTag: (selectedTagId) => set({ selectedTagId }),
  setDraftCollectionExpression: (draftCollectionExpression) => set({ draftCollectionExpression }),
  createTag: async (tag) => {
    const created = await api.createTag(tag);
    await get().load();
    return created;
  },
  updateTag: async (id, tag) => { await api.updateTag(id, tag); await get().load(); },
  deleteTag: async (id) => {
    await api.deleteTag(id);
    if (get().selectedTagId === id) set({ selectedTagId: null });
    await get().load();
  },
  updateDocumentTags: async (update) => { await api.updateDocumentTags(update); },
  saveCollection: async (id, collection) => {
    const saved = id
      ? await api.updateSmartCollection(id, collection)
      : await api.createSmartCollection(collection);
    await get().load();
    return saved;
  },
  deleteCollection: async (id) => {
    await api.deleteSmartCollection(id);
    if (get().selectedCollectionId === id) set({ selectedCollectionId: null });
    await get().load();
  },
  deleteHistory: async (id) => { await api.deleteSearchLog(id); await get().loadHistory(); },
  clearHistory: async () => { await api.clearSearchLog(); set({ history: [] }); },
}));
