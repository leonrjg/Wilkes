import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import type { MatchRef, PreviewData } from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";
import {
  activeViewerTab,
  useViewerStore,
  VIEWER_SESSION_STORAGE_KEY,
} from "./useViewerStore";

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
    useViewerStore.setState({
      tabs: [],
      activeTabId: null,
      sessionHydrated: false,
    });
    localStorage.clear();
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

  it("persists only durable tab state", () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 3));

    const stored = JSON.parse(localStorage.getItem(VIEWER_SESSION_STORAGE_KEY)!);

    expect(stored).toEqual({
      state: {
        tabs: [{
          path: "/docs/one.txt",
          history: [textMatch("/docs/one.txt", 3)],
          historyIndex: 0,
        }],
        activePath: "/docs/one.txt",
      },
      version: 1,
    });
    expect(stored.state.tabs[0]).not.toHaveProperty("previewData");
    expect(stored.state.tabs[0]).not.toHaveProperty("metadata");
    expect(stored.state.tabs[0]).not.toHaveProperty("requestId");
    expect(stored.state.tabs[0]).not.toHaveProperty("pdfLoadAttempt");
  });

  it("restores tab order, history, and the active tab while loading inactive tabs lazily", async () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 4));
    useViewerStore.getState().openMatch(textMatch("/docs/two.txt", 2));
    const firstId = useViewerStore.getState().tabs[0].id;
    useViewerStore.getState().activateTab(firstId);
    const persisted = localStorage.getItem(VIEWER_SESSION_STORAGE_KEY)!;

    useViewerStore.setState({ tabs: [], activeTabId: null, sessionHydrated: false });
    localStorage.setItem(VIEWER_SESSION_STORAGE_KEY, persisted);
    vi.clearAllMocks();
    vi.mocked(api.preview).mockImplementation(async (match) => textPreview(match.path));
    vi.mocked(api.getFileMetadata).mockResolvedValue(metadata);

    await useViewerStore.getState().restoreSession();

    const restored = useViewerStore.getState();
    expect(restored.tabs.map((tab) => tab.path)).toEqual([
      "/docs/one.txt",
      "/docs/two.txt",
    ]);
    expect(activeViewerTab(restored)?.path).toBe("/docs/one.txt");
    expect(restored.tabs[0].history).toEqual([
      textMatch("/docs/one.txt", 1),
      textMatch("/docs/one.txt", 4),
    ]);
    expect(restored.tabs[0].historyIndex).toBe(1);
    expect(api.preview).toHaveBeenCalledTimes(1);
    expect(api.preview).toHaveBeenCalledWith(textMatch("/docs/one.txt", 4));

    useViewerStore.getState().activateTab(restored.tabs[1].id);

    expect(api.preview).toHaveBeenCalledTimes(2);
    expect(api.preview).toHaveBeenLastCalledWith(textMatch("/docs/two.txt", 2));
  });

  it("keeps persisted viewer sessions isolated by workspace", async () => {
    const storedState = (path: string) => JSON.stringify({
      version: 1,
      state: {
        tabs: [{ path, history: [textMatch(path, 1)], historyIndex: 0 }],
        activePath: path,
      },
    });
    localStorage.setItem(`${VIEWER_SESSION_STORAGE_KEY}.workspace-a`, storedState("/a.txt"));
    localStorage.setItem(`${VIEWER_SESSION_STORAGE_KEY}.workspace-b`, storedState("/b.txt"));

    await useViewerStore.getState().switchWorkspace("workspace-a");
    expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/a.txt");

    await useViewerStore.getState().switchWorkspace("workspace-b");
    expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/b.txt");

    await useViewerStore.getState().switchWorkspace("workspace-a");
    expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/a.txt");
    await useViewerStore.getState().switchWorkspace("default");
  });

  it("ignores unsupported and malformed persisted sessions", async () => {
    localStorage.setItem(
      VIEWER_SESSION_STORAGE_KEY,
      JSON.stringify({
        version: 0,
        state: {
          tabs: [{ path: "/docs/old.txt", history: [textMatch("/docs/old.txt")], historyIndex: 0 }],
          activePath: "/docs/old.txt",
        },
      }),
    );

    await useViewerStore.getState().restoreSession();
    expect(useViewerStore.getState().tabs).toEqual([]);

    useViewerStore.setState({ tabs: [], activeTabId: null, sessionHydrated: false });
    localStorage.setItem(
      VIEWER_SESSION_STORAGE_KEY,
      JSON.stringify({ version: 1, state: { tabs: [{ path: 42 }], activePath: 42 } }),
    );

    await useViewerStore.getState().restoreSession();
    expect(useViewerStore.getState().tabs).toEqual([]);
    expect(api.preview).not.toHaveBeenCalled();

    useViewerStore.setState({ tabs: [], activeTabId: null, sessionHydrated: false });
    localStorage.setItem(VIEWER_SESSION_STORAGE_KEY, "{not valid JSON");

    await expect(useViewerStore.getState().restoreSession()).resolves.toBeUndefined();
    expect(useViewerStore.getState().tabs).toEqual([]);
  });

  it("exposes preview failures for retry", async () => {
    vi.mocked(api.preview)
      .mockRejectedValueOnce(new Error("file no longer exists"))
      .mockResolvedValueOnce(textPreview("restored"));

    useViewerStore.getState().openFile("/docs/missing.txt");
    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.previewError).toBe(
        "file no longer exists",
      ),
    );

    useViewerStore.getState().retryTab(useViewerStore.getState().activeTabId!);

    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.previewData).toEqual(
        textPreview("restored"),
      ),
    );
    expect(activeViewerTab(useViewerStore.getState())?.previewError).toBeNull();
  });

  it("remaps open tab paths and history after a directory rename", () => {
    useViewerStore.getState().openMatch(textMatch("/library/old/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/library/old/one.txt", 5));
    useViewerStore.getState().openFile("/library/other.txt");
    useViewerStore.getState().remapPathPrefix("/library/old", "/library/new");

    const [renamed, unchanged] = useViewerStore.getState().tabs;
    expect(renamed.path).toBe("/library/new/one.txt");
    expect(renamed.history.map((match) => match.path)).toEqual([
      "/library/new/one.txt",
      "/library/new/one.txt",
    ]);
    expect(renamed.match.path).toBe("/library/new/one.txt");
    expect(unchanged.path).toBe("/library/other.txt");

    const stored = JSON.parse(localStorage.getItem(VIEWER_SESSION_STORAGE_KEY)!);
    expect(stored.state.tabs.map((tab: { path: string }) => tab.path)).toEqual([
      "/library/new/one.txt",
      "/library/other.txt",
    ]);
  });

  it("loads the restored neighbor selected by closing the active tab", async () => {
    const persisted = {
      version: 1,
      state: {
        tabs: [
          { path: "/docs/one.txt", history: [textMatch("/docs/one.txt")], historyIndex: 0 },
          { path: "/docs/two.txt", history: [textMatch("/docs/two.txt")], historyIndex: 0 },
        ],
        activePath: "/docs/one.txt",
      },
    };
    localStorage.setItem(VIEWER_SESSION_STORAGE_KEY, JSON.stringify(persisted));

    await useViewerStore.getState().restoreSession();
    expect(api.preview).toHaveBeenCalledTimes(1);
    const activeId = useViewerStore.getState().activeTabId!;

    useViewerStore.getState().closeTab(activeId);

    expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/docs/two.txt");
    expect(api.preview).toHaveBeenCalledTimes(2);
    expect(api.preview).toHaveBeenLastCalledWith(textMatch("/docs/two.txt"));
  });
});
