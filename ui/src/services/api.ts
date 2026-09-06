import type {
  EmbedDone,
  EmbedError,
  EmbedProgress,
  EmbeddingEngine,
  Bookmark,
  BookmarkClustersQuery,
  CatalogueDownload,
  CatalogueCourse,
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  CatalogueFetchProgress,
  CatalogueProbe,
  CatalogueSearchResponse,
  CatalogueStatus,
  CatalogueSyncResponse,
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
  RootCoverage,
  AddOutcome,
  CitationResult,
  IntegrationStatus,
  ManifestSummary,
  ProbeReport,
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
  RecognitionEngine,
  RecognizerCatalogue,
  RecognizerInventory,
  NativeOpenRequest,
} from "../lib/types";

export interface DataPaths {
  /** The installation's data directory: the one that contains `workspaces/`. */
  app_data: string;
  /** The active workspace's own directory, under `app_data/workspaces/<id>`. */
  workspace: string;
}

// Shared across desktop and web. All methods are identical.
export interface SearchApi {
  getStartupStatus(): Promise<StartupStatus>;
  /** Desktop external-open bridge. These are absent from the HTTP surface,
   * where the operating system cannot launch a local file into the page. */
  getGlobalSettings?(): Promise<Settings>;
  previewStandalone?(matchRef: MatchRef): Promise<PreviewData>;
  getStandaloneFileMetadata?(path: string): Promise<DocumentMetadata>;
  /** Announce that this window is listening, and take what arrived before it
   * was. Answered per window, so the standalone reader and the main window
   * each drain only what was addressed to them. */
  nativeOpenReady?(): Promise<NativeOpenRequest[]>;
  onNativeOpen?(handler: (request: NativeOpenRequest) => void): Promise<() => void>;
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
  /** What a draft manifest declares, including the one host it will contact.
   *  Offline: nothing is requested and nothing is saved. */
  customIntegrationSummary(manifest: string): Promise<ManifestSummary>;
  /** Run a draft manifest's search capability once against the real service
   *  and report what was mapped, what was not, and why. */
  customIntegrationProbe(
    manifest: string,
    secrets: Record<string, string>,
  ): Promise<ProbeReport>;
  customIntegrationStatus(id: string): Promise<IntegrationStatus>;
  /** A URL this application will serve a local file at, whatever the file is:
   *  the PDF a reader loads, and the pictures an HTML document sits beside. */
  resolveAssetUrl(path: string): string;
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

  // ── Catalogue ──────────────────────────────────────────────────────────────
  /** What the mirror holds, per provider, and when each was last fetched.
   *  Installation-wide: the mirror lives beside the model cache, not in the
   *  workspace, so these numbers do not change when the workspace does. */
  catalogueStatus(): Promise<CatalogueStatus>;
  /** Recall over the mirror — wide, and explicitly not a ranking of which
   *  record is the better thing to read. Batched because a caller with several
   *  gaps should pay for one round trip, and keyed so answers can be reattached
   *  without relying on order. */
  catalogueSearch(probes: CatalogueProbe[], limit?: number): Promise<CatalogueSearchResponse>;
  /** Refreshes the named providers, or every one of them when none is named.
   *  All four is a minutes-long call; name one at a time to show progress. */
  catalogueSync(providers?: string[]): Promise<CatalogueSyncResponse>;
  /** Fetches a candidate into the workspace's uploads directory. Getting it
   *  from there into a library root is a separate, user-driven step. */
  catalogueAcquire(url: string, filename?: string): Promise<CatalogueDownload>;
  /** Each page of a provider's catalogue, as it lands. A whole-catalogue fetch
   *  is minutes long, and this is the only thing that happens in the middle. */
  onCatalogueSyncProgress(
    handler: (progress: CatalogueFetchProgress) => void,
  ): Promise<() => void>;
  /** Fetches a whole course — every PDF it publishes plus a generated Markdown
   *  document holding the syllabus, calendar and reading list — into a
   *  directory of its own under uploads. A course has no single `pdf_url`
   *  because a course is not a file, which is why this is its own call. */
  catalogueAcquireCourse(courseUrl: string): Promise<CatalogueCourse>;
  /** Bytes of a document being acquired, as they arrive. */
  onCatalogueDownloadProgress(
    handler: (progress: CatalogueDownloadProgress) => void,
  ): Promise<() => void>;
  /** How far a course acquisition has got: the manifest walk, then the
   *  documents. Separate from the byte stream above, which cannot say which
   *  of forty documents it is reporting. */
  onCatalogueCourseProgress(
    handler: (progress: CatalogueCourseProgress) => void,
  ): Promise<() => void>;

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
  /** What the most recent indexing job for `root` did, document by document. */
  indexActivity(root: string): Promise<import("../lib/types").IndexActivity>;
  /** Index the documents the last job for `root` never reached. */
  continueIndexJob(root: string, selected: SelectedEmbedder): Promise<void>;
  /** Re-attempt the documents the last job for `root` failed on. */
  retryFailedDocuments(root: string, selected: SelectedEmbedder): Promise<void>;
  cancelEmbed(): Promise<void>;
  getIndexStatus(root?: string): Promise<IndexStatus>;
  /** How much of each root the index covers, so the interface can say which
   *  roots are indexed without starting a build to find out. */
  indexCoverage(roots: string[]): Promise<RootCoverage[]>;
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
  /** Every recognizer this build can read with, and the engines it compiled
   *  in — the recognition counterpart of `embedderCapabilities`. */
  imageRecognizerCatalogue(): Promise<RecognizerCatalogue>;
  /** What the named recognizer is, where it came from and under what licence.
   *  Static — it describes the recipe, not this machine — so it answers
   *  before the download it discloses, and for a recognizer that is merely
   *  being considered. */
  imageRecognizerInventory(
    engine: RecognitionEngine,
    modelId: string,
  ): Promise<RecognizerInventory>;
  /** Download and verify the named recognizer. Progress arrives on
   *  `onImageAnalysisProgress`, terminated by
   *  `onImageAnalysisDone`/`onImageAnalysisError`. Installing is not
   *  choosing: the analyzer is only re-attached when the settings already
   *  name this recognizer. */
  /** Download and verify the layout detector. Reports on the same stream as
   *  `installImageRecognizer`. No arguments: there is one detector and it is
   *  not chosen from a catalogue. */
  installLayoutDetector(): Promise<void>;
  installImageRecognizer(
    engine: RecognitionEngine,
    modelId: string,
  ): Promise<void>;
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
  onResearchStateUpdated(handler: () => void): Promise<() => void>;
  onFileMetadataUpdated(
    handler: (updates: FileMetadataUpdate[]) => void,
  ): Promise<() => void>;
}

export type PathKind = "directory" | "file";

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
  /** Imports into `root`, or into `folder` beneath it when given — created if
   *  absent. The folder is for things that are not one file: a course is forty
   *  PDFs that belong together, and loose in the root they are neither
   *  attributable nor importable alongside a second course. */
  importFiles(
    paths: string[],
    root: string,
    mode: "move" | "copy",
    folder?: string,
  ): Promise<string[]>;
  readClipboardFiles(): Promise<string[]>;
  moveFile(path: string, targetRoot: string): Promise<string>;
  listDirectories(path: string): Promise<string[]>;
  /** Classify paths (in the order given) so a drop can route folders and files apart. */
  pathKinds(paths: string[]): Promise<PathKind[]>;
  createDirectory(parent: string, name: string): Promise<string>;
}

export interface WebSourceApi extends SourceApi {
  type: "web";
  uploadFiles(files: File[]): Promise<string>;
  deleteAll(): Promise<void>;
}
