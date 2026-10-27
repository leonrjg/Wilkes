import { create } from "zustand";
import type { BrowserPageEvent } from "../services/api";

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
  show: (url: string) => void;
  togglePane: () => void;
  closePane: () => void;
  applyPageEvent: (event: BrowserPageEvent) => void;
  setHeld: (held: boolean) => void;
  toggleInversion: () => void;
}

export const useBrowserStore = create<BrowserStore>((set, get) => ({
  paneOpen: false,
  request: null,
  url: null,
  title: null,
  loading: false,
  held: false,
  invertLightPages: true,
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
    }
  },
  setHeld: (held) => set({ held }),
  toggleInversion: () => set((state) => ({ invertLightPages: !state.invertLightPages })),
}));
