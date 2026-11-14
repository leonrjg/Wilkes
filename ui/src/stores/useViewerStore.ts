import { create } from "zustand";
import { isPagedPath } from "../lib/documentFormats";
import { createJSONStorage, persist, type StateStorage } from "zustand/middleware";
import { api } from "../services";
import { randomId } from "../lib/types";
import type {
  DocumentMetadata,
  MatchRef,
  PreviewData,
  SourceOrigin,
  ViewerMetadataStatus,
} from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";
import {
  forgetReaderPositionsWorkspace,
  setReaderPositionPersistence,
  switchReaderPositionsWorkspace,
} from "./readerPositions";

export const VIEWER_SESSION_STORAGE_KEY = "wilkes.viewer-session";
let activeViewerWorkspaceId = "default";
let viewerPersistenceEnabled = true;
const workspaceStorageName = (name: string) =>
  activeViewerWorkspaceId === "default" ? name : `${name}.${activeViewerWorkspaceId}`;
const VIEWER_SESSION_VERSION = 1;
const MAX_PERSISTED_HISTORY_PER_TAB = 100;

export interface ViewerTab {
  id: string;
  path: string;
  /** The pane the tab sits in. */
  paneId: string;
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

/** One of the viewer's side-by-side panes of tabs. */
export interface ViewerPane {
  id: string;
  /** The tab the pane shows. */
  activeTabId: string | null;
}

interface PersistedViewerTab {
  path: string;
  history: MatchRef[];
  historyIndex: number;
  /** Index of the tab's pane. Absent from sessions saved before panes, which
   *  had one. */
  pane?: number;
}

interface PersistedViewerState {
  tabs: PersistedViewerTab[];
  /** The focused pane's tab. */
  activePath: string | null;
  /** Each pane's tab, left to right. Absent from sessions saved before panes. */
  paneActivePaths?: (string | null)[];
  focusedPane?: number;
}

interface ViewerStore {
  mode: "workspace" | "standalone";
  tabs: ViewerTab[];
  /** Left to right. Every pane holds at least one tab; none exist with no tabs. */
  panes: ViewerPane[];
  /** The pane whose tab is the document being worked with. */
  focusedPaneId: string | null;
  sessionHydrated: boolean;

