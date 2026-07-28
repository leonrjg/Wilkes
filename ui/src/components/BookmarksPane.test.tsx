import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import BookmarksPane from "./BookmarksPane";
import { ToastProvider } from "./Toast";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useViewerStore } from "../stores/useViewerStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";

const ensureCurrentRootIndexed = useSemanticStore.getState().ensureCurrentRootIndexed;
const virtualizerOptionsSpy = vi.hoisted(() => vi.fn());

const renderPane = (ui: ReactElement = <BookmarksPane />) =>
  render(<ToastProvider>{ui}</ToastProvider>);

vi.mock("../services", () => ({
  api: {
    zoteroGenerateCitation: vi.fn(),
    writeClipboard: vi.fn().mockResolvedValue(undefined),
    updateBookmarkNote: vi.fn(),
    clusterBookmarks: vi.fn(),
  },
}));

import { api } from "../services";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: (options: {
    count: number;
    getItemKey?: (index: number) => string | number;
  }) => {
    virtualizerOptionsSpy(options);
    return {
      getTotalSize: () => options.count * 104,
      getVirtualItems: () =>
        Array.from({ length: options.count }, (_, index) => ({
          index,
          key: options.getItemKey?.(index) ?? index,
          start: index * 104,
        })),
    };
  },
}));

