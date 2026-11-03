import { create } from "zustand";
import type { LanPeer } from "../lib/types";

/**
 * Other Wilkes instances on the local network, as the backend last reported
 * them.
 *
 * Kept here rather than fetched when a file menu opens because the menu is
 * built synchronously from the click: it has to already know where a file can
 * be sent. Written only by `useGlobalEvents`, from the initial listing and
 * every `lan-peers-changed` after it.
 */
interface LanPeersStore {
  peers: LanPeer[];
  setPeers(peers: LanPeer[]): void;
}

export const useLanPeersStore = create<LanPeersStore>((set) => ({
  peers: [],
  setPeers: (peers) => set({ peers }),
}));
