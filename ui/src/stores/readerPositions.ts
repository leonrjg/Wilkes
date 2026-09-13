import {
  TEXT_MAX_ZOOM,
  TEXT_MIN_ZOOM,
  type PdfScrollPosition,
  type ReaderPositionStore,
  type TextViewerMode,
} from "@leonrjg/wilkes-reader";

/**
 * Where each document was left, kept across restarts for the active workspace.
 *
 * The readers capture and restore the geometry and ask this store for it
 * synchronously, so it answers from an in-memory copy of the workspace's record
 * and writes that copy to localStorage shortly after reading stops. Like the tab
 * session beside it, it is a convenience and never a precondition: storage that
 * refuses a read or a write is logged and reading carries on from memory.
 *
 * Keyed by what the readers key on: a PDF's document key (the asset URL the
 * webview loads, or a book's path) and a text document's path. A renamed or
 * moved document starts again from its beginning; its old record ages out under
 * `MAX_REMEMBERED_DOCUMENTS`.
 *
 * Which presentation a Markdown or HTML document was last read in lives here
 * too. No reader reads it -- PreviewPane chooses which reader to mount -- but it
 * is the same kind of fact about the same document, and a restored scroll
 * position is only right on the surface it was taken on.
 */

export const READER_POSITIONS_STORAGE_KEY = "wilkes.reader-positions";
const READER_POSITIONS_VERSION = 1;
export const MAX_REMEMBERED_DOCUMENTS = 500;
export const READER_POSITIONS_PERSIST_DELAY_MS = 500;

interface DocumentReading {
  pdf?: PdfScrollPosition;
  source?: number;
  rendered?: number;
  zoom?: number;
  viewMode?: TextViewerMode;
  updatedAt: number;
}

interface PersistedReaderPositions {
  version: number;
  documents: Record<string, DocumentReading>;
}

let activeWorkspaceId = "default";
let persistenceEnabled = true;
/** Most recently updated last, so eviction takes from the front. Loaded on first
 *  use after a workspace becomes active. */
let documents: Map<string, DocumentReading> | null = null;
let persistTimer: ReturnType<typeof setTimeout> | null = null;

const storageKey = (workspaceId: string) => `${READER_POSITIONS_STORAGE_KEY}.${workspaceId}`;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function isRatio(value: unknown): value is number {
  return isFiniteNumber(value) && value >= 0 && value <= 1;
}

function isPdfPosition(value: unknown): value is PdfScrollPosition {
  return (
    isRecord(value) &&
    Number.isInteger(value.page) &&
    (value.page as number) >= 1 &&
    isRatio(value.offsetRatio) &&
    // Exceeds 1 when the page is narrower than the viewport.
    isFiniteNumber(value.horizontalRatio) &&
    value.horizontalRatio >= 0 &&
    isFiniteNumber(value.zoom) &&
    value.zoom > 0
  );
}

function isDocumentReading(value: unknown): value is DocumentReading {
  if (!isRecord(value) || !isFiniteNumber(value.updatedAt)) return false;
  if (value.pdf !== undefined && !isPdfPosition(value.pdf)) return false;
  if (value.source !== undefined && !isRatio(value.source)) return false;
  if (value.rendered !== undefined && !isRatio(value.rendered)) return false;
  if (
    value.zoom !== undefined &&
    !(isFiniteNumber(value.zoom) && value.zoom >= TEXT_MIN_ZOOM && value.zoom <= TEXT_MAX_ZOOM)
  ) {
    return false;
  }
  return value.viewMode === undefined || value.viewMode === "source" || value.viewMode === "rendered";
}

function load(): Map<string, DocumentReading> {
  const loaded = new Map<string, DocumentReading>();
  if (!persistenceEnabled) return loaded;

  let raw: string | null;
  try {
    raw = localStorage.getItem(storageKey(activeWorkspaceId));
  } catch (error) {
    console.error("Could not read the remembered reader positions:", error);
    return loaded;
  }
  if (raw === null) return loaded;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error("Remembered reader positions are not valid JSON; starting without them:", error);
    return loaded;
  }
  if (!isRecord(parsed) || parsed.version !== READER_POSITIONS_VERSION || !isRecord(parsed.documents)) {
    console.error(
      `Remembered reader positions have an unsupported shape (version ${
        isRecord(parsed) ? String(parsed.version) : "none"
      }); starting without them.`,
    );
    return loaded;
  }

  const rejected: string[] = [];
  const valid: [string, DocumentReading][] = [];
  for (const [key, reading] of Object.entries(parsed.documents)) {
    if (isDocumentReading(reading)) valid.push([key, reading]);
    else rejected.push(key);
  }
  if (rejected.length > 0) {
    console.error("Dropped malformed remembered reader positions for:", rejected);
  }
  valid.sort(([, a], [, b]) => a.updatedAt - b.updatedAt);
  for (const [key, reading] of valid) loaded.set(key, reading);
  return loaded;
}

