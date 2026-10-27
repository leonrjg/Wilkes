import { create } from "zustand";
import type { BrowserPageEvent } from "../services/api";

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
  /** A drag in the page is in progress, and the native pane must step aside
   *  for it to keep the pointer. */
  held: boolean;
  /** In dark mode, invert pages that come out light. On by default; the pane's
   *  own switch, for a page the lightness check misjudges. */
  invertLightPages: boolean;
  /** This session's downloads, newest first, until cleared. */
  downloads: BrowserDownload[];
  show: (url: string) => void;
  togglePane: () => void;
  closePane: () => void;
  applyPageEvent: (event: BrowserPageEvent) => void;
  setHeld: (held: boolean) => void;
  toggleInversion: () => void;
  clearDownloads: () => void;
}

/** The download for `url` replaced by `next`, or `next` added first. */
function withDownload(downloads: BrowserDownload[], next: BrowserDownload): BrowserDownload[] {
  const rest = downloads.filter((d) => d.url !== next.url);
  return [next, ...rest];
}

export const useBrowserStore = create<BrowserStore>((set, get) => ({
  paneOpen: false,
  request: null,
  url: null,
  title: null,
  loading: false,
  held: false,
  invertLightPages: true,
  downloads: [],
  show: (url) => set({ paneOpen: true, request: { url } }),
  togglePane: () => {
    const { paneOpen, url } = get();
    if (paneOpen) set({ paneOpen: false });
    else set({ paneOpen: true, request: url ? { url } : null });
  },
  closePane: () => set({ paneOpen: false, loading: false }),
  applyPageEvent: (event) => {
    switch (event.kind) {
      case "loading":
        set({ url: event.url, loading: true });
        break;
      case "loaded":
        set({ url: event.url, loading: false });
        break;
      case "title":
        set({ title: event.title });
        break;
      case "download_started":
        set((state) => ({
          downloads: withDownload(state.downloads, { url: event.url, name: event.name, path: event.path, state: "saving" }),
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
    }
  },
  setHeld: (held) => set({ held }),
  toggleInversion: () => set((state) => ({ invertLightPages: !state.invertLightPages })),
  clearDownloads: () => set({ downloads: [] }),
}));
