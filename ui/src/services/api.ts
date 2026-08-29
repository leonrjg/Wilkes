import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  Bookmark,
  BookmarkClustersQuery,
  BookmarkClustersResult,
  ChunkTopicsQuery,
  ChunkTopicsResult,
  FileListResponse,
  FileListChanged,
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
  EmbedderCapabilityManifest,
  CitationLinks,
  CitationLinksQuery,
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
  Tag,
  NewTag,
  DocumentTagUpdate,
  SmartCollection,
  NewSmartCollection,
  CollectionValidation,
  SearchLogEntry,
  SearchResultsSummaryInput,
  ExternalMcpStatus,
  HttpApiStatus,
  BookmarkClusterLabelled,
  ChunkTopicLabelled,
  GeneratorDescriptor,
  GenerationStreamEvent,
  CompletionEvent,
  CompletionFeedback,
  CompletionRequest,
  SessionSteering,
  WorkspaceState,
  WorkspaceSummary,
  StartupStatus,
  RecognizerInventory,
} from "../lib/types";

export interface DataPaths {
  app_data: string;
}

// Shared across desktop and web. All methods are identical.
export interface SearchApi {
  getStartupStatus(): Promise<StartupStatus>;
  search(
    query: SearchQuery,
    onResult: (fm: FileMatches) => void,
    onComplete: (stats: SearchStats) => void,
  ): Promise<string>;
  cancelSearch(searchId: string): Promise<void>;
  relatedDocuments(query: RelatedDocumentsQuery): Promise<RelatedDocument[]>;
  citationLinks(query: CitationLinksQuery): Promise<CitationLinks>;
  preview(matchRef: MatchRef): Promise<PreviewData>;
  getSettings(): Promise<Settings>;
  updateSettings(patch: Partial<Settings>): Promise<Settings>;
  listWorkspaces(): Promise<WorkspaceState>;
  createWorkspace(name: string): Promise<WorkspaceSummary>;
  renameWorkspace(workspaceId: string, name: string): Promise<WorkspaceSummary>;
  switchWorkspace(workspaceId: string): Promise<WorkspaceState>;
  /** Desktop-only lifecycle controls for the opt-in external MCP endpoint. */
  getExternalMcpStatus?(): Promise<ExternalMcpStatus>;
  /** Desktop-only live viewer context exposed by the external MCP endpoint. */
  setActiveDocument?(path: string | null, page?: number | null): Promise<void>;
  configureExternalMcp?(
    enabled: boolean,
    requireToken: boolean,
    bindAddress: string,
    port: number,
  ): Promise<ExternalMcpStatus>;
  rotateExternalMcpToken?(): Promise<ExternalMcpStatus>;
  /** Desktop-only lifecycle controls for the opt-in HTTP API. The web build is
   *  already served by a `wilkes-server`, which always exposes it. */
  getHttpApiStatus?(): Promise<HttpApiStatus>;
  configureHttpApi?(
    enabled: boolean,
    bindAddress: string,
    port: number,
  ): Promise<HttpApiStatus>;
  listBookmarks(): Promise<Bookmark[]>;
  addBookmark(bookmark: NewBookmark): Promise<Bookmark>;
  removeBookmark(id: string): Promise<void>;
  updateBookmarkNote(id: string, note: string | null): Promise<Bookmark>;
  clusterBookmarks(query: BookmarkClustersQuery): Promise<BookmarkClustersResult>;
  chunkTopics(requestId: string, query: ChunkTopicsQuery): Promise<ChunkTopicsResult>;
  cancelChunkTopics(requestId: string): Promise<void>;
  listFiles(root: string, collectionId?: string | null, tagIds?: string[], collectionExpression?: string | null): Promise<FileListResponse>;
  listTags(): Promise<Tag[]>;
  createTag(tag: NewTag): Promise<Tag>;
  updateTag(id: string, tag: NewTag): Promise<Tag>;
  deleteTag(id: string): Promise<void>;
  updateDocumentTags(update: DocumentTagUpdate): Promise<void>;
  listSmartCollections(): Promise<SmartCollection[]>;
  createSmartCollection(collection: NewSmartCollection): Promise<SmartCollection>;
  updateSmartCollection(id: string, collection: NewSmartCollection): Promise<SmartCollection>;
  deleteSmartCollection(id: string): Promise<void>;
  validateSmartCollection(expression: string): Promise<CollectionValidation>;
  listSearchLog(limit?: number): Promise<SearchLogEntry[]>;
  deleteSearchLog(id: string): Promise<void>;
  clearSearchLog(): Promise<void>;
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
  getDataPaths(): Promise<DataPaths>;
  openPath(path: string): Promise<void>;
  revealPath(path: string): Promise<void>;
  /** Write text to the system clipboard. On desktop this goes through the
   *  native plugin, which (unlike `navigator.clipboard`) does not require the
   *  call to happen inside a transient user-activation window. */
  writeClipboard(text: string): Promise<void>;

