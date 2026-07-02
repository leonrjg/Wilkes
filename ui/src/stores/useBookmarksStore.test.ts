import { beforeEach, describe, expect, it, vi } from "vitest";
import { useBookmarksStore } from "./useBookmarksStore";
import { api } from "../services";
import type { Bookmark, NewBookmark } from "../lib/types";

vi.mock("../services", () => ({
  api: {
    listBookmarks: vi.fn(),
    addBookmark: vi.fn(),
    removeBookmark: vi.fn(),
    updateBookmarkNote: vi.fn(),
  },
}));

const newBookmark: NewBookmark = {
  path: "/tmp/example.pdf",
  origin: { PdfPage: { page: 2, bbox: { x: 1, y: 2, width: 3, height: 4 } } },
  quote: "selected text",
  rects: [{ x: 1, y: 2, width: 3, height: 4 }],
};

const bookmark: Bookmark = {
  id: "bookmark-1",
  ...newBookmark,
  created_at: "2026-01-01T00:00:00Z",
  note: null,
};

describe("useBookmarksStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useBookmarksStore.setState({
      bookmarks: [],
      filterText: "",
      scopePath: null,
      paneOpen: false,
    });
  });

  it("loads bookmarks", async () => {
    vi.mocked(api.listBookmarks).mockResolvedValue([bookmark]);

    await useBookmarksStore.getState().load();

    expect(useBookmarksStore.getState().bookmarks).toEqual([bookmark]);
  });

  it("adds a bookmark through the API", async () => {
    vi.mocked(api.addBookmark).mockResolvedValue(bookmark);

    await useBookmarksStore.getState().add(newBookmark);

    expect(api.addBookmark).toHaveBeenCalledWith(newBookmark);
    expect(useBookmarksStore.getState().bookmarks).toEqual([bookmark]);
  });

  it("removes a bookmark through the API", async () => {
    useBookmarksStore.setState({ bookmarks: [bookmark] });
    vi.mocked(api.removeBookmark).mockResolvedValue(undefined);

    await useBookmarksStore.getState().remove("bookmark-1");

    expect(api.removeBookmark).toHaveBeenCalledWith("bookmark-1");
    expect(useBookmarksStore.getState().bookmarks).toEqual([]);
  });

  it("updates a bookmark note through the API", async () => {
    useBookmarksStore.setState({ bookmarks: [bookmark] });
    const noted: Bookmark = { ...bookmark, note: "my note" };
    vi.mocked(api.updateBookmarkNote).mockResolvedValue(noted);

    await useBookmarksStore.getState().updateNote("bookmark-1", "my note");

    expect(api.updateBookmarkNote).toHaveBeenCalledWith("bookmark-1", "my note");
    expect(useBookmarksStore.getState().bookmarks).toEqual([noted]);
  });

  it("toggles pane state", () => {
    useBookmarksStore.getState().togglePane();
    expect(useBookmarksStore.getState().paneOpen).toBe(true);
  });
});
