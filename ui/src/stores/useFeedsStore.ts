import { create } from "zustand";
import { api } from "../services";
import type { Feed, FeedItem, FeedSource, FeedUpdate, NewFeed } from "../lib/types";

/** How many inbox items are read at a time. */
const PAGE = 50;

interface FeedsStore {
  /** Whether the feeds have been read in this workspace. Until they are, a
   *  workspace switch or an event has nothing to bring up to date. */
  loaded: boolean;
  feeds: Feed[];
  /** The inbox, newest first, for `filter`. */
  items: FeedItem[];
  inboxTotal: number;
  /** One feed's items, or every feed's when null. */
  filter: string | null;
  /** Whether more inbox items than are held exist for `filter`. */
  more: boolean;
  /** A refresh was asked for and has not reported back. */
  refreshing: boolean;
  error: string | null;
  /** Items being imported, by id. */
  importing: Record<string, boolean>;

  load: () => Promise<void>;
  /** Re-read whatever was read, after a change or a workspace switch. */
  reloadIfLoaded: () => Promise<void>;
  loadMore: () => Promise<void>;
  setFilter: (feedId: string | null) => Promise<void>;
  add: (feed: NewFeed) => Promise<Feed>;
  update: (id: string, update: FeedUpdate) => Promise<void>;
  remove: (id: string, deleteFiles: boolean) => Promise<void>;
  refresh: () => Promise<void>;
  stop: () => Promise<void>;
  /** Import one item. Rejects with why it could not be imported; the item
   *  stays in the inbox carrying the same reason. */
  importItem: (id: string, allowMissingImages?: boolean) => Promise<void>;
  dismiss: (id: string) => Promise<void>;
}

let unlisten: (() => void) | null = null;
// A change event arrives once per feed while a refresh runs; reading the
// lists once per burst is enough to show it.
let pendingReload: ReturnType<typeof setTimeout> | null = null;

export const useFeedsStore = create<FeedsStore>((set, get) => {
  const readItems = async (offset: number): Promise<{ items: FeedItem[]; total: number }> => {
    const page = await api.feedItems({
      status: "inbox",
      feed_id: get().filter,
      offset,
      limit: PAGE,
    });
    return { items: page.items, total: page.inbox_total };
  };

  const reload = async () => {
    const held = Math.max(get().items.length, PAGE);
    const [feeds, page] = await Promise.all([
      api.feedsList(),
      api.feedItems({ status: "inbox", feed_id: get().filter, offset: 0, limit: held }),
    ]);
    set({
      feeds,
      items: page.items,
      inboxTotal: page.inbox_total,
      more: page.items.length === held,
      error: null,
    });
  };

  const subscribe = async () => {
    if (unlisten !== null) return;
    unlisten = await api.onFeedsChanged(() => {
      // A refresh that changed something has reported back.
      set({ refreshing: false });
      if (pendingReload !== null) return;
      pendingReload = setTimeout(() => {
        pendingReload = null;
        void reload().catch((error) => {
          console.error("feeds: re-reading after a change failed:", error);
          set({ error: `${error}` });
        });
      }, 250);
    });
  };

  return {
    loaded: false,
    feeds: [],
    items: [],
    inboxTotal: 0,
    filter: null,
    more: false,
    refreshing: false,
    error: null,
    importing: {},

    load: async () => {
      await subscribe();
      try {
        await reload();
        set({ loaded: true });
      } catch (error) {
        console.error("feeds: loading failed:", error);
        set({ error: `${error}` });
      }
    },

    reloadIfLoaded: async () => {
      if (!get().loaded) return;
      set({ filter: null, items: [], importing: {}, refreshing: false });
      try {
        await reload();
      } catch (error) {
        console.error("feeds: re-reading failed:", error);
        set({ error: `${error}` });
      }
    },

    loadMore: async () => {
      const { items, total } = await readItems(get().items.length);
      set((state) => ({
        items: [...state.items, ...items.filter((item) => !state.items.some((held) => held.id === item.id))],
        inboxTotal: total,
        more: items.length === PAGE,
      }));
    },

    setFilter: async (feedId) => {
      set({ filter: feedId, items: [] });
      const { items, total } = await readItems(0);
      if (get().filter !== feedId) return;
      set({ items, inboxTotal: total, more: items.length === PAGE });
    },

    add: async (feed) => {
      const added = await api.feedAdd(feed);
      set({ refreshing: true });
      await reload();
      return added;
    },

    update: async (id, update) => {
      const updated = await api.feedUpdate(id, update);
      set((state) => ({ feeds: state.feeds.map((feed) => (feed.id === id ? updated : feed)) }));
    },

    remove: async (id, deleteFiles) => {
      await api.feedRemove(id, deleteFiles);
      if (get().filter === id) set({ filter: null });
      await reload();
    },

    refresh: async () => {
      set({ refreshing: true });
      try {
        await api.feedsRefresh();
      } catch (error) {
        set({ refreshing: false });
        throw error;
      }
    },

    stop: async () => {
      await api.feedsRefreshCancel();
      set({ refreshing: false });
    },

    importItem: async (id, allowMissingImages = false) => {
      set((state) => ({ importing: { ...state.importing, [id]: true } }));
      try {
        await api.feedItemSetStatus(id, {
          action: "import",
          allow_missing_images: allowMissingImages,
        });
      } finally {
        set((state) => {
          const { [id]: _done, ...rest } = state.importing;
          return { importing: rest };
        });
        await reload();
      }
    },

    dismiss: async (id) => {
      await api.feedItemSetStatus(id, { action: "dismiss" });
      set((state) => ({
        items: state.items.filter((item) => item.id !== id),
        inboxTotal: Math.max(state.inboxTotal - 1, 0),
      }));
      await reload();
    },
  };
});

/** The source a listing on screen is followed as. */
export function listingSource(
  provider: string,
  request: import("../lib/types").BrowseRequest,
): FeedSource {
  return { kind: "listing", provider, request: { ...request, page: null, limit: null } };
}
