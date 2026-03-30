import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import type {
  FileEntry,
  FileMatches,
  MatchRef,
  PreviewData,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";
import type { SearchApi, DesktopSourceApi } from "./api";

export class TauriSearchApi implements SearchApi {
  async search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string> {
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
  }

  async cancelSearch(searchId: string): Promise<void> {
    await invoke("cancel_search", { searchId });
  }

  async preview(matchRef: MatchRef): Promise<PreviewData> {
    return invoke<PreviewData>("preview", { matchRef });
  }

  async getSettings(): Promise<Settings> {
    return invoke<Settings>("get_settings");
  }

  async updateSettings(patch: Partial<Settings>): Promise<Settings> {
    return invoke<Settings>("update_settings", { patch });
  }

  async listFiles(root: string): Promise<FileEntry[]> {
    return invoke<FileEntry[]>("list_files", { root });
  }

  async openFile(path: string): Promise<PreviewData> {
    return invoke<PreviewData>("open_file", { path });
  }

  resolvePdfUrl(path: string): string {
    return convertFileSrc(path);
  }
}

export class TauriSourceApi implements DesktopSourceApi {
  type = "desktop" as const;

  async pickDirectory(): Promise<string | null> {
    return invoke<string | null>("pick_directory");
  }
}