describe("BookmarksPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useBookmarksStore.setState({
      bookmarks: [
        {
          id: "one",
          path: "/tmp/current.pdf",
          origin: { PdfPage: { page: 2, bbox: null } },
          quote: "current file quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
        },
        {
          id: "two",
          path: "/tmp/other.pdf",
          origin: { PdfPage: { page: 9, bbox: null } },
          quote: "other file quote",
          created_at: "2026-01-01T00:00:00Z",
          note: null,
        },
      ],
      filterText: "",
      scope: "current",
      paneOpen: true,
      remove: vi.fn().mockResolvedValue(undefined),
    });
    const match = {
        path: "/tmp/current.pdf",
        origin: { PdfPage: { page: 1, bbox: null } },
      } as const;
    useViewerStore.setState({
      activeTabId: "current-tab",
      tabs: [{
        id: "current-tab",
        path: match.path,
        match,
        history: [match],
        historyIndex: 0,
        previewData: null,
        previewLoading: false,
        metadata: null,
        metadataStatus: "idle",
        requestId: 1,
      }],
      openMatch: vi.fn(),
    });
    useSettingsStore.setState({
      bookmarksDock: "Right",
      setBookmarksDock: vi.fn(),
      preferSemantic: false,
    });
    useSemanticStore.setState({ readyForCurrentRoot: false, ensureCurrentRootIndexed });
  });

  it("closes the pane from the header close button and keeps the dock toggle", () => {
    const closePane = vi.fn();
    const setBookmarksDock = vi.fn();
    useBookmarksStore.setState({ closePane });
    useSettingsStore.setState({ bookmarksDock: "Right", setBookmarksDock });
    renderPane();

    fireEvent.click(screen.getByRole("button", { name: "Close bookmarks" }));
    expect(closePane).toHaveBeenCalledTimes(1);

    // Dock toggle still available (moved next to the scope selector).
    fireEvent.click(screen.getByRole("button", { name: "Dock left" }));
    expect(setBookmarksDock).toHaveBeenCalledWith("Left");
  });

  it("scopes to the current file and navigates through the viewer", () => {
    renderPane();

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("current file quote"));

    expect(useViewerStore.getState().openMatch).toHaveBeenCalledWith({
      path: "/tmp/current.pdf",
      origin: { PdfPage: { page: 2, bbox: null } },
    });
  });

  it("preserves a text bookmark range when navigating", () => {
    useBookmarksStore.setState({
      bookmarks: [
        {
          id: "text",
          path: "/tmp/current.txt",
          origin: { TextFile: { line: 3, col: 2 } },
          text_range: { start: 12, end: 20 },
          quote: "selected",
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
      ],
    });
    const match = {
        path: "/tmp/current.txt",
        origin: { TextFile: { line: 1, col: 0 } },
      } as const;
    useViewerStore.setState({
      activeTabId: "text-tab",
      tabs: [{
        id: "text-tab",
        path: match.path,
        match,
        history: [match],
        historyIndex: 0,
        previewData: null,
        previewLoading: false,
        metadata: null,
        metadataStatus: "idle",
        requestId: 1,
      }],
      openMatch: vi.fn(),
    });

    renderPane();
    fireEvent.click(screen.getByText("selected"));

    expect(useViewerStore.getState().openMatch).toHaveBeenCalledWith({
      path: "/tmp/current.txt",
      origin: { TextFile: { line: 3, col: 2 } },
      text_range: { start: 12, end: 20 },
    });
  });

  it("shows all bookmarks and filters in memory", () => {
    renderPane();

    expect(screen.getByRole("heading", { name: "Bookmarks" })).toBeInTheDocument();
    expect(screen.getByLabelText("1 bookmark")).toHaveTextContent("1");

    fireEvent.click(screen.getByText("All"));
    expect(screen.getByText("other file quote")).toBeInTheDocument();
    expect(screen.getByLabelText("2 bookmarks")).toHaveTextContent("2");

    fireEvent.change(screen.getByPlaceholderText("Filter bookmarks"), {
      target: { value: "current" },
    });

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();
    expect(screen.getByLabelText("1 bookmark")).toHaveTextContent("1");
  });

  it("preserves the All scope when bookmark navigation changes the active file", () => {
    renderPane();
    fireEvent.click(screen.getByText("All"));

    fireEvent.click(screen.getByText("other file quote"));
    expect(useViewerStore.getState().openMatch).toHaveBeenCalledWith({
      path: "/tmp/other.pdf",
      origin: { PdfPage: { page: 9, bbox: null } },
      text_range: undefined,
    });

    const otherMatch = {
      path: "/tmp/other.pdf",
      origin: { PdfPage: { page: 9, bbox: null } },
    } as const;
    act(() => {
      useViewerStore.setState((state) => ({
        tabs: state.tabs.map((tab) =>
          tab.id === state.activeTabId
            ? {
                ...tab,
                path: otherMatch.path,
                match: otherMatch,
                history: [otherMatch],
                historyIndex: 0,
              }
            : tab,
        ),
      }));
    });

    expect(useBookmarksStore.getState().scope).toBe("all");
    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.getByText("other file quote")).toBeInTheDocument();
    expect(screen.getByLabelText("2 bookmarks")).toHaveTextContent("2");
  });

  it("groups scoped bookmarks by semantic theme and filters within stable groups", async () => {
    const representativeQuote =
      "alpha cats form social bonds through repeated grooming and shared resting places";
    useBookmarksStore.setState({
      scope: "all",
      bookmarks: [
        {
          id: "cat-1",
          path: "/tmp/cats.pdf",
          origin: { PdfPage: { page: 1, bbox: null } },
          quote: representativeQuote,
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
        {
          id: "cat-2",
          path: "/tmp/cats.pdf",
          origin: { PdfPage: { page: 2, bbox: null } },
          quote: "feline behavior",
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
        {
          id: "physics-1",
          path: "/tmp/physics.pdf",
          origin: { PdfPage: { page: 1, bbox: null } },
          quote: "quantum fields",
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
        {
          id: "physics-2",
          path: "/tmp/physics.pdf",
          origin: { PdfPage: { page: 2, bbox: null } },
          quote: "particle interactions",
          created_at: "2026-01-01T00:00:00Z",
          rects: [],
        },
      ],
    });
    useSemanticStore.setState({
      readyForCurrentRoot: true,
      ensureCurrentRootIndexed: vi.fn().mockResolvedValue(true),
    });
    useSettingsStore.setState({ preferSemantic: true });
    vi.mocked(api.clusterBookmarks).mockResolvedValue({
      clusters: [
        {
          bookmark_ids: ["cat-1", "cat-2"],
          representative_bookmark_id: "cat-1",
          cohesion: 0.9,
        },
        {
          bookmark_ids: ["physics-1", "physics-2"],
          representative_bookmark_id: "physics-1",
          cohesion: 0.88,
        },
      ],
      unclustered_bookmark_ids: [],
    });

    renderPane();
    fireEvent.click(screen.getByRole("button", { name: "Group bookmarks by theme" }));

    const representativeHeading = await screen.findByText(`Around “${representativeQuote}”`);
    expect(representativeHeading).toHaveClass("whitespace-pre-wrap");
    expect(representativeHeading).not.toHaveClass("truncate");
    expect(screen.getByText("Around “quantum fields”")).toBeInTheDocument();
    const virtualizerOptions = virtualizerOptionsSpy.mock.calls.at(-1)?.[0] as {
      getItemKey: (index: number) => string | number;
    };
    expect(virtualizerOptions.getItemKey(0)).toBe("theme:cat-1");
    expect(virtualizerOptions.getItemKey(1)).toBe("theme:physics-1");
    expect(api.clusterBookmarks).toHaveBeenCalledWith({
      bookmark_ids: ["cat-1", "cat-2", "physics-1", "physics-2"],
      granularity: "balanced",
    });
    const granularity = screen.getByRole("slider", { name: "Theme granularity" });
    expect(granularity).toHaveValue("2");
    expect(granularity).toHaveAttribute("aria-valuetext", "Balanced");
    expect(screen.getByText("2 themes")).toBeInTheDocument();

    const expandCats = screen.getByRole("button", {
      name: `Expand cluster: Around “${representativeQuote}”`,
    });
    expect(expandCats).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("feline behavior")).not.toBeInTheDocument();
    expect(screen.queryByText("particle interactions")).not.toBeInTheDocument();
    fireEvent.click(expandCats);

    expect(
      screen.getByRole("button", {
        name: `Collapse cluster: Around “${representativeQuote}”`,
      }),
    ).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("feline behavior")).toBeInTheDocument();
    expect(screen.queryByText("particle interactions")).not.toBeInTheDocument();

    fireEvent.change(granularity, { target: { value: "3" } });
    expect(screen.queryByText("feline behavior")).not.toBeInTheDocument();
    expect(granularity).toHaveAttribute("aria-valuetext", "More");
    await waitFor(() => {
      expect(api.clusterBookmarks).toHaveBeenLastCalledWith({
        bookmark_ids: ["cat-1", "cat-2", "physics-1", "physics-2"],
        granularity: "more",
      });
    });

    fireEvent.change(screen.getByPlaceholderText("Filter bookmarks"), {
      target: { value: "alpha" },
    });

    await waitFor(() => {
      expect(screen.getByText(`Around “${representativeQuote}”`)).toBeInTheDocument();
      expect(screen.queryByText("Around “quantum fields”")).not.toBeInTheDocument();
    });
    expect(api.clusterBookmarks).toHaveBeenCalledTimes(2);
  });

  it("edits and saves a note through the store", async () => {
    const updateNote = vi.fn().mockResolvedValue(undefined);
    useBookmarksStore.setState({ updateNote });
    renderPane();

    fireEvent.click(screen.getByRole("button", { name: "Add note" }));
    fireEvent.change(screen.getByPlaceholderText("Add a note…"), {
      target: { value: "  a thought  " },
    });
    fireEvent.click(screen.getByText("Save"));

    await vi.waitFor(() => expect(updateNote).toHaveBeenCalledWith("one", "a thought"));
  });

  it("renders an existing note and offers to edit it", () => {
    useBookmarksStore.setState({
      bookmarks: [
        {
          id: "one",
          path: "/tmp/current.pdf",
          origin: { PdfPage: { page: 2, bbox: null } },
          quote: "current file quote",
          created_at: "2026-01-01T00:00:00Z",
          note: "existing note",
        },
      ],
    });
    renderPane();

    expect(screen.getByText("existing note")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Edit note" })).toBeInTheDocument();
  });

  it("hides the citation action unless the Zotero integration is enabled", () => {
    renderPane();
    expect(screen.queryByRole("button", { name: "Get citation from Zotero" })).not.toBeInTheDocument();
  });

  it("copies the plain-text in-text citation from Zotero when enabled", async () => {
    useSettingsStore.setState({
      settings: { integrations: { zotero: { enabled: true } } },
    } as never);
    vi.mocked(api.zoteroGenerateCitation).mockResolvedValue({
      citation: "<span>(Smith 2020)</span>",
      bibliography: "<div class=\"csl-entry\">Smith, J. (2020). <i>A Title</i>.</div>",
      low_confidence: false,
    });

    renderPane();
    fireEvent.click(screen.getAllByRole("button", { name: "Get citation from Zotero" })[0]);

    await vi.waitFor(() => expect(api.writeClipboard).toHaveBeenCalledTimes(1));
    expect(api.zoteroGenerateCitation).toHaveBeenCalledWith("/tmp/current.pdf");
    expect(api.writeClipboard).toHaveBeenCalledWith('"current file quote" (Smith 2020)');
  });

  it("shows an immediate pending indicator while the citation is fetched", async () => {
    useSettingsStore.setState({
      settings: { integrations: { zotero: { enabled: true } } },
    } as never);
    let resolveLookup!: (value: {
      citation: string | null;
      bibliography: string | null;
      low_confidence: boolean;
    }) => void;
    vi.mocked(api.zoteroGenerateCitation).mockReturnValue(
      new Promise((resolve) => {
        resolveLookup = resolve;
      }),
    );

    renderPane();
    fireEvent.click(screen.getAllByRole("button", { name: "Get citation from Zotero" })[0]);

    // Feedback appears before the network call settles.
    expect(await screen.findByText("Fetching citation…")).toBeInTheDocument();

    resolveLookup({ citation: "<span>(Smith 2020)</span>", bibliography: null, low_confidence: false });

    await vi.waitFor(() => expect(screen.queryByText("Fetching citation…")).not.toBeInTheDocument());
    expect(screen.getByText("Citation copied")).toBeInTheDocument();
  });
});
