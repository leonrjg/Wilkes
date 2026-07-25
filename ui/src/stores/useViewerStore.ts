import { create } from "zustand";
import { api } from "../services";
import { randomId } from "../lib/types";
import type {
  DocumentMetadata,
  MatchRef,
  PreviewData,
  ViewerMetadataStatus,
} from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";

export interface ViewerTab {
  id: string;
  path: string;
  match: MatchRef;
  history: MatchRef[];
  historyIndex: number;
  previewData: PreviewData | null;
  previewLoading: boolean;
  metadata: DocumentMetadata | null;
  metadataStatus: ViewerMetadataStatus;
  requestId: number;
}

interface ViewerStore {
  tabs: ViewerTab[];
  activeTabId: string | null;

  openMatch: (match: MatchRef) => void;
  openFile: (path: string) => void;
  activateTab: (id: string) => void;
  closeTab: (id: string) => void;
  closePath: (path: string) => void;
  goBack: () => void;
  goForward: () => void;
  clear: () => void;
}

function directFileMatch(path: string): MatchRef {
  return {
    path,
    origin: path.toLowerCase().endsWith(".pdf")
      ? { PdfPage: { page: 1, bbox: null } }
      : { TextFile: { line: 0, col: 0 } },
  };
}

function sameMatch(a: MatchRef, b: MatchRef): boolean {
  return (
    a.path === b.path &&
    JSON.stringify(a.origin) === JSON.stringify(b.origin) &&
    JSON.stringify(a.text_range) === JSON.stringify(b.text_range)
  );
}

function replaceTab(
  tabs: ViewerTab[],
  id: string,
  update: (tab: ViewerTab) => ViewerTab,
): ViewerTab[] {
  return tabs.map((tab) => (tab.id === id ? update(tab) : tab));
}

function loadPreview(tabId: string, requestId: number, match: MatchRef): void {
  api
    .preview(match)
    .then((previewData) => {
      useViewerStore.setState((state) => ({
        tabs: replaceTab(state.tabs, tabId, (tab) =>
          tab.requestId === requestId
            ? { ...tab, previewData, previewLoading: false }
            : tab,
        ),
      }));
    })
    .catch((error) => {
      console.error("Preview failed:", error);
      useViewerStore.setState((state) => ({
        tabs: replaceTab(state.tabs, tabId, (tab) =>
          tab.requestId === requestId
            ? { ...tab, previewData: null, previewLoading: false }
            : tab,
        ),
      }));
    });
}

function updateMetadata(
  tabId: string,
  path: string,
  metadata: DocumentMetadata | null,
  metadataStatus: ViewerMetadataStatus,
): void {
  useViewerStore.setState((state) => ({
    tabs: replaceTab(state.tabs, tabId, (tab) =>
      tab.path === path ? { ...tab, metadata, metadataStatus } : tab,
    ),
  }));
}

function loadMetadata(tabId: string, path: string): void {
  const upgradeAuthoritative = () => {
    if (!useSettingsStore.getState().settings?.integrations.zotero.enabled) return;
    api
      .resolveFileMetadata(path)
      .then((metadata) => updateMetadata(tabId, path, metadata, "ready"))
      .catch((error) => {
        // Preserve the fast file-based metadata when authoritative resolution
        // is unavailable.
        console.debug("Authoritative metadata resolve skipped:", error);
      });
  };

  api
    .getFileMetadata(path)
    .then((metadata) => updateMetadata(tabId, path, metadata, "ready"))
    .catch((error) => {
      console.error("Metadata fetch failed:", error);
      updateMetadata(tabId, path, null, "failed");
    })
    .finally(upgradeAuthoritative);
}

function navigateToHistoryIndex(tabId: string, historyIndex: number): void {
  const tab = useViewerStore.getState().tabs.find((candidate) => candidate.id === tabId);
  const match = tab?.history[historyIndex];
  if (!tab || !match) return;
  const requestId = tab.requestId + 1;
  useViewerStore.setState((state) => ({
    tabs: replaceTab(state.tabs, tabId, (candidate) =>
      candidate.requestId === tab.requestId
        ? {
            ...candidate,
            match,
            historyIndex,
            previewLoading: true,
            requestId,
          }
        : candidate,
    ),
  }));
  loadPreview(tabId, requestId, match);
}

