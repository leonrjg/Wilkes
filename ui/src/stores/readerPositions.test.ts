import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  MAX_REMEMBERED_DOCUMENTS,
  READER_POSITIONS_PERSIST_DELAY_MS,
  READER_POSITIONS_STORAGE_KEY,
  flushReaderPositions,
  forgetReaderPositionsWorkspace,
  readTextViewMode,
  readerPositionStore as positions,
  saveTextViewMode,
  setReaderPositionPersistence,
  switchReaderPositionsWorkspace,
} from "./readerPositions";

const PAGE_THREE = { page: 3, offsetRatio: 0.25, horizontalRatio: 0.5, zoom: 1.4 };
const stored = (workspaceId: string) =>
  JSON.parse(localStorage.getItem(`${READER_POSITIONS_STORAGE_KEY}.${workspaceId}`) ?? "null");

/** What a restart looks like to this module: whatever is in memory is gone and
 *  the next read comes from storage. */
function restart(workspaceId: string) {
  switchReaderPositionsWorkspace(`${workspaceId}-elsewhere`);
  localStorage.removeItem(`${READER_POSITIONS_STORAGE_KEY}.${workspaceId}-elsewhere`);
  switchReaderPositionsWorkspace(workspaceId);
}

describe("readerPositions", () => {
  beforeEach(() => {
    localStorage.clear();
    setReaderPositionPersistence(true);
    switchReaderPositionsWorkspace("workspace-a");
    localStorage.clear();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("keeps a document's place across a restart", () => {
    positions.savePdf("asset://paper.pdf", PAGE_THREE);
    positions.saveTextScroll("/notes.md", "rendered", 0.6);
    positions.saveTextZoom("/notes.md", 1.3);
    saveTextViewMode("/notes.md", "source");
    vi.advanceTimersByTime(READER_POSITIONS_PERSIST_DELAY_MS);

    restart("workspace-a");

    expect(positions.readPdf("asset://paper.pdf")).toEqual(PAGE_THREE);
    expect(positions.readTextScroll("/notes.md", "rendered")).toBe(0.6);
    expect(positions.readTextScroll("/notes.md", "source")).toBeNull();
    expect(positions.readTextZoom("/notes.md")).toBe(1.3);
    expect(readTextViewMode("/notes.md")).toBe("source");
    expect(readTextViewMode("/other.md")).toBe("rendered");
  });

  it("writes once reading settles rather than on every scroll", () => {
    positions.saveTextScroll("/notes.md", "source", 0.1);
    vi.advanceTimersByTime(READER_POSITIONS_PERSIST_DELAY_MS - 1);
    positions.saveTextScroll("/notes.md", "source", 0.2);
    vi.advanceTimersByTime(READER_POSITIONS_PERSIST_DELAY_MS - 1);

    expect(stored("workspace-a")).toBeNull();

    vi.advanceTimersByTime(1);
    expect(stored("workspace-a").documents["/notes.md"].source).toBe(0.2);
  });

  it("writes what is waiting when the page goes away", () => {
    positions.savePdf("asset://paper.pdf", PAGE_THREE);

    window.dispatchEvent(new Event("pagehide"));

    expect(stored("workspace-a").documents["asset://paper.pdf"].pdf).toEqual(PAGE_THREE);
  });

  it("keeps workspaces apart and lands a pending write under the workspace it came from", () => {
    positions.saveTextScroll("/shared.md", "source", 0.3);

    switchReaderPositionsWorkspace("workspace-b");
    expect(stored("workspace-a").documents["/shared.md"].source).toBe(0.3);
    expect(positions.readTextScroll("/shared.md", "source")).toBeNull();

    positions.saveTextScroll("/shared.md", "source", 0.8);
    switchReaderPositionsWorkspace("workspace-a");

    expect(positions.readTextScroll("/shared.md", "source")).toBe(0.3);
    expect(stored("workspace-b").documents["/shared.md"].source).toBe(0.8);
  });

  it("forgets the least recently read documents beyond the cap", () => {
    for (let index = 0; index < MAX_REMEMBERED_DOCUMENTS; index += 1) {
      vi.setSystemTime(index);
      positions.saveTextScroll(`/doc-${index}.txt`, "source", 0.5);
    }
    // Reading the oldest again makes it recent, so the next one out goes instead.
    vi.setSystemTime(MAX_REMEMBERED_DOCUMENTS);
    positions.saveTextScroll("/doc-0.txt", "source", 0.7);
    vi.setSystemTime(MAX_REMEMBERED_DOCUMENTS + 1);
    positions.saveTextScroll("/newest.txt", "source", 0.1);
    flushReaderPositions();

    restart("workspace-a");

    expect(positions.readTextScroll("/doc-0.txt", "source")).toBe(0.7);
    expect(positions.readTextScroll("/doc-1.txt", "source")).toBeNull();
    expect(positions.readTextScroll("/doc-2.txt", "source")).toBe(0.5);
    expect(positions.readTextScroll("/newest.txt", "source")).toBe(0.1);
    expect(Object.keys(stored("workspace-a").documents)).toHaveLength(MAX_REMEMBERED_DOCUMENTS);
  });

  it("drops a malformed record, says so, and keeps the rest", () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem(
      `${READER_POSITIONS_STORAGE_KEY}.workspace-a`,
      JSON.stringify({
        version: 1,
        documents: {
          "asset://good.pdf": { pdf: PAGE_THREE, updatedAt: 1 },
          "asset://bad.pdf": { pdf: { ...PAGE_THREE, page: 0 }, updatedAt: 2 },
          "/bad.md": { source: 4, updatedAt: 3 },
        },
      }),
    );

    restart("workspace-a");

    expect(positions.readPdf("asset://good.pdf")).toEqual(PAGE_THREE);
    expect(positions.readPdf("asset://bad.pdf")).toBeNull();
    expect(positions.readTextScroll("/bad.md", "source")).toBeNull();
    expect(error).toHaveBeenCalledWith(
      "Dropped malformed remembered reader positions for:",
      ["asset://bad.pdf", "/bad.md"],
    );
  });

  it("starts empty, and says so, when storage holds an unsupported version", () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem(
      `${READER_POSITIONS_STORAGE_KEY}.workspace-a`,
      JSON.stringify({ version: 99, documents: { "/notes.md": { source: 0.5, updatedAt: 1 } } }),
    );

    restart("workspace-a");

    expect(positions.readTextScroll("/notes.md", "source")).toBeNull();
    expect(error).toHaveBeenCalled();
  });

  it("keeps a standalone window's positions in memory only", () => {
    setReaderPositionPersistence(false);
    positions.saveTextScroll("/outside.md", "rendered", 0.4);
    vi.runAllTimers();
    window.dispatchEvent(new Event("pagehide"));

    expect(positions.readTextScroll("/outside.md", "rendered")).toBe(0.4);
    expect(localStorage.length).toBe(0);
  });

  it("forgets a deleted workspace without a pending write bringing it back", () => {
    positions.saveTextScroll("/notes.md", "source", 0.5);
    flushReaderPositions();
    positions.saveTextScroll("/notes.md", "source", 0.6);

    forgetReaderPositionsWorkspace("workspace-a");
    vi.runAllTimers();

    expect(stored("workspace-a")).toBeNull();
  });
});
