import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Feed, FeedItem } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    feedsList: vi.fn(),
    feedItems: vi.fn(),
    feedAdd: vi.fn(),
    feedUpdate: vi.fn(),
    feedRemove: vi.fn(() => Promise.resolve()),
    feedsRefresh: vi.fn(() => Promise.resolve()),
    feedsRefreshCancel: vi.fn(() => Promise.resolve()),
    feedItemSetStatus: vi.fn(),
    onFeedsChanged: vi.fn(() => Promise.resolve(() => {})),
    listSavedWorks: vi.fn(() => Promise.resolve([])),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: {
    type: "desktop",
    deletionKind: "trash",
    importFiles: vi.fn(() => Promise.resolve([])),
  },
}));

import { api } from "../services";
import FeedsView, { FollowButton } from "./FeedsView";
import { ToastProvider } from "./Toast";
import { useFeedsStore } from "../stores/useFeedsStore";
import { useSettingsStore } from "../stores/useSettingsStore";

const mocked = api as unknown as Record<string, ReturnType<typeof vi.fn>>;

function feed(overrides: Partial<Feed> = {}): Feed {
  return {
    id: "f1",
    name: "Simon Willison",
    source: { kind: "syndication", url: "https://simonwillison.net/atom/everything/" },
    root: "/library",
    folder: "Feeds/Simon Willison",
    auto_import: false,
    created_at_ms: 0,
    last_attempt_at_ms: Date.now() - 2 * 3_600_000,
    last_success_at_ms: Date.now() - 2 * 3_600_000,
    last_error: null,
    retry_after_ms: null,
    may_have_missed: false,
    inbox_count: 2,
    ...overrides,
  };
}

function item(overrides: Partial<FeedItem> = {}): FeedItem {
  return {
    id: "i1",
    feed_id: "f1",
    link_key: "https://example.org/p",
    link: "https://example.org/p",
    doi: null,
    arxiv_id: null,
    published: null,
    first_seen_at_ms: Date.UTC(2026, 10, 14),
    status: "inbox",
    preview_only: false,
    work: {
      id: "https://example.org/p",
      doi: null,
      title: "Locating a beep",
      year: null,
      publication_date: null,
      venue: "Simon Willison",
      citation_count: null,
      is_open_access: false,
      pdf_url: null,
      landing_page_url: "https://example.org/p",
      image_url: null,
      open_access_status: null,
      license: null,
      authors: null,
      publisher: null,
      language: null,
      file_format: "html",
      file_size: null,
      summary: null,
      subject: null,
      kind: "post",
      pages: null,
      recall_score: null,
      acquisition: "none",
    },
    imported_path: null,
    last_error: null,
    held: false,
    ...overrides,
  } as FeedItem;
}

function show(items: FeedItem[], feeds: Feed[] = [feed()]) {
  mocked.feedsList.mockResolvedValue(feeds);
  mocked.feedItems.mockResolvedValue({ items, inbox_total: items.length });
  return render(
    <ToastProvider>
      <FeedsView readOnly={false} />
    </ToastProvider>,
  );
}

