import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../services";
import type { MatchRef, PreviewData } from "../lib/types";
import { useSettingsStore } from "./useSettingsStore";
import {
  activeViewerTab,
  paneActiveTab,
  paneTabs,
  useViewerStore,
  VIEWER_SESSION_STORAGE_KEY,
} from "./useViewerStore";

vi.mock("../services", () => ({
  api: {
    preview: vi.fn(),
    previewStandalone: vi.fn(),
    getFileMetadata: vi.fn(),
    getStandaloneFileMetadata: vi.fn(),
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
      mode: "workspace",
      tabs: [],
      panes: [], focusedPaneId: null,
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

  it("opens every paginated format at a page, not only PDFs", () => {
    // The regression: this asked whether the path ended in `.pdf`, so a book
    // opened from the file tree was given a text origin. The pane then took
    // its text branches, the page-shaped preview that arrived matched none of
    // them, and the document rendered as nothing at all — blank, for every
    // format except PDF.
    for (const name of [
      "/books/a.epub",
      "/books/b.mobi",
      "/books/c.azw3",
      "/books/d.fb2",
      "/books/e.cbz",
      "/books/f.CBT",
    ]) {
      useViewerStore.getState().openFile(name);
    }

    const origins = useViewerStore.getState().tabs.map((tab) => tab.match.origin);
    expect(origins).toEqual(origins.map(() => ({ PdfPage: { page: 1, bbox: null } })));
    expect(origins).toHaveLength(6);
  });

  it("keeps standalone documents outside workspace preview, metadata, and persistence", async () => {
    vi.mocked(api.previewStandalone!).mockResolvedValue(textPreview("outside"));
    vi.mocked(api.getStandaloneFileMetadata!).mockResolvedValue(metadata);

    useViewerStore.getState().enterStandaloneMode();
    useViewerStore.getState().openFile("/outside/all-roots/notes.txt");

    await vi.waitFor(() =>
      expect(activeViewerTab(useViewerStore.getState())?.previewData).toEqual(
        textPreview("outside"),
      ),
    );
    expect(api.previewStandalone).toHaveBeenCalled();
    expect(api.preview).not.toHaveBeenCalled();
    expect(api.getStandaloneFileMetadata).toHaveBeenCalledWith(
      "/outside/all-roots/notes.txt",
    );
    expect(api.getFileMetadata).not.toHaveBeenCalled();
    expect(localStorage.getItem(VIEWER_SESSION_STORAGE_KEY)).toBeNull();

    await useViewerStore.getState().switchWorkspace("default");
  });

  it("creates one tab per path and activates an existing tab on reopen", () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt"));
    const firstId = (activeViewerTab(useViewerStore.getState())?.id ?? null);
    useViewerStore.getState().openMatch(textMatch("/docs/two.txt"));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt"));

    expect(useViewerStore.getState().tabs).toHaveLength(2);
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe(firstId);
    expect(api.getFileMetadata).toHaveBeenCalledTimes(2);
  });

  it("keeps independent histories per tab", () => {
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 1));
    useViewerStore.getState().openMatch(textMatch("/docs/one.txt", 5));
    const firstId = (activeViewerTab(useViewerStore.getState())?.id ?? null)!;
    useViewerStore.getState().openMatch(textMatch("/docs/two.txt", 3));
    const secondId = (activeViewerTab(useViewerStore.getState())?.id ?? null)!;

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
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe(three.id);

    useViewerStore.getState().closeTab(three.id);
    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe(one.id);

    useViewerStore.getState().closeTab(one.id);
    expect(useViewerStore.getState()).toEqual(
      expect.objectContaining({ tabs: [], panes: [], focusedPaneId: null }),
    );
  });

  it("does not change the active tab when closing an inactive path", () => {
    useViewerStore.getState().openFile("/docs/one.txt");
    const oneId = (activeViewerTab(useViewerStore.getState())?.id ?? null);
    useViewerStore.getState().openFile("/docs/two.txt");
    const twoId = (activeViewerTab(useViewerStore.getState())?.id ?? null);

    useViewerStore.getState().closePath("/docs/one.txt");

    expect((activeViewerTab(useViewerStore.getState())?.id ?? null)).toBe(twoId);
    expect(useViewerStore.getState().tabs.map((tab) => tab.id)).not.toContain(oneId);
  });

  it("keeps each tab's cached preview when switching tabs", async () => {
    vi.mocked(api.preview)
      .mockResolvedValueOnce(textPreview("first"))
      .mockResolvedValueOnce(textPreview("second"));

    useViewerStore.getState().openFile("/docs/one.txt");
    const firstId = (activeViewerTab(useViewerStore.getState())?.id ?? null)!;
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
          pane: 0,
        }],
        activePath: "/docs/one.txt",
        paneActivePaths: ["/docs/one.txt"],
        focusedPane: 0,
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

    useViewerStore.setState({ tabs: [], panes: [], focusedPaneId: null, sessionHydrated: false });
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

    useViewerStore.setState({ tabs: [], panes: [], focusedPaneId: null, sessionHydrated: false });
    localStorage.setItem(
      VIEWER_SESSION_STORAGE_KEY,
      JSON.stringify({ version: 1, state: { tabs: [{ path: 42 }], activePath: 42 } }),
    );

    await useViewerStore.getState().restoreSession();
    expect(useViewerStore.getState().tabs).toEqual([]);
    expect(api.preview).not.toHaveBeenCalled();

    useViewerStore.setState({ tabs: [], panes: [], focusedPaneId: null, sessionHydrated: false });
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

    useViewerStore.getState().retryTab((activeViewerTab(useViewerStore.getState())?.id ?? null)!);

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
    const activeId = (activeViewerTab(useViewerStore.getState())?.id ?? null)!;

    useViewerStore.getState().closeTab(activeId);

    expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/docs/two.txt");
    expect(api.preview).toHaveBeenCalledTimes(2);
    expect(api.preview).toHaveBeenLastCalledWith(textMatch("/docs/two.txt"));
  });

  describe("panes", () => {
    const open = (...paths: string[]) => {
      for (const path of paths) useViewerStore.getState().openMatch(textMatch(path));
    };
    const tabId = (path: string) =>
      useViewerStore.getState().tabs.find((tab) => tab.path === path)!.id;
    /** Each pane's tabs, left to right, with the one it shows starred. */
    const layout = () => {
      const state = useViewerStore.getState();
      return state.panes.map((pane) =>
        paneTabs(state, pane.id).map((tab) => {
          const name = tab.path.replace("/docs/", "").replace(".txt", "");
          return pane.activeTabId === tab.id ? `${name}*` : name;
        }),
      );
    };
    const focused = () => {
      const state = useViewerStore.getState();
      return state.panes.findIndex((pane) => pane.id === state.focusedPaneId);
    };

    it("splits tabs off into as many panes as asked, each to the right of its own", () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt", "/docs/four.txt");

      useViewerStore.getState().splitTab(tabId("/docs/four.txt"));
      useViewerStore.getState().focusPane(useViewerStore.getState().panes[0].id);
      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));

      expect(layout()).toEqual([["one", "three*"], ["two*"], ["four*"]]);
      expect(focused()).toBe(1);
      expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/docs/two.txt");
    });

    it("does not split a pane's only tab away from it", () => {
      open("/docs/one.txt");

      useViewerStore.getState().splitTab(tabId("/docs/one.txt"));

      expect(layout()).toEqual([["one*"]]);
    });

    it("opens new documents in the focused pane", () => {
      open("/docs/one.txt", "/docs/two.txt");
      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));

      open("/docs/three.txt");
      useViewerStore.getState().focusPane(useViewerStore.getState().panes[0].id);
      open("/docs/four.txt");

      expect(layout()).toEqual([["one", "four*"], ["two", "three*"]]);
      expect(focused()).toBe(0);
    });

    it("shows a document already open in another pane there instead of opening it twice", () => {
      open("/docs/one.txt", "/docs/two.txt");
      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));
      useViewerStore.getState().focusPane(useViewerStore.getState().panes[0].id);

      useViewerStore.getState().openMatch(textMatch("/docs/two.txt", 9));

      const state = useViewerStore.getState();
      expect(state.tabs.filter((tab) => tab.path === "/docs/two.txt")).toHaveLength(1);
      expect(focused()).toBe(1);
      expect(activeViewerTab(state)?.match).toEqual(textMatch("/docs/two.txt", 9));
    });

    it("moves a tab between panes, closing a pane it leaves empty", () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt");
      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));
      useViewerStore.getState().splitTab(tabId("/docs/three.txt"));
      // Each split opens directly right of the pane the tab came from.
      expect(layout()).toEqual([["one*"], ["three*"], ["two*"]]);

      const [first] = useViewerStore.getState().panes;
      useViewerStore.getState().moveTabToPane(tabId("/docs/two.txt"), first.id);

      expect(layout()).toEqual([["one", "two*"], ["three*"]]);
      expect(focused()).toBe(0);
    });

    it("loads the tab every pane shows", () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt");
      useViewerStore.getState().splitTab(tabId("/docs/three.txt"));
      useViewerStore.setState((state) => ({
        tabs: state.tabs.map((tab) => ({ ...tab, previewData: null, previewLoading: false })),
      }));
      vi.mocked(api.preview).mockClear();

      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));

      expect(vi.mocked(api.preview).mock.calls.map(([match]) => match.path).sort())
        .toEqual(["/docs/one.txt", "/docs/three.txt", "/docs/two.txt"]);
    });

    it("hands the focus to the pane that took a closed pane's place, else the one left of it", () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt");
      useViewerStore.getState().splitTab(tabId("/docs/two.txt"));
      useViewerStore.getState().splitTab(tabId("/docs/three.txt"));
      useViewerStore.getState().focusPane(useViewerStore.getState().panes[1].id);

      useViewerStore.getState().closeTab(tabId("/docs/two.txt"));
      expect(layout()).toEqual([["one*"], ["three*"]]);
      expect(focused()).toBe(1);

      useViewerStore.getState().closeTab(tabId("/docs/three.txt"));
      expect(layout()).toEqual([["one*"]]);
      expect(focused()).toBe(0);
    });

    it("closes one pane's tabs, or joins every pane back into the first", () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt", "/docs/four.txt");
      useViewerStore.getState().splitTab(tabId("/docs/three.txt"));
      useViewerStore.getState().splitTab(tabId("/docs/four.txt"));

      expect(layout()).toEqual([["one", "two*"], ["four*"], ["three*"]]);

      useViewerStore.getState().closePane(useViewerStore.getState().panes[1].id);
      expect(layout()).toEqual([["one", "two*"], ["three*"]]);

      // The tab being worked with stays the one shown.
      useViewerStore.getState().joinPanes();
      expect(layout()).toEqual([["one", "two", "three*"]]);
      expect(focused()).toBe(0);
    });

    it("restores every pane, the tab each shows and which has the focus", async () => {
      open("/docs/one.txt", "/docs/two.txt", "/docs/three.txt", "/docs/four.txt");
      useViewerStore.getState().splitTab(tabId("/docs/three.txt"));
      useViewerStore.getState().splitTab(tabId("/docs/four.txt"));
      useViewerStore.getState().activateTab(tabId("/docs/one.txt"));
      useViewerStore.getState().focusPane(useViewerStore.getState().panes[1].id);
      const persisted = localStorage.getItem(VIEWER_SESSION_STORAGE_KEY)!;

      useViewerStore.setState({ tabs: [], panes: [], focusedPaneId: null, sessionHydrated: false });
      localStorage.setItem(VIEWER_SESSION_STORAGE_KEY, persisted);
      await useViewerStore.getState().restoreSession();

      expect(layout()).toEqual([["one*", "two"], ["four*"], ["three*"]]);
      expect(focused()).toBe(1);
      expect(activeViewerTab(useViewerStore.getState())?.path).toBe("/docs/four.txt");
      expect(paneActiveTab(useViewerStore.getState(), useViewerStore.getState().panes[2].id)?.path)
        .toBe("/docs/three.txt");
    });

    it("restores a session saved before panes into one", async () => {
      localStorage.setItem(VIEWER_SESSION_STORAGE_KEY, JSON.stringify({
        state: {
          tabs: [
            { path: "/docs/one.txt", history: [textMatch("/docs/one.txt")], historyIndex: 0 },
            { path: "/docs/two.txt", history: [textMatch("/docs/two.txt")], historyIndex: 0 },
          ],
          activePath: "/docs/two.txt",
        },
        version: 1,
      }));

      await useViewerStore.getState().restoreSession();

      expect(layout()).toEqual([["one", "two*"]]);
      expect(focused()).toBe(0);
    });
  });
});
