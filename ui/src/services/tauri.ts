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
  BookmarkClustersQuery,
  BookmarkClustersResult,
  FileListChanged,
  FileListResponse,
  FileMatches,
  FileMetadataUpdate,
  GenerationDone,
  GenerationError,
  IndexStatus,
  AddOutcome,
  CitationResult,
  IntegrationStatus,
  MatchRef,
  DocumentMetadata,
  ModelDescriptor,
  NewBookmark,
  OpenAlexWork,
  PreviewData,
  CitationLinks,
  CitationLinksQuery,
  RelatedDocument,
  RelatedDocumentsQuery,
  SelectedEmbedder,
  SemanticScholarPaper,
  SearchQuery,
  SearchStats,
  SearchResultsSummaryInput,
  Settings,
  Tag,
  NewTag,
  DocumentTagUpdate,
  SmartCollection,
  NewSmartCollection,
  CollectionValidation,
  SearchLogEntry,
  ExternalMcpStatus,
  BookmarkClusterLabelled,
  GeneratorDescriptor,
  GenerationStreamEvent,
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

  async relatedDocuments(query: RelatedDocumentsQuery): Promise<RelatedDocument[]> {
    return invoke<RelatedDocument[]>("related_documents", { query });
  }

  async citationLinks(query: CitationLinksQuery): Promise<CitationLinks> {
    return invoke<CitationLinks>("citation_links", { query });
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

  async getExternalMcpStatus(): Promise<ExternalMcpStatus> {
    return invoke<ExternalMcpStatus>("get_external_mcp_status");
  }

  async configureExternalMcp(
    enabled: boolean,
    requireToken: boolean,
    bindAddress: string,
    port: number,
  ): Promise<ExternalMcpStatus> {
    return invoke<ExternalMcpStatus>("configure_external_mcp", {
      enabled,
      requireToken,
      bindAddress,
      port,
    });
  }

  async rotateExternalMcpToken(): Promise<ExternalMcpStatus> {
    return invoke<ExternalMcpStatus>("rotate_external_mcp_token");
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

  async clusterBookmarks(query: BookmarkClustersQuery): Promise<BookmarkClustersResult> {
    return invoke<BookmarkClustersResult>("cluster_bookmarks", { query });
  }

  async listFiles(root: string, collectionId?: string | null, tagIds: string[] = [], collectionExpression?: string | null): Promise<FileListResponse> {
    return invoke<FileListResponse>("list_files", {
      root,
      ...(collectionId ? { collectionId } : {}),
      ...(tagIds.length ? { tagIds } : {}),
      ...(collectionExpression ? { collectionExpression } : {}),
    });
  }

  async listTags(): Promise<Tag[]> { return invoke<Tag[]>("list_tags"); }
  async createTag(tag: NewTag): Promise<Tag> { return invoke<Tag>("create_tag", { tag }); }
  async updateTag(id: string, tag: NewTag): Promise<Tag> { return invoke<Tag>("update_tag", { id, tag }); }
  async deleteTag(id: string): Promise<void> { await invoke("delete_tag", { id }); }
  async updateDocumentTags(update: DocumentTagUpdate): Promise<void> { await invoke("update_document_tags", { update }); }
  async listSmartCollections(): Promise<SmartCollection[]> { return invoke<SmartCollection[]>("list_smart_collections"); }
  async createSmartCollection(collection: NewSmartCollection): Promise<SmartCollection> { return invoke<SmartCollection>("create_smart_collection", { collection }); }
  async updateSmartCollection(id: string, collection: NewSmartCollection): Promise<SmartCollection> { return invoke<SmartCollection>("update_smart_collection", { id, collection }); }
  async deleteSmartCollection(id: string): Promise<void> { await invoke("delete_smart_collection", { id }); }
  async validateSmartCollection(expression: string): Promise<CollectionValidation> { return invoke<CollectionValidation>("validate_smart_collection", { expression }); }
  async listSearchLog(limit = 100): Promise<SearchLogEntry[]> { return invoke<SearchLogEntry[]>("list_search_log", { limit }); }
  async deleteSearchLog(id: string): Promise<void> { await invoke("delete_search_log", { id }); }
  async clearSearchLog(): Promise<void> { await invoke("clear_search_log"); }

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

  async refreshFileMetadata(path?: string): Promise<void> {
    await invoke("refresh_file_metadata", { path });
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

  async semanticScholarStatus(): Promise<IntegrationStatus> {
    return invoke<IntegrationStatus>("semantic_scholar_status");
  }

  async semanticScholarLookup(doi: string): Promise<SemanticScholarPaper> {
    return invoke<SemanticScholarPaper>("semantic_scholar_lookup", { doi });
  }

  async openAlexStatus(): Promise<IntegrationStatus> {
    return invoke<IntegrationStatus>("openalex_status");
  }

  async openAlexLookup(doi: string): Promise<OpenAlexWork> {
    return invoke<OpenAlexWork>("openalex_lookup", { doi });
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

  async getWorkerStatuses(): Promise<import("../lib/types").WorkerStatus[]> {
    return invoke<import("../lib/types").WorkerStatus[]>("get_worker_statuses");
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

  async getIndexStatus(root?: string): Promise<IndexStatus> {
    return invoke<IndexStatus>("get_index_status", { root: root ?? null });
  }

  async isSemanticReady(): Promise<boolean> {
    return invoke<boolean>("is_semantic_ready");
  }

  async deleteIndex(root?: string): Promise<void> {
    return invoke("delete_index", { root: root ?? null });
  }

  // ── Generation commands ────────────────────────────────────────────────────

  async isGenerationReady(): Promise<boolean> {
    return invoke<boolean>("is_generation_ready");
  }

  async listGenerationModels(): Promise<GeneratorDescriptor[]> {
    return invoke<GeneratorDescriptor[]>("list_generation_models");
  }

  async getGenerationModelSize(modelId: string): Promise<number> {
    return invoke<number>("get_generation_model_size", { modelId });
  }

  async loadGenerationModel(): Promise<boolean> {
    return invoke<boolean>("load_generation_model");
  }

  async explainRelatedDocument(
    requestId: string,
    anchorPath: string,
    path: string,
  ): Promise<void> {
    return invoke("explain_related_document", { requestId, anchorPath, path });
  }

  async summarizeDocument(requestId: string, path: string): Promise<void> {
    return invoke("summarize_document", { requestId, path });
  }

  async summarizeSearchResults(
    requestId: string,
    input: SearchResultsSummaryInput,
  ): Promise<void> {
    return invoke("summarize_search_results", { requestId, input });
  }

  async onBookmarkClusterLabelled(
    handler: (event: BookmarkClusterLabelled) => void,
  ): Promise<() => void> {
    return listen<BookmarkClusterLabelled>("bookmark-cluster-labelled", (e) =>
      handler(e.payload),
    );
  }

  async onGenerationStream(
    handler: (event: GenerationStreamEvent) => void,
  ): Promise<() => void> {
    return listen<GenerationStreamEvent>("generation-stream", (e) => handler(e.payload));
  }

  async onGenerationProgress(
    handler: (progress: EmbedProgress) => void,
  ): Promise<() => void> {
    return listen<EmbedProgress>("generation-progress", (e) => handler(e.payload));
  }

  async onGenerationDone(handler: (done: GenerationDone) => void): Promise<() => void> {
    return listen<GenerationDone>("generation-done", (e) => handler(e.payload));
  }

  async onGenerationError(handler: (err: GenerationError) => void): Promise<() => void> {
    return listen<GenerationError>("generation-error", (e) => handler(e.payload));
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

  async onFileListChanged(
    handler: (event: FileListChanged) => void,
  ): Promise<() => void> {
    return listen<FileListChanged>("file-list-changed", (e) => handler(e.payload));
  }

  async onFileMetadataUpdated(
    handler: (updates: FileMetadataUpdate[]) => void,
  ): Promise<() => void> {
    return listen<FileMetadataUpdate[]>("file-metadata-updated", (e) => handler(e.payload));
  }
}

export class TauriSourceApi implements DesktopSourceApi {
  type = "desktop" as const;
  deletionKind = "trash" as const;

  async pickDirectory(): Promise<string | null> {
    return invoke<string | null>("pick_directory");
  }

  async importFiles(
    paths: string[],
    root: string,
    mode: "move" | "copy",
  ): Promise<string[]> {
    return invoke<string[]>("import_files", { paths, root, mode });
  }

  async readClipboardFiles(): Promise<string[]> {
    return invoke<string[]>("read_clipboard_files");
  }

  async moveFile(path: string, targetRoot: string): Promise<string> {
    return invoke<string>("move_file", { path, targetRoot });
  }

  async listDirectories(path: string): Promise<string[]> {
    return invoke<string[]>("list_directories", { path });
  }

  async createDirectory(parent: string, name: string): Promise<string> {
    return invoke<string>("create_directory", { parent, name });
  }

  async deleteFile(path: string): Promise<void> {
    return invoke("trash_file", { path });
  }
}
