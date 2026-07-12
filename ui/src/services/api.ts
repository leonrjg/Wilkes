import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  Bookmark,
  FileListResponse,
  FileListChanged,
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
  OpenAlexWork,
  PreviewData,
  RelatedDocument,
  RelatedDocumentsQuery,
  SelectedEmbedder,
  SemanticScholarPaper,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";

export interface DataPaths {
  app_data: string;
}

// Shared across desktop and web. All methods are identical.
export interface SearchApi {
  search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string>;
  cancelSearch(searchId: string): Promise<void>;
  relatedDocuments(query: RelatedDocumentsQuery): Promise<RelatedDocument[]>;
  preview(matchRef: MatchRef): Promise<PreviewData>;
  getSettings(): Promise<Settings>;
  updateSettings(patch: Partial<Settings>): Promise<Settings>;
  listBookmarks(): Promise<Bookmark[]>;
  addBookmark(bookmark: NewBookmark): Promise<Bookmark>;
  removeBookmark(id: string): Promise<void>;
  updateBookmarkNote(id: string, note: string | null): Promise<Bookmark>;
  listFiles(root: string): Promise<FileListResponse>;
  openFile(path: string): Promise<PreviewData>;
  renameFile(path: string, newName: string): Promise<string>;
  /** File-based extraction only (fast; used for the viewer's first paint). */
  getFileMetadata(path: string): Promise<DocumentMetadata>;
  /** Authoritative metadata: file-based overridden by Zotero when it resolves. */
  resolveFileMetadata(path: string): Promise<DocumentMetadata>;
  /** Re-derive metadata for every cached file, or one file when `path` is provided. */
  refreshFileMetadata(path?: string): Promise<void>;
  zoteroStatus(): Promise<IntegrationStatus>;
  zoteroAddItem(path: string): Promise<AddOutcome>;
  zoteroGenerateCitation(path: string): Promise<CitationResult>;
  semanticScholarStatus(): Promise<IntegrationStatus>;
  semanticScholarLookup(doi: string): Promise<SemanticScholarPaper>;
  openAlexStatus(): Promise<IntegrationStatus>;
  openAlexLookup(doi: string): Promise<OpenAlexWork>;
  resolvePdfUrl(path: string): string;
  getLogs(): Promise<string[]>;
  clearLogs(): Promise<void>;
  getPythonInfo(): Promise<string>;
  getSupportedEngines(): Promise<EmbeddingEngine[]>;
  getDataPaths(): Promise<DataPaths>;
  openPath(path: string): Promise<void>;
  revealPath(path: string): Promise<void>;
  /** Write text to the system clipboard. On desktop this goes through the
   *  native plugin, which (unlike `navigator.clipboard`) does not require the
   *  call to happen inside a transient user-activation window. */
  writeClipboard(text: string): Promise<void>;

  // ── Worker Management ────────────────────────────────────────────────────────
  getWorkerStatus(): Promise<import("../lib/types").WorkerStatus>;
  killWorker(): Promise<void>;
  setWorkerTimeout(secs: number): Promise<void>;

  // ── Semantic / embed commands ──────────────────────────────────────────────
  listModels(engine: EmbeddingEngine): Promise<ModelDescriptor[]>;
  getModelSize(engine: EmbeddingEngine, modelId: string): Promise<number>;
  downloadModel(selected: SelectedEmbedder): Promise<void>;
  buildIndex(root: string, selected: SelectedEmbedder): Promise<void>;
  cancelEmbed(): Promise<void>;
  getIndexStatus(root?: string): Promise<IndexStatus>;
  isSemanticReady(): Promise<boolean>;
  deleteIndex(root?: string): Promise<void>;

  onEmbedProgress(handler: (progress: EmbedProgress) => void): Promise<() => void>;
  onEmbedDone(handler: (done: EmbedDone) => void): Promise<() => void>;
  onEmbedError(handler: (err: EmbedError) => void): Promise<() => void>;
  onManagerEvent(handler: (event: string) => void): Promise<() => void>;
  onFileListChanged(handler: (event: FileListChanged) => void): Promise<() => void>;
  onFileMetadataUpdated(
    handler: (updates: FileMetadataUpdate[]) => void,
  ): Promise<() => void>;
}

// Desktop: native directory picker.
// Web: file upload returning a server-side root path.
export interface SourceApi {
  type: "desktop" | "web";
  deletionKind: "trash" | "permanent";
  deleteFile(path: string): Promise<void>;
}

export interface DesktopSourceApi extends SourceApi {
  type: "desktop";
  pickDirectory(): Promise<string | null>;
  importDroppedFiles(paths: string[], root: string): Promise<string[]>;
  moveFile(path: string, targetRoot: string): Promise<string>;
  listDirectories(path: string): Promise<string[]>;
}

export interface WebSourceApi extends SourceApi {
  type: "web";
  uploadFiles(files: File[]): Promise<string>;
  deleteAll(): Promise<void>;
}