  // ── Worker Management ────────────────────────────────────────────────────────
  getWorkerStatus(): Promise<import("../lib/types").WorkerStatus>;
  /** Every worker, one entry per role. Two processes can die independently, so
   *  a single status would misreport a dead generation worker as healthy. */
  getWorkerStatuses(): Promise<import("../lib/types").WorkerStatus[]>;
  killWorker(): Promise<void>;
  setWorkerTimeout(secs: number): Promise<void>;

  // ── Semantic / embed commands ──────────────────────────────────────────────
  /** What this build can embed with: the engines, and every model under each
   *  — catalogued and hand-added alike. One call, because deciding what a
   *  model is by joining two replies is how a picker and a backend come to
   *  disagree about which models exist. */
  getEmbedderCapabilities(): Promise<EmbedderCapabilityManifest>;
  getModelSize(engine: EmbeddingEngine, modelId: string): Promise<number>;
  downloadModel(selected: SelectedEmbedder): Promise<void>;
  buildIndex(root: string, selected: SelectedEmbedder): Promise<void>;
  cancelEmbed(): Promise<void>;
  getIndexStatus(root?: string): Promise<IndexStatus>;
  isSemanticReady(): Promise<boolean>;
  deleteIndex(root?: string): Promise<void>;

  // ── Generation commands ────────────────────────────────────────────────────
  /** The single gate. Every LLM-dependent affordance hangs off this, never off
   *  `settings.generation.enabled` directly — enabled but not installed is
   *  exactly the state that produces a spinner which never resolves. */
  isGenerationReady(): Promise<boolean>;
  listGenerationModels(): Promise<GeneratorDescriptor[]>;
  getGenerationModelSize(modelId: string): Promise<number>;
  /** Download if needed, then attach. Progress arrives on
   *  `onGenerationProgress`, terminated by `onGenerationDone`/`onGenerationError`. */
  loadGenerationModel(): Promise<boolean>;
  /** Whether the image recognizer the shipped recipe names is on disk. The
   *  gate for image enrichment, for the same reason `isGenerationReady` is
   *  the gate for generation: enabled but not installed is a state that
   *  reads as broken. */
  isImageRecognizerInstalled(): Promise<boolean>;
  /** What the recognizer is, where it came from and under what licence.
   *  Static — it describes the shipped recipe, not this machine — so it
   *  answers before the download it discloses. */
  imageRecognizerInventory(): Promise<RecognizerInventory>;
  /** Download if needed, verify, then attach the analyzer the settings
   *  describe. Progress arrives on `onImageAnalysisProgress`, terminated by
   *  `onImageAnalysisDone`/`onImageAnalysisError`. */
  installImageRecognizer(): Promise<void>;
  /** Starts a related-document explanation. Its complete lifecycle arrives on
   *  `onGenerationStream`, correlated by `requestId`. */
  explainRelatedDocument(
    requestId: string,
    anchorPath: string,
    path: string,
  ): Promise<void>;
  /** Starts a summary of one viewer document. */
  summarizeDocument(requestId: string, path: string): Promise<void>;
  /** Starts cited synthesis over cleaned passages in search-result order. */
  summarizeSearchResults(
    requestId: string,
    input: SearchResultsSummaryInput,
  ): Promise<void>;
  requestCompletion(completionId: string, request: CompletionRequest): Promise<void>;
  cancelCompletion(completionId: string): Promise<void>;
  completionFeedback(completionId: string, feedback: CompletionFeedback): Promise<void>;
  getSessionSteering(): Promise<SessionSteering>;
  resetSessionSteering(): Promise<void>;
  saveDocument(path: string, text: string): Promise<void>;

  onBookmarkClusterLabelled(
    handler: (event: BookmarkClusterLabelled) => void,
  ): Promise<() => void>;
  onChunkTopicLabelled(handler: (event: ChunkTopicLabelled) => void): Promise<() => void>;
  onGenerationStream(handler: (event: GenerationStreamEvent) => void): Promise<() => void>;
  onCompletion(
    completionId: string,
    handler: (event: CompletionEvent) => void,
  ): Promise<() => void>;

  onGenerationProgress(handler: (progress: EmbedProgress) => void): Promise<() => void>;
  onImageAnalysisProgress(handler: (progress: EmbedProgress) => void): Promise<() => void>;
  onImageAnalysisDone(handler: () => void): Promise<() => void>;
  onImageAnalysisError(handler: (err: GenerationError) => void): Promise<() => void>;
  onGenerationDone(handler: (done: GenerationDone) => void): Promise<() => void>;
  onGenerationError(handler: (err: GenerationError) => void): Promise<() => void>;

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
  importFiles(paths: string[], root: string, mode: "move" | "copy"): Promise<string[]>;
  readClipboardFiles(): Promise<string[]>;
  moveFile(path: string, targetRoot: string): Promise<string>;
  listDirectories(path: string): Promise<string[]>;
  createDirectory(parent: string, name: string): Promise<string>;
}

export interface WebSourceApi extends SourceApi {
  type: "web";
  uploadFiles(files: File[]): Promise<string>;
  deleteAll(): Promise<void>;
}
