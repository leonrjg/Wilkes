import { fireEvent, render, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import BookmarksPane from "./BookmarksPane";
import { ToastProvider } from "./Toast";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";

const renderPane = (ui: ReactElement = <BookmarksPane />) =>
  render(<ToastProvider>{ui}</ToastProvider>);

vi.mock("../services", () => ({
  api: {
    zoteroGenerateCitation: vi.fn(),
    writeClipboard: vi.fn().mockResolvedValue(undefined),
    updateBookmarkNote: vi.fn(),
  },
}));

import { api } from "../services";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 104,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, index) => ({
        index,
        key: index,
        start: index * 104,
      })),
  }),
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
      scopePath: null,
      paneOpen: true,
      remove: vi.fn().mockResolvedValue(undefined),
    });
    useSearchStore.setState({
      selectedMatch: {
        path: "/tmp/current.pdf",
        origin: { PdfPage: { page: 1, bbox: null } },
      },
      selectMatch: vi.fn(),
    });
    useSettingsStore.setState({
      bookmarksDock: "Right",
      setBookmarksDock: vi.fn(),
    });
  });

  it("closes the pane from the header close button and keeps the dock toggle", () => {
    const togglePane = vi.fn();
    const setBookmarksDock = vi.fn();
    useBookmarksStore.setState({ togglePane });
    useSettingsStore.setState({ bookmarksDock: "Right", setBookmarksDock });
    renderPane();

    fireEvent.click(screen.getByRole("button", { name: "Close bookmarks" }));
    expect(togglePane).toHaveBeenCalledTimes(1);

    // Dock toggle still available (moved next to the scope selector).
    fireEvent.click(screen.getByRole("button", { name: "Dock left" }));
    expect(setBookmarksDock).toHaveBeenCalledWith("Left");
  });

  it("scopes to the current file and navigates through selectMatch", () => {
    renderPane();

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("current file quote"));

    expect(useSearchStore.getState().selectMatch).toHaveBeenCalledWith({
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
    useSearchStore.setState({
      selectedMatch: {
        path: "/tmp/current.txt",
        origin: { TextFile: { line: 1, col: 0 } },
      },
      selectMatch: vi.fn(),
    });

    renderPane();
    fireEvent.click(screen.getByText("selected"));

    expect(useSearchStore.getState().selectMatch).toHaveBeenCalledWith({
      path: "/tmp/current.txt",
      origin: { TextFile: { line: 3, col: 2 } },
      text_range: { start: 12, end: 20 },
    });
  });

  it("shows all bookmarks and filters in memory", () => {
    renderPane();

    fireEvent.click(screen.getByText("All"));
    expect(screen.getByText("other file quote")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("Filter bookmarks"), {
      target: { value: "current" },
    });

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();
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
