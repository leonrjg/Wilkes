import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  FileEntry,
  FileMatches,
  MatchRef,
  PreviewData,
  SearchStats,
  Settings,
} from "../lib/types";
import type { SearchApi } from "./api";

export const tauriApi: SearchApi = {
  async search(query, onResult, onComplete) {
    const searchId: string = await invoke("search", { query });

    const unlistenResult = await listen<FileMatches>(
      `search-result-${searchId}`,
      (event) => onResult(event.payload),
    );

    const unlistenComplete = await listen<SearchStats>(
      `search-complete-${searchId}`,
      (event) => {
        unlistenResult();
        unlistenComplete();
        onComplete(event.payload);
      },
    );

    return searchId;
  },

  async cancelSearch(searchId) {
    await invoke("cancel_search", { searchId });
  },

  async preview(matchRef: MatchRef) {
    return invoke<PreviewData>("preview", { matchRef });
  },

  async getSettings() {
    return invoke<Settings>("get_settings");
  },

  async updateSettings(patch) {
    return invoke<Settings>("update_settings", { patch });
  },

  async listFiles(root: string) {
    return invoke<FileEntry[]>("list_files", { root });
  },

  async openFile(path: string) {
    return invoke<PreviewData>("open_file", { path });
  },

  async pickDirectory() {
    return invoke<string | null>("pick_directory");
  },
};
