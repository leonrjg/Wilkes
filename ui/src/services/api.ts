import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  FileListResponse,
  FileMatches,
  IndexStatus,
  MatchRef,
  ModelDescriptor,
  PreviewData,
  SelectedEmbedder,
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
  preview(matchRef: MatchRef): Promise<PreviewData>;
  getSettings(): Promise<Settings>;
  updateSettings(patch: Partial<Settings>): Promise<Settings>;
  listFiles(root: string): Promise<FileListResponse>;
  openFile(path: string): Promise<PreviewData>;
  resolvePdfUrl(path: string): string;
  getLogs(): Promise<string[]>;
  clearLogs(): Promise<void>;
  getPythonInfo(): Promise<string>;
  getSupportedEngines(): Promise<EmbeddingEngine[]>;
  getDataPaths(): Promise<DataPaths>;
  openPath(path: string): Promise<void>;

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
  getIndexStatus(): Promise<IndexStatus>;
  isSemanticReady(): Promise<boolean>;
  deleteIndex(): Promise<void>;

  onEmbedProgress(handler: (progress: EmbedProgress) => void): Promise<() => void>;
  onEmbedDone(handler: (done: EmbedDone) => void): Promise<() => void>;
  onEmbedError(handler: (err: EmbedError) => void): Promise<() => void>;
  onManagerEvent(handler: (event: string) => void): Promise<() => void>;
}

// Desktop: native directory picker.
// Web: file upload returning a server-side root path.
export interface SourceApi {
  type: "desktop" | "web";
}

export interface DesktopSourceApi extends SourceApi {
  type: "desktop";
  pickDirectory(): Promise<string | null>;
}

export interface WebSourceApi extends SourceApi {
  type: "web";
  uploadFiles(files: File[]): Promise<string>;
  deleteFile(path: string): Promise<void>;
  deleteAll(): Promise<void>;
}
