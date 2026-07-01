import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import BookmarksPane from "./BookmarksPane";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";

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

  it("scopes to the current file and navigates through selectMatch", () => {
    render(<BookmarksPane />);

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("current file quote"));

    expect(useSearchStore.getState().selectMatch).toHaveBeenCalledWith({
      path: "/tmp/current.pdf",
      origin: { PdfPage: { page: 2, bbox: null } },
    });
  });

  it("shows all bookmarks and filters in memory", () => {
    render(<BookmarksPane />);

    fireEvent.click(screen.getByText("All"));
    expect(screen.getByText("other file quote")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("Filter bookmarks"), {
      target: { value: "current" },
    });

    expect(screen.getByText("current file quote")).toBeInTheDocument();
    expect(screen.queryByText("other file quote")).not.toBeInTheDocument();
  });
});
