import { create } from "zustand";
import { createJSONStorage, persist, type StateStorage } from "zustand/middleware";
import { api } from "../services";
import { randomId } from "../lib/types";
import type {
  DocumentMetadata,
  MatchRef,
  PreviewData,
  ViewerMetadataStatus,
} from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";

export const VIEWER_SESSION_STORAGE_KEY = "wilkes.viewer-session";
const VIEWER_SESSION_VERSION = 1;
const MAX_PERSISTED_HISTORY_PER_TAB = 100;

export interface ViewerTab {
  id: string;
  path: string;
  match: MatchRef;
  history: MatchRef[];
  historyIndex: number;
  previewData: PreviewData | null;
  previewLoading: boolean;
  previewError: string | null;
  pdfLoadAttempt: number;
  metadata: DocumentMetadata | null;
  metadataStatus: ViewerMetadataStatus;
  requestId: number;
}

interface PersistedViewerTab {
  path: string;
  history: MatchRef[];
  historyIndex: number;
}

interface PersistedViewerState {
  tabs: PersistedViewerTab[];
  activePath: string | null;
}

interface ViewerStore {
  tabs: ViewerTab[];
  activeTabId: string | null;
  sessionHydrated: boolean;

  restoreSession: () => Promise<void>;
  openMatch: (match: MatchRef) => void;
  openFile: (path: string) => void;
  activateTab: (id: string) => void;
  retryTab: (id: string) => void;
  reportTabLoadError: (id: string, error: unknown) => void;
  closeTab: (id: string) => void;
  closePath: (path: string) => void;
  remapPathPrefix: (oldPath: string, newPath: string) => void;
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

function errorMessage(error: unknown): string {
  return error instanceof Error && error.message.trim()
    ? error.message
    : "Could not load this document";
}

function loadPreview(tabId: string, requestId: number, match: MatchRef): void {
  api
    .preview(match)
    .then((previewData) => {
      useViewerStore.setState((state) => ({
        tabs: replaceTab(state.tabs, tabId, (tab) =>
          tab.requestId === requestId
            ? {
                ...tab,
                previewData,
                previewLoading: false,
                previewError: null,
              }
            : tab,
        ),
      }));
    })
    .catch((error) => {
      console.error("Preview failed:", error);
      useViewerStore.setState((state) => ({
        tabs: replaceTab(state.tabs, tabId, (tab) =>
          tab.requestId === requestId
            ? {
                ...tab,
                previewData: null,
                previewLoading: false,
                previewError: errorMessage(error),
              }
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

function ensureTabLoaded(tabId: string, forcePreview = false): void {
  const tab = useViewerStore.getState().tabs.find((candidate) => candidate.id === tabId);
  if (!tab) return;

  if (forcePreview || (!tab.previewLoading && tab.previewData == null && tab.previewError == null)) {
    const requestId = tab.requestId + 1;
    useViewerStore.setState((state) => ({
      tabs: replaceTab(state.tabs, tabId, (candidate) => ({
        ...candidate,
        previewLoading: true,
        previewError: null,
        requestId,
      })),
    }));
    loadPreview(tabId, requestId, tab.match);
  }

  if (tab.metadataStatus === "idle") {
    useViewerStore.setState((state) => ({
      tabs: replaceTab(state.tabs, tabId, (candidate) => ({
        ...candidate,
        metadataStatus: "loading",
      })),
    }));
    loadMetadata(tabId, tab.path);
  }
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
            previewError: null,
            requestId,
          }
        : candidate,
    ),
  }));
  loadPreview(tabId, requestId, match);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isNonNegativeInteger(value: unknown): value is number {
  return Number.isInteger(value) && (value as number) >= 0;
}

function isMatchRef(value: unknown, path: string): value is MatchRef {
  if (!isRecord(value) || value.path !== path || !isRecord(value.origin)) return false;

  const origin = value.origin;
  const textFile = origin.TextFile;
  const pdfPage = origin.PdfPage;
  const validTextOrigin =
    isRecord(textFile) &&
    isNonNegativeInteger(textFile.line) &&
    isNonNegativeInteger(textFile.col);
  const validPdfOrigin =
    isRecord(pdfPage) &&
    isNonNegativeInteger(pdfPage.page) &&
    pdfPage.page > 0 &&
    (pdfPage.bbox === null ||
      (isRecord(pdfPage.bbox) &&
        isFiniteNumber(pdfPage.bbox.x) &&
        isFiniteNumber(pdfPage.bbox.y) &&
        isFiniteNumber(pdfPage.bbox.width) &&
        pdfPage.bbox.width >= 0 &&
        isFiniteNumber(pdfPage.bbox.height) &&
        pdfPage.bbox.height >= 0));
  if (!validTextOrigin && !validPdfOrigin) return false;

  if (value.text_range === undefined) return true;
  if (!isRecord(value.text_range)) return false;
  const { start, end } = value.text_range;
  return isNonNegativeInteger(start) && isNonNegativeInteger(end) && end >= start;
}

function restorePersistedState(value: unknown): Pick<ViewerStore, "tabs" | "activeTabId"> {
  if (!isRecord(value) || !Array.isArray(value.tabs)) {
    return { tabs: [], activeTabId: null };
  }

  const seenPaths = new Set<string>();
  const tabs: ViewerTab[] = [];
  for (const candidate of value.tabs) {
    if (!isRecord(candidate) || typeof candidate.path !== "string" || !candidate.path) {
      continue;
    }
    const path = candidate.path;
    if (seenPaths.has(path) || !Array.isArray(candidate.history)) continue;
    const history = candidate.history.filter((match) => isMatchRef(match, path));
    if (history.length === 0) continue;

    const requestedIndex = Number.isInteger(candidate.historyIndex)
      ? (candidate.historyIndex as number)
      : history.length - 1;
    const historyIndex = Math.min(Math.max(requestedIndex, 0), history.length - 1);
    seenPaths.add(path);
    tabs.push({
      id: randomId(),
      path,
      match: history[historyIndex],
      history,
      historyIndex,
      previewData: null,
      previewLoading: false,
      previewError: null,
      pdfLoadAttempt: 0,
      metadata: null,
      metadataStatus: "idle",
      requestId: 0,
    });
  }

  const activePath = typeof value.activePath === "string" ? value.activePath : null;
  const activeTab = tabs.find((tab) => tab.path === activePath) ?? tabs[0] ?? null;
  return { tabs, activeTabId: activeTab?.id ?? null };
}

function remapPath(path: string, oldPath: string, newPath: string): string {
  if (path === oldPath) return newPath;
  if (path.startsWith(`${oldPath}/`) || path.startsWith(`${oldPath}\\`)) {
    return newPath + path.slice(oldPath.length);
  }
  return path;
}

function persistedTab(tab: ViewerTab): PersistedViewerTab {
  const maxStart = Math.max(0, tab.history.length - MAX_PERSISTED_HISTORY_PER_TAB);
  const centeredStart = tab.historyIndex - Math.floor(MAX_PERSISTED_HISTORY_PER_TAB / 2);
  const start = Math.min(Math.max(centeredStart, 0), maxStart);
  const history = tab.history.slice(start, start + MAX_PERSISTED_HISTORY_PER_TAB);
  return {
    path: tab.path,
    history,
    historyIndex: tab.historyIndex - start,
  };
}

// Persistence is a convenience, never a precondition for opening or navigating
// documents. Browser privacy settings and storage quotas can reject writes, so
// isolate those failures from the viewer's in-memory state transitions.
const viewerSessionStorage: StateStorage = {
  getItem: (name) => {
    try {
      return localStorage.getItem(name);
    } catch (error) {
      console.error("Could not read the viewer session:", error);
      return null;
    }
  },
  setItem: (name, value) => {
    try {
      localStorage.setItem(name, value);
    } catch (error) {
      console.error("Could not persist the viewer session:", error);
    }
  },
  removeItem: (name) => {
    try {
      localStorage.removeItem(name);
    } catch (error) {
      console.error("Could not clear the viewer session:", error);
    }
  },
};

let restorePromise: Promise<void> | null = null;

export const useViewerStore = create<ViewerStore>()(
  persist(
    (set, get) => ({
      tabs: [],
      activeTabId: null,
      sessionHydrated: false,

      restoreSession: async () => {
        if (get().sessionHydrated) return;
        if (restorePromise) return restorePromise;
        restorePromise = (async () => {
          await useViewerStore.persist.rehydrate();
          useViewerStore.setState({ sessionHydrated: true });
          const activeTabId = useViewerStore.getState().activeTabId;
          if (activeTabId) ensureTabLoaded(activeTabId);
        })().finally(() => {
          restorePromise = null;
        });
        return restorePromise;
      },

      openMatch: (match) => {
        const existing = get().tabs.find((tab) => tab.path === match.path);
        if (existing) {
          if (sameMatch(existing.match, match)) {
            set({ activeTabId: existing.id });
            ensureTabLoaded(existing.id);
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
                previewError: null,
                requestId,
              };
            }),
          }));
          loadPreview(existing.id, requestId, match);
          ensureTabLoaded(existing.id);
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
          previewLoading: false,
          previewError: null,
          pdfLoadAttempt: 0,
          metadata: null,
          metadataStatus: "idle",
          requestId: 0,
        };
        set((state) => ({ tabs: [...state.tabs, tab], activeTabId: id }));
        ensureTabLoaded(id);
      },

      openFile: (path) => get().openMatch(directFileMatch(path)),

      activateTab: (id) => {
        if (!get().tabs.some((tab) => tab.id === id)) return;
        set({ activeTabId: id });
        ensureTabLoaded(id);
      },

      retryTab: (id) => {
        set((state) => ({
          tabs: replaceTab(state.tabs, id, (tab) => ({
            ...tab,
            previewError: null,
            pdfLoadAttempt: tab.pdfLoadAttempt + 1,
          })),
        }));
        ensureTabLoaded(id, true);
      },

      reportTabLoadError: (id, error) =>
        set((state) => ({
          tabs: replaceTab(state.tabs, id, (tab) => ({
            ...tab,
            previewLoading: false,
            previewError: errorMessage(error),
          })),
        })),

      closeTab: (id) => {
        set((state) => {
          const index = state.tabs.findIndex((tab) => tab.id === id);
          if (index < 0) return state;
          const tabs = state.tabs.filter((tab) => tab.id !== id);
          if (state.activeTabId !== id) return { tabs };
          return {
            tabs,
            activeTabId: tabs[index]?.id ?? tabs[index - 1]?.id ?? null,
          };
        });
        const activeTabId = get().activeTabId;
        if (activeTabId) ensureTabLoaded(activeTabId);
      },

      closePath: (path) => {
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
        });
        const activeTabId = get().activeTabId;
        if (activeTabId) ensureTabLoaded(activeTabId);
      },

