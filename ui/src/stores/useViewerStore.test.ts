import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import type { MatchRef, PreviewData } from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";
import { activeViewerTab, useViewerStore } from "./useViewerStore";

vi.mock("../services", () => ({
  api: {
    preview: vi.fn(),
    getFileMetadata: vi.fn(),
    resolveFileMetadata: vi.fn(),
  },
}));

const metadata = {
  title: "Document",
  author: null,
  doi: null,
  created_at: null,
};

const textPreview = (content: string, line = 1): PreviewData => ({
  Text: {
    content,
    language: "text",
    highlight_line: line,
    highlight_range: { start: 0, end: content.length },
  },
});

const textMatch = (path: string, line = 1): MatchRef => ({
  path,
  origin: { TextFile: { line, col: 0 } },
});

describe("useViewerStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useViewerStore.getState().clear();
    useSettingsStore.setState({ settings: null });
    vi.mocked(api.preview).mockImplementation(async (match) =>
      textPreview(match.path, "TextFile" in match.origin ? match.origin.TextFile.line : 1),
    );
    vi.mocked(api.getFileMetadata).mockResolvedValue(metadata);
  });

  it("opens direct text and PDF files at their natural starting locations", () => {
    useViewerStore.getState().openFile("/docs/notes.txt");
    useViewerStore.getState().openFile("/docs/paper.PDF");

    expect(useViewerStore.getState().tabs.map((tab) => tab.match.origin)).toEqual([
      { TextFile: { line: 0, col: 0 } },
      { PdfPage: { page: 1, bbox: null } },
    ]);
  });

  it("creates one tab per path and activates an existing tab on reopen", () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt"));
    const firstId = useViewerStore.getState().activeTabId;
    useViewerStore.getState().openMatch(textMatch("/docs/two.txt"));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt"));

    expect(useViewerStore.getState().tabs).toHaveLength(2);
    expect(useViewerStore.getState().activeTabId).toBe(firstId);
    expect(api.getFileMetadata).toHaveBeenCalledTimes(2);
  });

  it("keeps independent histories per tab", () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 5));
    const firstId = useViewerStore.getState().activeTabId!;
    useViewerStore.getState().openMatch(textMatch("/docs/two.txt", 3));
    const secondId = useViewerStore.getState().activeTabId!;

    useViewerStore.getState().goBack();
    expect(activeViewerTab(useViewerStore.getState())?.match.origin).toEqual({
      TextFile: { line: 3, col: 0 },
    });

    useViewerStore.getState().activateTab(firstId);
    useViewerStore.getState().goBack();
    expect(activeViewerTab(useViewerStore.getState())?.match.origin).toEqual({
      TextFile: { line: 1, col: 0 },
    });
    expect(
      useViewerStore.getState().tabs.find((tab) => tab.id === secondId)?.historyIndex,
    ).toBe(0);
  });

  it("truncates forward history after navigating back", () => {
    const store = useViewerStore.getState();
    store.openMatch(textMatch("/docs/one.txt", 1));
    store.openMatch(textMatch("/docs/one.txt", 2));
    store.openMatch(textMatch("/docs/one.txt", 3));
    useViewerStore.getState().goBack();
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 8));
    useViewerStore.getState().goForward();

    const tab = activeViewerTab(useViewerStore.getState())!;
    expect(tab.history.map((match) => (match.origin as any).TextFile.line)).toEqual([
      1, 2, 8,
    ]);
    expect((tab.match.origin as any).TextFile.line).toBe(8);
  });

  it("selects the right neighbor, then the left, when closing active tabs", () => {
    useViewerStore.getState().openFile("/docs/one.txt");
    useViewerStore.getState().openFile("/docs/two.txt");
    useViewerStore.getState().openFile("/docs/three.txt");
    const [one, two, three] = useViewerStore.getState().tabs;

    useViewerStore.getState().activateTab(two.id);
    useViewerStore.getState().closeTab(two.id);
    expect(useViewerStore.getState().activeTabId).toBe(three.id);

    useViewerStore.getState().closeTab(three.id);
    expect(useViewerStore.getState().activeTabId).toBe(one.id);

    useViewerStore.getState().closeTab(one.id);
    expect(useViewerStore.getState()).toEqual(
      expect.objectContaining({ tabs: [], activeTabId: null }),
    );
  });

  it("does not change the active tab when closing an inactive path", () => {
    useViewerStore.getState().openFile("/docs/one.txt");
    const oneId = useViewerStore.getState().activeTabId;
    useViewerStore.getState().openFile("/docs/two.txt");
    const twoId = useViewerStore.getState().activeTabId;

    useViewerStore.getState().closePath("/docs/one.txt");

    expect(useViewerStore.getState().activeTabId).toBe(twoId);
    expect(useViewerStore.getState().tabs.map((tab) => tab.id)).not.toContain(oneId);
  });

  it("keeps each tab's cached preview when switching tabs", async () => {
    vi.mocked(api.preview)
      .mockResolvedValueOnce(textPreview("first"))
      .mockResolvedValueOnce(textPreview("second"));

    useViewerStore.getState().openFile("/docs/one.txt");
    const firstId = useViewerStore.getState().activeTabId!;
    useViewerStore.getState().openFile("/docs/two.txt");
    await vi.waitFor(() =>
      expect(useViewerStore.getState().tabs.every((tab) => !tab.previewLoading)).toBe(true),
    );

    useViewerStore.getState().activateTab(firstId);

    expect(activeViewerTab(useViewerStore.getState())?.previewData).toEqual(
      textPreview("first"),
    );
    expect(api.preview).toHaveBeenCalledTimes(2);
  });

  it("ignores a late preview response after newer navigation in the same tab", async () => {
    let resolveFirst: ((preview: PreviewData) => void) | undefined;
    vi.mocked(api.preview)
      .mockImplementationOnce(
        () => new Promise((resolve) => {
          resolveFirst = resolve;
        }),
      )
      .mockResolvedValueOnce(textPreview("new", 2));

    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 2));
    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.previewData).toEqual(
        textPreview("new", 2),
      ),
    );
    resolveFirst?.(textPreview("stale", 1));
    await Promise.resolve();

    expect(activeViewerTab(useViewerStore.getState())?.previewData).toEqual(
      textPreview("new", 2),
    );
  });

  it("loads metadata once per document and upgrades it through Zotero", async () => {
    useSettingsStore.setState({
      settings: { integrations: { zotero: { enabled: true } } },
    } as never);
    const authoritative = { ...metadata, title: "Authoritative" };
    vi.mocked(api.resolveFileMetadata).mockResolvedValue(authoritative);

    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 2));

    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.metadata).toEqual(
        authoritative,
      ),
    );
    expect(api.getFileMetadata).toHaveBeenCalledTimes(1);
    expect(api.resolveFileMetadata).toHaveBeenCalledTimes(1);
  });

  it("keeps preview usable when metadata loading fails", async () => {
    vi.mocked(api.getFileMetadata).mockRejectedValue(new Error("metadata failed"));

    useViewerStore.getState().openFile("/docs/one.txt");

    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.metadataStatus).toBe("failed"),
    );
    expect(activeViewerTab(useViewerStore.getState())?.previewData).not.toBeNull();
  });
});