describe("FeedsView", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocked.onFeedsChanged.mockResolvedValue(() => {});
    useFeedsStore.setState({ loaded: false, feeds: [], items: [], inboxTotal: 0, filter: null, importing: {} });
    useSettingsStore.setState({ directory: "/library", favorites: [], recentDirs: [] });
  });

  it("shows each feed's state and every item's date, never a guessed one", async () => {
    show([
      item(),
      item({
        id: "i2",
        published: "2026-10-31T15:17:33Z",
        preview_only: true,
        work: { ...item().work, title: "A paid post" },
      }),
    ], [feed({ may_have_missed: true, last_error: "https://example.org/feed: HTTP 503" })]);

    expect(await screen.findByText(/Refreshed 2 h ago · into/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Change where Simon Willison imports" })).toHaveTextContent(
      "Feeds/Simon Willison",
    );
    expect(screen.getByText(/older ones may have been missed/)).toBeInTheDocument();
    expect(screen.getByText("https://example.org/feed: HTTP 503")).toBeInTheDocument();
    // No date from the source: when it was first seen, said as such.
    expect(screen.getByText(/first seen/)).toBeInTheDocument();
    // A preview is marked and offers no import.
    expect(screen.getByText(/Preview only/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Import A paid post" })).toBeNull();
    expect(screen.getByRole("button", { name: "Import Locating a beep" })).toBeInTheDocument();
  });

  it("offers to go on without a picture that could not be fetched", async () => {
    mocked.feedItemSetStatus.mockRejectedValueOnce(
      "the picture https://example.org/a.png could not be fetched: HTTP 404 Not Found",
    );
    mocked.feedItemSetStatus.mockResolvedValueOnce({ ...item(), status: "imported" });
    show([item()]);

    fireEvent.click(await screen.findByRole("button", { name: "Import Locating a beep" }));
    fireEvent.click(await screen.findByRole("button", { name: "Import without it" }));
    await waitFor(() =>
      expect(mocked.feedItemSetStatus).toHaveBeenLastCalledWith("i1", {
        action: "import",
        allow_missing_images: true,
      }),
    );
  });

  it("dismisses an item", async () => {
    mocked.feedItemSetStatus.mockResolvedValue({ ...item(), status: "dismissed" });
    show([item()]);
    fireEvent.click(await screen.findByRole("button", { name: "Dismiss Locating a beep" }));
    await waitFor(() =>
      expect(mocked.feedItemSetStatus).toHaveBeenCalledWith("i1", { action: "dismiss" }),
    );
  });

  it("adds a blog by its address into the open library", async () => {
    mocked.feedAdd.mockResolvedValue(feed());
    show([], []);
    fireEvent.click(await screen.findByRole("button", { name: "Add a feed" }));
    fireEvent.change(screen.getByLabelText("Blog or feed address"), {
      target: { value: " https://simonwillison.net/ " },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Add feed" }));
    });
    expect(mocked.feedAdd).toHaveBeenCalledWith({
      source: { kind: "syndication", url: "https://simonwillison.net/" },
      name: null,
      root: "/library",
      auto_import: false,
    });
  });

  it("moves where a feed's new posts go, to another library and folder", async () => {
    useSettingsStore.setState({ directory: "/library", favorites: ["/other"], recentDirs: [] });
    mocked.feedUpdate.mockResolvedValue(feed({ root: "/other", folder: "Posts" }));
    show([], [feed()]);

    fireEvent.click(await screen.findByRole("button", { name: "Change where Simon Willison imports" }));
    expect(screen.getByText(/those already imported stay where they are/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Library Simon Willison writes into"), {
      target: { value: "/other" },
    });
    fireEvent.change(screen.getByLabelText("Folder Simon Willison writes into"), {
      target: { value: " Posts " },
    });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
    });
    expect(mocked.feedUpdate).toHaveBeenCalledWith("f1", { root: "/other", folder: "Posts" });
    expect(await screen.findByRole("button", { name: "Change where Simon Willison imports" })).toHaveTextContent(
      "Posts",
    );
  });

  it("follows a listing as it is filtered, without its page", async () => {
    mocked.feedsList.mockResolvedValue([]);
    mocked.feedItems.mockResolvedValue({ items: [], inbox_total: 0 });
    mocked.feedAdd.mockResolvedValue(feed({ name: "OpenAlex: graph theory" }));
    render(
      <ToastProvider>
        <FollowButton
          provider="openalex"
          name="OpenAlex"
          request={{ query: "graph theory", filters: { open: ["true"] }, page: 3, limit: 10 }}
        />
      </ToastProvider>,
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Follow OpenAlex" }));
    });
    expect(mocked.feedAdd).toHaveBeenCalledWith({
      source: {
        kind: "listing",
        provider: "openalex",
        request: { query: "graph theory", filters: { open: ["true"] }, page: null, limit: null },
      },
      root: "/library",
    });
    expect(await screen.findByText(/Following OpenAlex: graph theory/)).toBeInTheDocument();
  });
});
