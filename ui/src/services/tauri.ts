import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  Bookmark,
  FileListResponse,
  FileMatches,
  FileMetadataUpdate,
  IndexStatus,
  AddOutcome,
  CitationResult,
  IntegrationStatus,
  MatchRef,
  DocumentMetadata,
  ModelDescriptor,
  NewBookmark,
  PreviewData,
  SelectedEmbedder,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";
import { randomId } from "../lib/types";
import type { SearchApi, DesktopSourceApi, DataPaths } from "./api";

export class TauriSearchApi implements SearchApi {
  async search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string> {
    // Generate the ID here so we can register listeners before the backend
    // starts emitting, eliminating the race where search-complete fires before
    // the listener exists.
    const searchId = randomId();

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

  async listBookmarks(): Promise<Bookmark[]> {
    return invoke<Bookmark[]>("list_bookmarks");
  }

  async addBookmark(bookmark: NewBookmark): Promise<Bookmark> {
    return invoke<Bookmark>("add_bookmark", { bookmark });
  }

  async removeBookmark(id: string): Promise<void> {
    return invoke("remove_bookmark", { id });
  }

  async updateBookmarkNote(id: string, note: string | null): Promise<Bookmark> {
    return invoke<Bookmark>("update_bookmark_note", { id, note });
  }

  async listFiles(root: string): Promise<FileListResponse> {
    return invoke<FileListResponse>("list_files", { root });
  }

  async openFile(path: string): Promise<PreviewData> {
    return invoke<PreviewData>("open_file", { path });
  }

  async renameFile(path: string, newName: string): Promise<string> {
    return invoke<string>("rename_file", { path, newName });
  }

  async getFileMetadata(path: string): Promise<DocumentMetadata> {
    return invoke<DocumentMetadata>("get_file_metadata", { path });
  }

  async resolveFileMetadata(path: string): Promise<DocumentMetadata> {
    return invoke<DocumentMetadata>("resolve_file_metadata", { path });
  }

  async refreshFileMetadata(): Promise<void> {
    await invoke("refresh_file_metadata");
  }

  async zoteroStatus(): Promise<IntegrationStatus> {
    return invoke<IntegrationStatus>("zotero_status");
  }

  async zoteroAddItem(path: string): Promise<AddOutcome> {
    return invoke<AddOutcome>("zotero_add_item", { path });
  }

  async zoteroGenerateCitation(path: string): Promise<CitationResult> {
    return invoke<CitationResult>("zotero_generate_citation", { path });
  }

  resolvePdfUrl(path: string): string {
    return convertFileSrc(path);
  }

  async getLogs(): Promise<string[]> {
    return invoke<string[]>("get_logs");
  }

  async clearLogs(): Promise<void> {
    return invoke("clear_logs");
  }

  async getPythonInfo(): Promise<string> {
    return invoke<string>("get_python_info");
  }

  async getSupportedEngines(): Promise<EmbeddingEngine[]> {
    return invoke<EmbeddingEngine[]>("get_supported_engines");
  }

  async getDataPaths(): Promise<DataPaths> {
    return invoke<DataPaths>("get_data_paths");
  }

  async openPath(path: string): Promise<void> {
    return invoke("open_path", { path });
  }

  async revealPath(path: string): Promise<void> {
    return invoke("reveal_path", { path });
  }

  async writeClipboard(text: string): Promise<void> {
    return writeText(text);
  }

  // ── Worker Management ────────────────────────────────────────────────────────

  async getWorkerStatus(): Promise<import("../lib/types").WorkerStatus> {
    return invoke<import("../lib/types").WorkerStatus>("get_worker_status");
  }

  async killWorker(): Promise<void> {
    return invoke("kill_worker");
  }

  async setWorkerTimeout(secs: number): Promise<void> {
    return invoke("set_worker_timeout", { secs });
  }

  // ── Semantic / embed commands ──────────────────────────────────────────────

  async listModels(engine: EmbeddingEngine): Promise<ModelDescriptor[]> {
    return invoke<ModelDescriptor[]>("list_models", { engine });
  }

  async getModelSize(engine: EmbeddingEngine, modelId: string): Promise<number> {
    return invoke<number>("get_model_size", { engine, modelId });
  }

  async downloadModel(selected: SelectedEmbedder): Promise<void> {
    return invoke("download_model", { selected });
  }

  async buildIndex(root: string, selected: SelectedEmbedder): Promise<void> {
    return invoke("build_index", { root, selected });
  }

  async cancelEmbed(): Promise<void> {
    return invoke("cancel_embed");
  }

  async getIndexStatus(): Promise<IndexStatus> {
    return invoke<IndexStatus>("get_index_status");
  }

  async isSemanticReady(): Promise<boolean> {
    return invoke<boolean>("is_semantic_ready");
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

  async onManagerEvent(handler: (event: string) => void): Promise<() => void> {
    return listen<string>("manager-event", (e) => handler(e.payload));
  }

  async onFileMetadataUpdated(
    handler: (updates: FileMetadataUpdate[]) => void,
  ): Promise<() => void> {
    return listen<FileMetadataUpdate[]>("file-metadata-updated", (e) => handler(e.payload));
  }
}

export class TauriSourceApi implements DesktopSourceApi {
  type = "desktop" as const;

  async pickDirectory(): Promise<string | null> {
    return invoke<string | null>("pick_directory");
  }
}