export const useViewerStore = create<ViewerStore>((set, get) => ({
  tabs: [],
  activeTabId: null,

  openMatch: (match) => {
    const existing = get().tabs.find((tab) => tab.path === match.path);
    if (existing) {
      if (sameMatch(existing.match, match)) {
        set({ activeTabId: existing.id });
        if (!existing.previewLoading && existing.previewData == null) {
          const requestId = existing.requestId + 1;
          set((state) => ({
            tabs: replaceTab(state.tabs, existing.id, (tab) => ({
              ...tab,
              previewLoading: true,
              requestId,
            })),
          }));
          loadPreview(existing.id, requestId, match);
        }
        return;
      }

      const requestId = existing.requestId + 1;
      set((state) => ({
        activeTabId: existing.id,
        tabs: replaceTab(state.tabs, existing.id, (tab) => {
          const history = [...tab.history.slice(0, tab.historyIndex + 1), match];
          return {
            ...tab,
            match,
            history,
            historyIndex: history.length - 1,
            previewLoading: true,
            requestId,
          };
        }),
      }));
      loadPreview(existing.id, requestId, match);
      return;
    }

    const id = randomId();
    const tab: ViewerTab = {
      id,
      path: match.path,
      match,
      history: [match],
      historyIndex: 0,
      previewData: null,
      previewLoading: true,
      metadata: null,
      metadataStatus: "loading",
      requestId: 1,
    };
    set((state) => ({ tabs: [...state.tabs, tab], activeTabId: id }));
    loadPreview(id, tab.requestId, match);
    loadMetadata(id, match.path);
  },

  openFile: (path) => get().openMatch(directFileMatch(path)),

  activateTab: (id) => {
    if (get().tabs.some((tab) => tab.id === id)) set({ activeTabId: id });
  },

  closeTab: (id) =>
    set((state) => {
      const index = state.tabs.findIndex((tab) => tab.id === id);
      if (index < 0) return state;
      const tabs = state.tabs.filter((tab) => tab.id !== id);
      if (state.activeTabId !== id) return { tabs };
      return {
        tabs,
        activeTabId: tabs[index]?.id ?? tabs[index - 1]?.id ?? null,
      };
    }),

  closePath: (path) =>
    set((state) => {
      const tabs = state.tabs.filter((tab) => tab.path !== path);
      if (tabs.length === state.tabs.length) return state;
      const activeIndex = state.tabs.findIndex((tab) => tab.id === state.activeTabId);
      const activeTab = state.tabs[activeIndex];
      if (activeTab?.path !== path) return { tabs };
      const nextActive =
        state.tabs.slice(activeIndex + 1).find((tab) => tab.path !== path) ??
        state.tabs.slice(0, activeIndex).reverse().find((tab) => tab.path !== path) ??
        null;
      return { tabs, activeTabId: nextActive?.id ?? null };
    }),

  goBack: () => {
    const state = get();
    const tab = state.tabs.find((candidate) => candidate.id === state.activeTabId);
    if (tab && tab.historyIndex > 0) {
      navigateToHistoryIndex(tab.id, tab.historyIndex - 1);
    }
  },

  goForward: () => {
    const state = get();
    const tab = state.tabs.find((candidate) => candidate.id === state.activeTabId);
    if (tab && tab.historyIndex < tab.history.length - 1) {
      navigateToHistoryIndex(tab.id, tab.historyIndex + 1);
    }
  },

  clear: () => set({ tabs: [], activeTabId: null }),
}));

export function activeViewerTab(state: Pick<ViewerStore, "tabs" | "activeTabId">) {
  return state.tabs.find((tab) => tab.id === state.activeTabId) ?? null;
}
