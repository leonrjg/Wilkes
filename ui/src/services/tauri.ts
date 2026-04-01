import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import type {
  EmbedderModel,
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  FileEntry,
  FileMatches,
  IndexStatus,
  MatchRef,
  ModelDescriptor,
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
    // Generate the ID here so we can register listeners before the backend
    // starts emitting, eliminating the race where search-complete fires before
    // the listener exists.
    const searchId = crypto.randomUUID();

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

    await invoke("search", { query, searchId });
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

  // ── Semantic / embed commands ──────────────────────────────────────────────

  async listModels(engine: EmbeddingEngine): Promise<ModelDescriptor[]> {
    return invoke<ModelDescriptor[]>("list_models", { engine });
  }

  async getModelSize(engine: EmbeddingEngine, modelId: string): Promise<number> {
    return invoke<number>("get_model_size", { engine, modelId });
  }

  async downloadModel(model: EmbedderModel, engine: EmbeddingEngine): Promise<void> {
    return invoke("download_model", { model, engine });
  }

  async buildIndex(root: string, model: EmbedderModel, engine: EmbeddingEngine): Promise<void> {
    return invoke("build_index", { root, model, engine });
  }

  async cancelEmbed(): Promise<void> {
    return invoke("cancel_embed");
  }

  async getIndexStatus(): Promise<IndexStatus> {
    return invoke<IndexStatus>("get_index_status");
  }

  async deleteIndex(): Promise<void> {
    return invoke("delete_index");
  }

  async onEmbedProgress(
    handler: (progress: EmbedProgress) => void,
  ): Promise<() => void> {
    return listen<EmbedProgress>("embed-progress", (e) => handler(e.payload));
  }

  async onEmbedDone(handler: (done: EmbedDone) => void): Promise<() => void> {
    return listen<EmbedDone>("embed-done", (e) => handler(e.payload));
  }

  async onEmbedError(
    handler: (err: EmbedError) => void,
  ): Promise<() => void> {
    return listen<EmbedError>("embed-error", (e) => handler(e.payload));
  }
}

export class TauriSourceApi implements DesktopSourceApi {
  type = "desktop" as const;

  async pickDirectory(): Promise<string | null> {
    return invoke<string | null>("pick_directory");
  }
}