  restoreSession: () => Promise<void>;
  enterStandaloneMode: () => void;
  switchWorkspace: (workspaceId: string) => Promise<void>;
  /** Drops the persisted tab session and reader positions a deleted workspace
   *  left behind. */
  forgetWorkspace: (workspaceId: string) => void;
  openMatch: (match: MatchRef) => void;
  openFile: (path: string, origin?: SourceOrigin | null) => void;
  activateTab: (id: string) => void;
  retryTab: (id: string) => void;
  reportTabLoadError: (id: string, error: unknown) => void;
  closeTab: (id: string) => void;
  closePath: (path: string) => void;
  remapPathPrefix: (oldPath: string, newPath: string) => void;
  goBack: () => void;
  goForward: () => void;
  clear: () => void;
  /** Make a pane the focused one, so its tab is the active document. */
  focusPane: (paneId: string) => void;
  /** Move a tab into a new pane just right of its own. */
  splitTab: (id: string) => void;
  /** Move a tab into another existing pane. */
  moveTabToPane: (id: string, paneId: string) => void;
  /** Close every tab in a pane, and with them the pane. */
  closePane: (paneId: string) => void;
  /** Bring every tab back into the leftmost pane. */
  joinPanes: () => void;
}

type PaneLayout = Pick<ViewerStore, "tabs" | "panes" | "focusedPaneId">;

const EMPTY_LAYOUT: PaneLayout = { tabs: [], panes: [], focusedPaneId: null };

/** Restore the pane invariants after tabs have been opened, closed or moved:
 *  a pane with no tabs is gone, a tab without a pane joins the leftmost one,
 *  each pane shows one of its own tabs, and the focus is on a pane that exists
 *  -- where the focused pane went away, the one that took its place, else the
 *  one left of it. `previous` is the pane order before the change. */
function settlePanes(
  tabs: ViewerTab[],
  panes: ViewerPane[],
  focusedPaneId: string | null,
  previous: ViewerPane[],
): PaneLayout {
  if (tabs.length === 0) return EMPTY_LAYOUT;
  let candidates = panes;
  if (candidates.length === 0) candidates = [{ id: randomId(), activeTabId: null }];
  const known = new Set(candidates.map((pane) => pane.id));
  const homeless = tabs.some((tab) => !known.has(tab.paneId));
  if (homeless) {
    const first = candidates[0].id;
    tabs = tabs.map((tab) => (known.has(tab.paneId) ? tab : { ...tab, paneId: first }));
  }
  const settled = candidates
    .filter((pane) => tabs.some((tab) => tab.paneId === pane.id))
    .map((pane) =>
      tabs.some((tab) => tab.id === pane.activeTabId && tab.paneId === pane.id)
        ? pane
        : { ...pane, activeTabId: tabs.find((tab) => tab.paneId === pane.id)!.id },
    );
  let focused = settled.find((pane) => pane.id === focusedPaneId)?.id ?? null;
  if (!focused) {
    const at = Math.max(0, previous.findIndex((pane) => pane.id === focusedPaneId));
    focused = (settled[Math.min(at, settled.length - 1)] ?? settled[0]).id;
  }
  return { tabs, panes: settled, focusedPaneId: focused };
}

/** The neighbour that takes a tab's place in its pane when it leaves: the
 *  next tab, else the previous one. */
function paneNeighbour(tabs: ViewerTab[], leaving: ViewerTab): string | null {
  const siblings = tabs.filter((tab) => tab.paneId === leaving.paneId);
  const index = siblings.findIndex((tab) => tab.id === leaving.id);
  return siblings[index + 1]?.id ?? siblings[index - 1]?.id ?? null;
}

function withPaneActive(panes: ViewerPane[], paneId: string, activeTabId: string | null): ViewerPane[] {
  return panes.map((pane) => (pane.id === paneId ? { ...pane, activeTabId } : pane));
}

/** Take a tab out of its pane, handing its place to its neighbour. */
function leavePane(state: PaneLayout, leaving: ViewerTab): ViewerPane[] {
  const pane = state.panes.find((candidate) => candidate.id === leaving.paneId);
  return pane?.activeTabId === leaving.id
    ? withPaneActive(state.panes, leaving.paneId, paneNeighbour(state.tabs, leaving))
    : state.panes;
}

/** Every pane's tab is on screen, so every one is loaded. */
function loadVisibleTabs(): void {
  for (const pane of useViewerStore.getState().panes) {
    if (pane.activeTabId) ensureTabLoaded(pane.activeTabId);
  }
}

/** The tab a pane shows. */
export function paneActiveTab(state: Pick<ViewerStore, "tabs" | "panes">, paneId: string): ViewerTab | null {
  const id = state.panes.find((pane) => pane.id === paneId)?.activeTabId;
  return state.tabs.find((tab) => tab.id === id) ?? null;
}

/** The tabs a pane holds, in order. */
export function paneTabs(state: Pick<ViewerStore, "tabs">, paneId: string): ViewerTab[] {
  return state.tabs.filter((tab) => tab.paneId === paneId);
}

function directFileMatch(path: string): MatchRef {
  return {
    path,
    // Every format the backend paginates, not PDF alone. A book given a text
    // origin here is one the pane never routes to a reader: it asks for the
    // text branches, the preview that arrives is page-shaped, and the chain
    // falls through to nothing — the document opens blank.
    origin: isPagedPath(path)
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
  const preview = useViewerStore.getState().mode === "standalone"
    ? api.previewStandalone
    : api.preview.bind(api);
  if (!preview) {
    useViewerStore.setState((state) => ({
      tabs: replaceTab(state.tabs, tabId, (tab) => ({
        ...tab,
        previewData: null,
        previewLoading: false,
        previewError: "Standalone document preview is unavailable",
      })),
    }));
    return;
  }
  preview(match)
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
  if (useViewerStore.getState().mode === "standalone") {
    const load = api.getStandaloneFileMetadata;
    if (!load) {
      updateMetadata(tabId, path, null, "failed");
      return;
    }
    load(path)
      .then((metadata) => updateMetadata(tabId, path, metadata, "ready"))
      .catch((error) => {
        console.error("Standalone metadata fetch failed:", error);
        updateMetadata(tabId, path, null, "failed");
      });
    return;
  }

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

function restorePersistedState(value: unknown): PaneLayout {
  if (!isRecord(value) || !Array.isArray(value.tabs)) {
    return EMPTY_LAYOUT;
  }

  // Panes are saved by position; their ids are this session's.
  const paneIds: string[] = [];
  const paneIdAt = (index: unknown): string => {
    const at = isNonNegativeInteger(index) ? index : 0;
    while (paneIds.length <= at) paneIds.push(randomId());
    return paneIds[at];
  };
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
      paneId: paneIdAt(candidate.pane),
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

  const idAt = (path: unknown) =>
    typeof path === "string" ? tabs.find((tab) => tab.path === path)?.id ?? null : null;
  const savedActives = Array.isArray(value.paneActivePaths) ? value.paneActivePaths : [];
  const panes = paneIds.map((id, index) => ({ id, activeTabId: idAt(savedActives[index]) }));
  const focusedIndex = isNonNegativeInteger(value.focusedPane) ? value.focusedPane : 0;
  const focusedPaneId = paneIds[focusedIndex] ?? paneIds[0] ?? null;
  // The focused pane's tab is also saved on its own, which is all a session
  // from before panes has.
  const activeId = idAt(value.activePath);
  const activeTab = tabs.find((tab) => tab.id === activeId);
  const focusedPanes = activeTab
    ? panes.map((pane) => (pane.id === activeTab.paneId ? { ...pane, activeTabId: activeTab.id } : pane))
    : panes;
  return settlePanes(tabs, focusedPanes, activeTab?.paneId ?? focusedPaneId, focusedPanes);
}

function remapPath(path: string, oldPath: string, newPath: string): string {
  if (path === oldPath) return newPath;
  if (path.startsWith(`${oldPath}/`) || path.startsWith(`${oldPath}\\`)) {
    return newPath + path.slice(oldPath.length);
  }
  return path;
}

function persistedTab(tab: ViewerTab, panes: ViewerPane[]): PersistedViewerTab {
  const maxStart = Math.max(0, tab.history.length - MAX_PERSISTED_HISTORY_PER_TAB);
  const centeredStart = tab.historyIndex - Math.floor(MAX_PERSISTED_HISTORY_PER_TAB / 2);
  const start = Math.min(Math.max(centeredStart, 0), maxStart);
  const history = tab.history.slice(start, start + MAX_PERSISTED_HISTORY_PER_TAB);
  return {
    path: tab.path,
    history,
    historyIndex: tab.historyIndex - start,
    pane: Math.max(0, panes.findIndex((pane) => pane.id === tab.paneId)),
  };
}

// Persistence is a convenience, never a precondition for opening or navigating
// documents. Browser privacy settings and storage quotas can reject writes, so
// isolate those failures from the viewer's in-memory state transitions.
const viewerSessionStorage: StateStorage = {
  getItem: (name) => {
    try {
      return localStorage.getItem(workspaceStorageName(name));
    } catch (error) {
      console.error("Could not read the viewer session:", error);
      return null;
    }
  },
  setItem: (name, value) => {
    if (!viewerPersistenceEnabled) return;
    try {
      localStorage.setItem(workspaceStorageName(name), value);
    } catch (error) {
      console.error("Could not persist the viewer session:", error);
    }
  },
  removeItem: (name) => {
    if (!viewerPersistenceEnabled) return;
    try {
      localStorage.removeItem(workspaceStorageName(name));
    } catch (error) {
      console.error("Could not clear the viewer session:", error);
    }
  },
};

let restorePromise: Promise<void> | null = null;

export const useViewerStore = create<ViewerStore>()(
  persist(
    (set, get) => ({
      mode: "workspace",
      ...EMPTY_LAYOUT,
      sessionHydrated: false,

      restoreSession: async () => {
        viewerPersistenceEnabled = true;
        setReaderPositionPersistence(true);
        if (get().sessionHydrated) return;
        if (restorePromise) return restorePromise;
        restorePromise = (async () => {
          await useViewerStore.persist.rehydrate();
          useViewerStore.setState({ sessionHydrated: true });
          loadVisibleTabs();
        })().finally(() => {
          restorePromise = null;
        });
        return restorePromise;
      },

      enterStandaloneMode: () => {
        viewerPersistenceEnabled = false;
        setReaderPositionPersistence(false);
        set({
          mode: "standalone",
          ...EMPTY_LAYOUT,
          sessionHydrated: true,
        });
      },

      switchWorkspace: async (workspaceId) => {
        viewerPersistenceEnabled = true;
        switchReaderPositionsWorkspace(workspaceId);
        let persisted: unknown = null;
        try {
          const raw = localStorage.getItem(`${VIEWER_SESSION_STORAGE_KEY}.${workspaceId}`);
          persisted = raw ? (JSON.parse(raw) as { state?: unknown }).state : null;
        } catch (error) {
          console.error("Could not read the workspace viewer session:", error);
        }
        activeViewerWorkspaceId = workspaceId;
        useViewerStore.setState({
          ...restorePersistedState(persisted),
          mode: "workspace",
          sessionHydrated: true,
        });
        loadVisibleTabs();
      },

      forgetWorkspace: (workspaceId) => {
        forgetReaderPositionsWorkspace(workspaceId);
        try {
          localStorage.removeItem(`${VIEWER_SESSION_STORAGE_KEY}.${workspaceId}`);
        } catch (error) {
          console.error("Could not clear the viewer session of a deleted workspace:", error);
        }
      },

      openMatch: (match) => {
        const existing = get().tabs.find((tab) => tab.path === match.path);
        if (existing) {
          // A path is open in one tab only: a document already open in another
          // pane is shown there, and that pane takes the focus.
          get().activateTab(existing.id);
          if (sameMatch(existing.match, match)) return;

          const requestId = existing.requestId + 1;
          set((state) => ({
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
        const paneId = get().focusedPaneId ?? randomId();
        const tab: ViewerTab = {
          id,
          path: match.path,
          paneId,
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
        set((state) => {
          const panes = state.panes.some((pane) => pane.id === paneId)
            ? withPaneActive(state.panes, paneId, id)
            : [...state.panes, { id: paneId, activeTabId: id }];
          return settlePanes([...state.tabs, tab], panes, paneId, state.panes);
        });
        ensureTabLoaded(id);
      },

      // An origin is where the caller was told to land -- a link naming a
      // page, say. Without one the file is opened wherever a plain open goes,
      // which is the only place the path itself can say.
      openFile: (path, origin) =>
        get().openMatch(origin ? { path, origin } : directFileMatch(path)),

      activateTab: (id) => {
        const tab = get().tabs.find((candidate) => candidate.id === id);
        if (!tab) return;
        set((state) => settlePanes(state.tabs, withPaneActive(state.panes, tab.paneId, id), tab.paneId, state.panes));
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
          const closing = state.tabs.find((tab) => tab.id === id);
          if (!closing) return state;
          return settlePanes(
            state.tabs.filter((tab) => tab.id !== id),
            leavePane(state, closing),
            state.focusedPaneId,
            state.panes,
          );
        });
        loadVisibleTabs();
      },

      // A path is open in one tab at most.
      closePath: (path) => {
        const tab = get().tabs.find((candidate) => candidate.path === path);
        if (tab) get().closeTab(tab.id);
      },

      remapPathPrefix: (oldPath, newPath) => {
        const visible = get().panes.map((pane) => ({
          id: pane.activeTabId,
          path: get().tabs.find((tab) => tab.id === pane.activeTabId)?.path,
        }));
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
        for (const { id, path } of visible) {
          if (id && path && remapPath(path, oldPath, newPath) !== path) ensureTabLoaded(id);
        }
      },

      goBack: () => {
        const tab = activeViewerTab(get());
        if (tab && tab.historyIndex > 0) {
          navigateToHistoryIndex(tab.id, tab.historyIndex - 1);
        }
      },

      goForward: () => {
        const tab = activeViewerTab(get());
        if (tab && tab.historyIndex < tab.history.length - 1) {
          navigateToHistoryIndex(tab.id, tab.historyIndex + 1);
        }
      },

      clear: () => set(EMPTY_LAYOUT),

      focusPane: (paneId) => {
        if (!get().panes.some((pane) => pane.id === paneId)) return;
        set({ focusedPaneId: paneId });
      },

      splitTab: (id) => {
        set((state) => {
          const moving = state.tabs.find((tab) => tab.id === id);
          if (!moving) return state;
          const fresh: ViewerPane = { id: randomId(), activeTabId: id };
          const panes = leavePane(state, moving);
          const at = panes.findIndex((pane) => pane.id === moving.paneId);
          return settlePanes(
            replaceTab(state.tabs, id, (tab) => ({ ...tab, paneId: fresh.id })),
            [...panes.slice(0, at + 1), fresh, ...panes.slice(at + 1)],
            fresh.id,
            state.panes,
          );
        });
        loadVisibleTabs();
      },

      moveTabToPane: (id, paneId) => {
        set((state) => {
          const moving = state.tabs.find((tab) => tab.id === id);
          if (!moving || moving.paneId === paneId || !state.panes.some((pane) => pane.id === paneId)) {
            return state;
          }
          return settlePanes(
            replaceTab(state.tabs, id, (tab) => ({ ...tab, paneId })),
            withPaneActive(leavePane(state, moving), paneId, id),
            paneId,
            state.panes,
          );
        });
        loadVisibleTabs();
      },

      closePane: (paneId) => {
        set((state) => settlePanes(
          state.tabs.filter((tab) => tab.paneId !== paneId),
          state.panes,
          state.focusedPaneId,
          state.panes,
        ));
        loadVisibleTabs();
      },

      joinPanes: () => {
        set((state) => {
          const first = state.panes[0];
          if (!first) return state;
          // The tab being worked with stays the one shown.
          const shown = activeViewerTab(state)?.id ?? first.activeTabId;
          return settlePanes(
            state.tabs.map((tab) => ({ ...tab, paneId: first.id })),
            [{ id: first.id, activeTabId: shown }],
            first.id,
            state.panes,
          );
        });
        loadVisibleTabs();
      },
    }),
    {
      name: VIEWER_SESSION_STORAGE_KEY,
      version: VIEWER_SESSION_VERSION,
      storage: createJSONStorage(() => viewerSessionStorage),
      skipHydration: true,
      partialize: (state): PersistedViewerState => ({
        tabs: state.tabs.map((tab) => persistedTab(tab, state.panes)),
        activePath: activeViewerTab(state)?.path ?? null,
        paneActivePaths: state.panes.map(
          (pane) => state.tabs.find((tab) => tab.id === pane.activeTabId)?.path ?? null,
        ),
        focusedPane: Math.max(0, state.panes.findIndex((pane) => pane.id === state.focusedPaneId)),
      }),
      migrate: () => ({ tabs: [], activePath: null }),
      merge: (persistedState, currentState) => ({
        ...currentState,
        ...restorePersistedState(persistedState),
      }),
    },
  ),
);

/** The focused pane's tab: the document being worked with. */
export function activeViewerTab(state: PaneLayout): ViewerTab | null {
  return state.focusedPaneId ? paneActiveTab(state, state.focusedPaneId) : null;
}