      remapPathPrefix: (oldPath, newPath) => {
        const activeTabId = get().activeTabId;
        const activePath = get().tabs.find((tab) => tab.id === activeTabId)?.path;
        set((state) => ({
          tabs: state.tabs.map((tab) => {
            const path = remapPath(tab.path, oldPath, newPath);
            if (path === tab.path) return tab;
            const history = tab.history.map((match) => ({
              ...match,
              path: remapPath(match.path, oldPath, newPath),
            }));
            return {
              ...tab,
              path,
              history,
              match: history[tab.historyIndex],
              previewData: null,
              previewLoading: false,
              previewError: null,
              pdfLoadAttempt: tab.pdfLoadAttempt + 1,
              metadata: null,
              metadataStatus: "idle",
              requestId: tab.requestId + 1,
            };
          }),
        }));
        if (activeTabId && activePath && remapPath(activePath, oldPath, newPath) !== activePath) {
          ensureTabLoaded(activeTabId);
        }
      },

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
    }),
    {
      name: VIEWER_SESSION_STORAGE_KEY,
      version: VIEWER_SESSION_VERSION,
      storage: createJSONStorage(() => viewerSessionStorage),
      skipHydration: true,
      partialize: (state): PersistedViewerState => ({
        tabs: state.tabs.map(persistedTab),
        activePath:
          state.tabs.find((tab) => tab.id === state.activeTabId)?.path ?? null,
      }),
      migrate: () => ({ tabs: [], activePath: null }),
      merge: (persistedState, currentState) => ({
        ...currentState,
        ...restorePersistedState(persistedState),
      }),
    },
  ),
);

export function activeViewerTab(state: Pick<ViewerStore, "tabs" | "activeTabId">) {
  return state.tabs.find((tab) => tab.id === state.activeTabId) ?? null;
}