function current(): Map<string, DocumentReading> {
  documents ??= load();
  return documents;
}

function writeNow(): void {
  if (persistTimer !== null) {
    clearTimeout(persistTimer);
    persistTimer = null;
  }
  if (!persistenceEnabled || documents === null) return;
  const persisted: PersistedReaderPositions = {
    version: READER_POSITIONS_VERSION,
    documents: Object.fromEntries(documents),
  };
  try {
    localStorage.setItem(storageKey(activeWorkspaceId), JSON.stringify(persisted));
  } catch (error) {
    console.error("Could not persist the reader positions:", error);
  }
}

function update(key: string, change: (reading: DocumentReading) => DocumentReading): void {
  const map = current();
  const previous = map.get(key) ?? { updatedAt: 0 };
  // Re-inserted so the map stays ordered by recency.
  map.delete(key);
  map.set(key, { ...change(previous), updatedAt: Date.now() });
  while (map.size > MAX_REMEMBERED_DOCUMENTS) {
    const oldest = map.keys().next().value as string;
    map.delete(oldest);
  }
  if (!persistenceEnabled) return;
  // The readers save on every scroll event; storage hears about it once they
  // settle.
  if (persistTimer !== null) clearTimeout(persistTimer);
  persistTimer = setTimeout(writeNow, READER_POSITIONS_PERSIST_DELAY_MS);
}

/** What the readers are handed. Stable for the life of the application: which
 *  workspace it answers for is switched underneath it. */
export const readerPositionStore: ReaderPositionStore = {
  readPdf: (documentKey) => current().get(documentKey)?.pdf ?? null,
  savePdf: (documentKey, position) => update(documentKey, (reading) => ({ ...reading, pdf: position })),
  readTextScroll: (documentPath, mode) => current().get(documentPath)?.[mode] ?? null,
  saveTextScroll: (documentPath, mode, ratio) =>
    update(documentPath, (reading) => ({ ...reading, [mode]: ratio })),
  readTextZoom: (documentPath) => current().get(documentPath)?.zoom ?? null,
  saveTextZoom: (documentPath, zoom) => update(documentPath, (reading) => ({ ...reading, zoom })),
};

export function readTextViewMode(path: string): TextViewerMode {
  return current().get(path)?.viewMode ?? "rendered";
}

export function saveTextViewMode(path: string, mode: TextViewerMode): void {
  update(path, (reading) => ({ ...reading, viewMode: mode }));
}

/** Writes whatever is waiting now rather than after the delay. */
export function flushReaderPositions(): void {
  writeNow();
}

/** Called with the viewer session's own switch: the outgoing workspace's pending
 *  write lands under its own key before the incoming one is read. */
export function switchReaderPositionsWorkspace(workspaceId: string): void {
  writeNow();
  persistenceEnabled = true;
  activeWorkspaceId = workspaceId;
  documents = null;
}

/** Mirrors the viewer session's persistence switch: a standalone document window
 *  reads and writes nothing, and keeps what it is told in memory only. */
export function setReaderPositionPersistence(enabled: boolean): void {
  if (enabled === persistenceEnabled) return;
  writeNow();
  persistenceEnabled = enabled;
  documents = null;
}

export function forgetReaderPositionsWorkspace(workspaceId: string): void {
  if (workspaceId === activeWorkspaceId) {
    if (persistTimer !== null) clearTimeout(persistTimer);
    persistTimer = null;
    documents = null;
  }
  try {
    localStorage.removeItem(storageKey(workspaceId));
  } catch (error) {
    console.error("Could not clear the reader positions of a deleted workspace:", error);
  }
}

if (typeof window !== "undefined") {
  // localStorage is synchronous, so a write started while the page is going
  // away completes; a hidden window is the last reliable moment on some
  // platforms, where closing never delivers `pagehide`.
  window.addEventListener("pagehide", writeNow);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") writeNow();
  });
}
