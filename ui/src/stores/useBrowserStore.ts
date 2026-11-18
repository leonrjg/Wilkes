import { useEffect } from "react";
import { create } from "zustand";
import { api } from "../services";
import type { BrowserPageEvent, BrowserPaneKind } from "../services/api";

/** A download from the pane, which goes into the library root. */
export interface BrowserDownload {
  url: string;
  name: string;
  path: string | null;
  state: "saving" | "saved" | "failed" | "refused";
  reason?: string;
}

/** An address the app asked the pane to show. A new object per request, so
 *  following the same link twice navigates twice. */
export interface BrowserRequest {
  url: string;
}

interface BrowserStore {
  paneOpen: boolean;
  request: BrowserRequest | null;
  /** What the page in the pane reports: its address, title and whether it is
   *  still loading. Kept after the pane closes, so reopening it returns there. */
  url: string | null;
  title: string | null;
  loading: boolean;
  /** A drag in the page is in progress, and the native panes must step aside
   *  for it to keep the pointer. The window's, so held in the user's store and
   *  read from there by every pane. */
  held: boolean;
  /** In dark mode, darken pages that come out light, as the reader darkens its
   *  own. On by default; the pane's own switch, for a page the lightness check
   *  misjudges. */
  darkenLightPages: boolean;
  /** This session's downloads, newest first, until cleared. */
  downloads: BrowserDownload[];
  /** The link the pointer is over in the page, for the pane's status line. */
  hoveredLink: string | null;
  /** The find strip is open. */
  finding: boolean;
  /** How many times Cmd/Ctrl+F has asked for the strip, so asking again while
   *  it is open puts the keyboard back in it. */
  findRequests: number;
  show: (url: string) => void;
  togglePane: () => void;
  closePane: () => void;
  applyPageEvent: (event: BrowserPageEvent) => void;
  setHeld: (held: boolean) => void;
  toggleDarkening: () => void;
  clearDownloads: () => void;
  closeFind: () => void;
}

/** The download for `url` replaced by `next`, or `next` added first. */
function withDownload(downloads: BrowserDownload[], next: BrowserDownload): BrowserDownload[] {
  const rest = downloads.filter((d) => d.url !== next.url);
  return [next, ...rest];
}

const createBrowserStore = () => create<BrowserStore>((set, get) => ({
  paneOpen: false,
  request: null,
  url: null,
  title: null,
  loading: false,
  held: false,
  darkenLightPages: true,
  downloads: [],
  hoveredLink: null,
  finding: false,
  findRequests: 0,
  show: (url) => set({ paneOpen: true, request: { url } }),
  togglePane: () => {
    const { paneOpen, url } = get();
    if (paneOpen) set({ paneOpen: false });
    else set({ paneOpen: true, request: url ? { url } : null });
  },
  closePane: () => set({ paneOpen: false, loading: false, finding: false, hoveredLink: null }),
  applyPageEvent: (event) => {
    switch (event.kind) {
      case "loading":
        set({ url: event.url, loading: true, hoveredLink: null });
        break;
      case "loaded":
        set({ url: event.url, loading: false });
        break;
      case "title":
        set({ title: event.title });
        break;
      case "download_started":
        set((state) => ({
          downloads: withDownload(state.downloads, { url: event.url, name: event.name, path: null, state: "saving" }),
        }));
        break;
      case "download_finished":
        set((state) => ({
          downloads: withDownload(state.downloads, {
            url: event.url,
            name: event.name,
            path: event.path,
            state: event.success ? "saved" : "failed",
          }),
        }));
        break;
      case "download_refused":
        set((state) => ({
          downloads: withDownload(state.downloads, {
            url: event.url,
            name: event.url.split(/[?#]/)[0].split("/").filter(Boolean).pop() ?? event.url,
            path: null,
            state: "refused",
            reason: event.reason,
          }),
        }));
        break;
      case "hover":
        set({ hoveredLink: event.url });
        break;
      case "find_requested":
        set((state) => ({ finding: true, findRequests: state.findRequests + 1 }));
        break;
    }
  },
  setHeld: (held) => set({ held }),
  toggleDarkening: () => set((state) => ({ darkenLightPages: !state.darkenLightPages })),
  clearDownloads: () => set({ downloads: [] }),
  closeFind: () => set({ finding: false }),
}));

/** The user's own pane. */
export const useBrowserStore = createBrowserStore();
/** The pane the agent browses in, which the user opens to watch or take over. */
export const useAgentBrowserStore = createBrowserStore();

export function browserStoreFor(kind: BrowserPaneKind) {
  return kind === "agent" ? useAgentBrowserStore : useBrowserStore;
}

/** Keep `kind`'s store following its pane's page for as long as the window
 *  lives, open or not: the agent browses with nobody watching, and opening its
 *  pane should show where it is. */
export function useBrowserPageEvents(kind: BrowserPaneKind) {
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    api
      .onBrowserPanePage?.(kind, browserStoreFor(kind).getState().applyPageEvent)
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((e) => console.error(`browser pane (${kind}): could not listen to its page`, e));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [kind]);
}
