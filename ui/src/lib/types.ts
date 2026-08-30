// Document coordinates are owned by the readers -- they are that package's
// vocabulary, and it must be usable by a host that has none of the types
// below. Re-exported here so the rest of Wilkes still has one import site.
import type {
  BoundingBox,
  ByteRange,
  SourceOrigin,
} from "@leonrjg/wilkes-reader";

export type { BoundingBox, ByteRange, SourceOrigin };

/** What a `Theme` resolves to once "System" has been asked of the OS. Defined
 *  by the readers, since it is what their host contract asks for. */
export type { ColorScheme } from "@leonrjg/wilkes-reader";

// Auto-generated from Rust types (manually maintained until tauri-specta is wired up).
// Keep in sync with crates/core/src/types.rs.

/** `crypto.randomUUID` is only available in secure contexts (HTTPS/localhost).
 *  Falls back to a Math.random-based UUID for plain-HTTP deployments. */
export function randomId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    return (c === "x" ? r : (r & 0x3) | 0x8).toString(16);
  });
}

export interface WorkspaceSummary {
  id: string;
  name: string;
  roots: string[];
  active_root: string | null;
  /**
   * True when another application owns the workspace and Wilkes may only read
   * it. Such a workspace is listed and searchable like any other; every write
   * to its documents, roots or index is refused by the backend.
   */
  read_only: boolean;
  /**
   * The owning application's name, so the user knows whose corpus they are
   * looking at. Null for an ordinary workspace.
   */
  managed_by: string | null;
}

export interface WorkspaceState {
  active_workspace_id: string;
  workspaces: WorkspaceSummary[];
}

export interface StartupAction {
  label: string;
  description: string;
  command?: string;
}

export interface StartupBlocker {
  id: string;
  feature: string;
  title: string;
  message: string;
  actions: StartupAction[];
}

export interface StartupStatus {
  blockers: StartupBlocker[];
}

export type SearchMode = "Grep" | "Semantic";
export type SearchScope =
  | { type: "corpus" }
  | { type: "all" }
  | { type: "file"; path: string };

export interface SearchQuery {
  pattern: string;
  is_regex: boolean;
  case_sensitive: boolean;
  root: string;
  /** 0 = unlimited */
  max_results: number;
  respect_gitignore: boolean;
  /** 0 = unlimited */
  max_file_size: number;
  context_lines: number;
  /** Defaults to "Grep" */
  mode: SearchMode;
  scope: SearchScope;
  supported_extensions: string[];
  collection_id?: string | null;
  tag_ids?: string[];
}

export type FileType = "PlainText" | "Pdf";

export interface Match {
  /** null for PDF chunks — highlight position is carried by origin.bbox. */
  text_range: ByteRange | null;
  matched_text: string;
  context_before: string;
  context_after: string;
  origin: SourceOrigin;
  /** Cosine similarity score for semantic matches; absent for grep matches. */
  score?: number;
}

export interface FileMatches {
  path: string;
  file_type: FileType;
  /** Composed cached document title, when metadata is available. */
  title?: string | null;
  /** Direct identity-field hits, which have no document line/page position. */
  field_matches?: SearchFieldMatch[];
  matches: Match[];
}

export type SearchField = "filename" | "title" | "author";

export interface SearchFieldMatch {
  field: SearchField;
  matched_text: string;
  context_before: string;
  context_after: string;
}

export interface RelatedDocumentsQuery {
  root: string;
  path: string;
  scope?: SearchScope;
  limit?: number | null;
  collection_id?: string | null;
}

export interface Tag { id: string; name: string; color: string | null }
export interface NewTag { name: string; color?: string | null }
export interface DocumentTagUpdate {
  paths: string[];
  add_tag_ids: string[];
  remove_tag_ids: string[];
}
export interface SmartCollection {
  id: string;
  name: string;
  expression: string;
  filter_schema_version: number;
  revision: number;
  created_at_ms: number;
  updated_at_ms: number;
}
export interface NewSmartCollection { name: string; expression: string }
export interface CollectionValidation { valid: boolean; error?: string | null }
export type SearchLogStatus = "running" | "completed" | "cancelled" | "failed";
export interface SearchLogEntry {
  id: string;
  query: SearchQuery;
  collection_name?: string | null;
  collection_revision?: number | null;
  initiated_by: string;
  started_at_ms: number;
  completed_at_ms?: number | null;
  result_count: number;
  duration_ms?: number | null;
  status: SearchLogStatus;
  error_message?: string | null;
}

export interface RelatedDocument extends FileEntry {
  score: number;
}

export interface CitationLinksQuery {
  root: string;
  path: string;
}

export interface CitationReference {
  doi: string;
  /** First document line containing this exact normalized DOI, when available. */
  citation_line?: string | null;
}

/** Citation neighbours resolved by DOI. The two document lists contain only
 *  library files; `all_references` contains every known outgoing DOI. */
export interface CitationLinks {
  references: FileEntry[];
  cited_by: FileEntry[];
  all_references: CitationReference[];
}

export interface MatchRef {
  path: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
}

export interface Bookmark {
  id: string;
  path: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
  quote: string;
  created_at: string;
  note?: string | null;
  /** Per-line rectangles (page coordinates) covering exactly the selected text.
   *  Empty for text bookmarks. */
  rects: BoundingBox[];
}

export type BookmarkClusterGranularity =
  | "much_fewer"
  | "fewer"
  | "balanced"
  | "more"
  | "much_more";

export interface BookmarkClustersQuery {
  bookmark_ids: string[];
  granularity?: BookmarkClusterGranularity;
}

export interface BookmarkCluster {
  /** Content-derived identity (sha256 over sorted member id:input_hash pairs).
   *  The only stable handle for patching a late-arriving label: clusters are
   *  recomputed on every call and `representative_bookmark_id` moves when
   *  granularity changes. */
  cluster_key: string;
  bookmark_ids: string[];
  representative_bookmark_id: string;
  cohesion: number;
  /** Absent until generated, or forever when generation is off. */
  label?: string | null;
}

export interface BookmarkClustersResult {
  clusters: BookmarkCluster[];
  unclustered_bookmark_ids: string[];
}

export interface ChunkTopicsQuery {
  root: string;
  path?: string | null;
  granularity?: BookmarkClusterGranularity;
}

export interface ChunkTopicMember {
  chunk_id: number;
  file_path: string;
  chunk_text: string;
  extraction_byte_range: ByteRange;
  origin: SourceOrigin;
}

export interface ChunkTopic {
  cluster_key: string;
  chunks: ChunkTopicMember[];
  representative_chunk_id: number;
  chunk_count: number;
  distinct_document_count: number;
  cohesion: number;
  /** Reach across every configured indexed library root for document-scoped
   *  topics. The source document is excluded from both counts. */
  library_coverage?: TopicLibraryCoverage | null;
  label?: string | null;
}

export interface TopicLibraryCoverage {
  related_document_count: number;
  eligible_document_count: number;
  /** Highest-similarity qualifying passages retained per related document. */
  chunks: ChunkTopicMember[];
}

export interface ChunkTopicsResult {
  topics: ChunkTopic[];
  total_chunk_count: number;
  sampled_chunk_count: number;
  total_document_count: number;
  sampled_document_count: number;
  input_cap: number;
}

export interface NewBookmark {
  path: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
  quote: string;
  note?: string | null;
  rects: BoundingBox[];
}

export type PreviewData =
  | {
      Text: {
        content: string;
        language: string | null;
        highlight_line: number;
        highlight_range: ByteRange;
      };
    }
  | {
      Pdf: {
        page: number;
        highlight_bbox: BoundingBox | null;
        /** The page areas whose text this document's reading owns rather than
         *  its glyphs — what a recognizer read where the page typeset a
         *  formula or a table, and whose flattened glyph run the reading
         *  dropped. Empty for a document the index has never read. */
        superseded: SupersededArea[];
      };
    };

/** A page area whose text the reading owns, ready to stand in place of what
 *  the page draws there. Mirrors `wilkes_core::types::SupersededArea`. */
export interface SupersededArea {
  page: number;
  bbox: BoundingBox;
  text: string;
}

export interface DocumentMetadata {
  title: string | null;
  author: string | null;
  doi: string | null;
  created_at: string | null;
  semantic_scholar?: SemanticScholarPaper | null;
  openalex?: OpenAlexWork | null;
}

export type IntegrationState =
  | "disabled"
  | "zotero_down"
  | "local_api_disabled"
  | "remote_api_down"
  | "rate_limited"
  | "ready";

export interface IntegrationStatus {
  id: string;
  enabled: boolean;
  state: IntegrationState;
  message: string;
  version: string | null;
}

export type AddOutcome =
  | { status: "added"; item_key: string | null }
  | { status: "already_present"; item_key: string }
  | { status: "possible_duplicate"; item_key: string; message: string };

export interface CitationResult {
  /** In-text citation (HTML from Zotero), e.g. "(Smith 2020)". */
  citation: string | null;
  /** Full bibliography entry (HTML from Zotero). */
  bibliography: string | null;
  /** True when resolved by a weak signal (filename/title); may be the wrong work. */
  low_confidence: boolean;
}

export interface SemanticScholarPaper {
  doi: string;
  paper_id: string;
  title: string | null;
  year: number | null;
  publication_date: string | null;
  venue: string | null;
  citation_count: number;
  external_ids: Record<string, unknown>;
  cached_at_ms: number;
}

export interface OpenAlexWork {
  doi: string;
  work_id: string;
  title: string | null;
  year: number | null;
  publication_date: string | null;
  venue: string | null;
  citation_count: number;
  external_ids: Record<string, unknown>;
  cached_at_ms: number;
}

export type ViewerMetadataStatus = "idle" | "loading" | "ready" | "failed";

export interface FileEntry {
  path: string;
  size_bytes: number;
  file_type: FileType;
  extension: string;
  created_at_ms?: number | null;
  modified_at_ms?: number | null;
  /** Document title from cached extracted metadata. Absent until metadata has
   * processed this file. */
  title?: string | null;
  /** Document author from cached extracted metadata. */
  author?: string | null;
  /** Normalized document DOI from cached extracted metadata. */
  doi?: string | null;
  /** Document publication date ("YYYY-MM") from cached extracted metadata.
   *  Absent until the metadata cache has processed this file. */
  publication_date?: string | null;
  /** Semantic Scholar citation count from cached extracted metadata. */
  citation_count?: number | null;
  metadata_conflicts?: Record<string, MetadataConflictValue[]>;
  tags?: Tag[];
}

export interface MetadataConflictValue {
  source: MetadataSourcePreference;
  value: string;
}

export const MetadataField = {
  Title: "title",
  Author: "author",
  Doi: "doi",
  PublicationDate: "publication_date",
  PaperId: "paper_id",
  Year: "year",
  Venue: "venue",
  CitationCount: "citation_count",
  ExternalIdsJson: "external_ids_json",
  CachedAtMs: "cached_at_ms",
  ExtractedAtMs: "extracted_at_ms",
} as const;

export type MetadataField = (typeof MetadataField)[keyof typeof MetadataField];

/** Payload entry of the `file-metadata-updated` event: cached document metadata
 *  filled in for a file after its background extraction completes. */
export interface FileMetadataUpdate {
  path: string;
  title?: string | null;
  author?: string | null;
  doi?: string | null;
  publication_date: string | null;
  citation_count?: number | null;
  metadata_conflicts?: Record<string, MetadataConflictValue[]>;
}

export interface FileListChanged {
  root: string;
}

export interface FileListResponse {
  files: FileEntry[];
  omitted: OmittedFileEntry[];
}

export interface OmittedFileEntry extends FileEntry {
  reason: OmittedFileReason;
}

export type OmittedFileReason = "TooLarge" | "UnsupportedExtension";
export type FileSortKey =
  | "filename"
  | "title"
  | "author"
  | "created"
  | "modified"
  | "size"
  | "publication"
  | "citations";
/** Optional document-metadata field that can be shown as a column in the file
 *  list. Extend alongside FILE_DISPLAY_FIELDS as FileEntry gains more fields. */
export type FileDisplayField =
  | "title"
  | "author"
  | "created"
  | "modified"
  | "size"
  | "publication"
  | "citations";
export type FileSortDirection = "asc" | "desc";

/** HuggingFace model code, e.g. "BAAI/bge-base-en-v1.5". */
export type EmbedderModel = string;

export type EmbeddingEngine = "SBERT" | "Candle" | "Fastembed";
export const ALL_ENGINES: EmbeddingEngine[] = ["SBERT", "Candle", "Fastembed"];

/** Where a model's retrieval prefixes came from, and whether that is known. */
export type PrefixSource = "discovered" | "curated" | "not_documented" | "undetermined";

/** Everything Wilkes says about one embedding model without loading it. */
export interface EmbedderCapability {
  engine: EmbeddingEngine;
  model_id: string;
  display_name: string;
  description: string;
  repository_id: string | null;
  /** Null for a model added by hand, whose width only a first load reveals.
   *  A picker must never fill this in. */
  dimension: number | null;
  supported_dimensions: number[];
  query_prefix: string | null;
  passage_prefix: string | null;
  prefix_source: PrefixSource;
  max_input_tokens: number | null;
  /** Whether the artifacts are on this machine already. */
  locally_available: boolean;
  /** Total bytes of all model files. Null for uncached models until fetched. */
  size_bytes: number | null;
  preferred_batch_size: number | null;
  /** False for a model the user added by hand. */
  catalogued: boolean;
  is_default: boolean;
  is_recommended: boolean;
}

/** What this Wilkes can embed with, as one answer. */
export interface EmbedderCapabilityManifest {
  engines: EmbeddingEngine[];
  roles: string[];
  models: EmbedderCapability[];
}
export interface CustomModel {
  engine: EmbeddingEngine;
  model_id: string;
}

export interface SelectedEmbedder {
  engine: EmbeddingEngine;
  model: EmbedderModel;
  dimension: number;
}

export interface SemanticSettings {
  enabled: boolean;
  selected: SelectedEmbedder;
  /** Per-engine device overrides. Missing entries use the engine's built-in default. */
  engine_devices: Partial<Record<EmbeddingEngine, string>>;
  index_path: string | null;
  custom_models: CustomModel[];
  chunk_size: number;
  chunk_overlap: number;
  topic_cloud_input_cap: number;
  worker_timeout_secs: number;
}

export type GenerationTask =
  | "cluster_label"
  | "relation_explanation"
  | "document_summary"
  | "search_results_summary"
  | "hypothetical_continuation"
  | "grounded_completion";

export type CompletionMode = "append" | "bridge";
export type CompletionScopeMode = "library" | "prefer" | "only";
export type CompletionFeedback = "accepted" | "partial" | "dismissed" | "typed_through";

export interface CompletionScope {
  mode: CompletionScopeMode;
  pinned: string[];
  excluded: string[];
}

export interface CompletionRequest {
  path: string;
  text: string;
  /** Unicode scalar offset; services translate from CodeMirror's UTF-16 position. */
  cursor: number;
  scope: CompletionScope;
  /** Candidates already shown at this document position. */
  avoid_suggestions: string[];
}

export interface CompletionSource {
  path: string;
  title: string;
  page: number | null;
  chunkIds: string[];
  score: number;
  pinned: boolean;
}

export type DocumentCoverage =
  | { kind: "full" }
  | { kind: "elided"; head_tokens: number; tail_tokens: number };

export interface ContextComposition {
  windowTokens: number;
  usedTokens: number;
  docCoverage: DocumentCoverage;
  retrievalTokens: number;
  docTokens: number;
  scopeMode: CompletionScopeMode;
}

export type CompletionEvent =
  | { kind: "retrieval"; sources: CompletionSource[]; hyde_query: string }
  | { kind: "context"; composition: ContextComposition }
  | { kind: "shown"; text: string; mode: CompletionMode }
  | { kind: "suppressed"; reason: string }
  | { kind: "error"; message: string };

export interface SteeringContribution {
  path: string;
  weight: number;
}

export interface SuppressionEntry {
  reason: string;
  candidate: string;
  hydeQuery: string;
}

export interface SessionSteering {
  documents: SteeringContribution[];
  suppressions: SuppressionEntry[];
}

export interface SearchResultsSummarySource {
  title: string;
  /** Anchor for citation links; the backend ignores unknown fields and uses
   * source positions only. */
  path: string;
}

export interface SearchResultsSummaryPassage {
  /** A cleaned source passage in authoritative search-result order. */
  text: string;
  /** Zero-based index into `sources`. */
  source_index: number;
}

export interface SearchResultsSummaryInput {
  query: string;
  sources: SearchResultsSummarySource[];
  passages: SearchResultsSummaryPassage[];
}

export interface GenerationSampling {
  temperature: number;
  top_p?: number | null;
  top_k?: number | null;
  repeat_penalty?: [number, number] | null;
  seed: number;
}

export type GenerationEngine = "candle" | "ollama";
export const ALL_GENERATION_ENGINES: GenerationEngine[] = ["candle", "ollama"];

export interface GenerationSettings {
  enabled: boolean;
  engine: GenerationEngine;
  model: string | null;
  device: string | null;
  ollama_url: string;
  /** Null/absent uses the maximum reported by the selected Ollama model. */
  context_tokens?: number | null;
  sampling_overrides: Partial<Record<GenerationTask, GenerationSampling>>;
}

/** Enrichment of the pictures inside a document: text transcribed out of
 *  them, and optionally a description of what they show. Off by default —
 *  turning it on installs a recognizer and re-reads every document that has a
 *  picture in it. */
export interface ImageAnalysisSettings {
  enabled: boolean;
  /** Which recognizer reads the pictures. */
  engine: RecognitionEngine;
  /** The recognizer's model id. Null takes the engine's default. */
  model: string | null;
  /** "auto" | "cpu" | "metal". Null takes the recognizer's default. */
  device: string | null;
  /** The Ollama tag figures are described with; empty means transcription
   *  only. The server is `generation.ollama_url`. */
  describer_model: string;
}

/** The recognizers Wilkes knows how to address.
 *
 *  Capitalised because that is what the backend enum serializes to; it accepts
 *  the lowercase spelling on the way in as well, and the two must not be
 *  allowed to drift into two different answers here. */
export type RecognitionEngine = "Onnx" | "Candle";
export const ALL_RECOGNITION_ENGINES: RecognitionEngine[] = ["Onnx", "Candle"];

/** What one recognizer is, and what choosing it would mean: what it costs to
 *  install, what confidence it admits a region at, and which kinds of region
 *  it produces under the task configuration Wilkes drives it with. */
export interface RecognizerDescriptor {
  engine: RecognitionEngine;
  model_id: string;
  display_name: string;
  description: string;
  /** The recognizer a fresh install reads with — one across the catalogue. */
  is_default: boolean;
  /** The recognizer this engine reads with unless told otherwise: what a
   *  picker selects when the engine is switched, and what an absent
   *  `image_analysis.model` resolves to. */
  is_engine_default: boolean;
  is_cached: boolean;
  footprint_bytes: number;
  admission_threshold: number;
  emits: string[];
}

/** What this Wilkes can recognize with, as one answer. The engines come from
 *  the build, so an engine missing from `engines` is one this build cannot
 *  read with at all — distinct from an engine that simply has no models. */
export interface RecognizerCatalogue {
  engines: RecognitionEngine[];
  models: RecognizerDescriptor[];
}

/** One file of the recognizer, as the inventory names it. */
export interface InventoriedArtifact {
  filename: string;
  size_bytes: number;
  sha256: string;
}

/** What the recognizer is, where it came from, and under what terms.
 *
 *  Wilkes fetches these files at the user's request rather than shipping them
 *  inside the application, so this is what the download is disclosed by: the
 *  licence, the pinned revision, and every file that will be written, each
 *  with the digest it is verified against. */
export interface RecognizerInventory {
  name: string;
  repo: string;
  revision: string;
  license: string;
  license_url: string;
  /** The works the weights are made of, upstream of the repository they are
   *  fetched from. */
  derived_from: string[];
  artifacts: InventoriedArtifact[];
  footprint_bytes: number;
}

/** HyDE: search with the embedding of an LLM-generated hypothetical answer,
 *  which sits in document space rather than terse-question space. Requires
 *  generation to be enabled and ready. */
export interface HydeSettings {
  enabled: boolean;
  /** Hypothetical passages generated and averaged together. */
  hypotheticals: number;
  /** Keep the original query vector in the average. */
  include_query: boolean;
}

/** Pseudo-relevance feedback (Rocchio): fold the centroid of the top initial
 *  hits back into the query vector and retrieve a second time. */
export interface PrfSettings {
  enabled: boolean;
  /** Top initial hits treated as pseudo-relevant feedback. */
  feedback_docs: number;
  /** Weight on the original query vector. */
  alpha: number;
  /** Weight on the feedback centroid. */
  beta: number;
}

/** Query-vector enhancement for semantic search. Both techniques reshape the
 *  vector before the nearest-neighbour lookup; neither re-ranks afterwards. */
export interface RetrievalSettings {
  hyde: HydeSettings;
  pseudo_relevance_feedback: PrfSettings;
}

/** Backend-neutral catalog entry for a generation model. */
export interface GeneratorDescriptor {
  engine: GenerationEngine;
  model_id: string;
  display_name: string;
  description: string;
  context_tokens: number;
  is_cached: boolean;
  is_default: boolean;
  is_recommended: boolean;
  size_bytes: number | null;
}

/** Emitted per cluster as its label finishes generating. */
export interface BookmarkClusterLabelled {
  cluster_key: string;
  label: string;
}

export interface ChunkTopicLabelled {
  request_id: string;
  cluster_key: string;
  label: string;
}

/** The shared lifecycle for every user-facing generation stream. */
export type GenerationStreamEvent =
  | {
      phase: "delta";
      request_id: string;
      task: GenerationTask;
      delta: string;
    }
  | {
      phase: "completed";
      request_id: string;
      task: GenerationTask;
      text: string;
    }
  | {
      phase: "failed";
      request_id: string;
      task: GenerationTask;
      error: string;
    };

export interface ZoteroSettings {
  enabled: boolean;
  base_url: string;
  citation_style: string;
}

export interface SemanticScholarSettings {
  enabled: boolean;
  base_url: string;
  api_key: string | null;
}

export interface OpenAlexSettings {
  enabled: boolean;
  base_url: string;
  email: string | null;
}

export interface IntegrationsSettings {
  zotero: ZoteroSettings;
  semantic_scholar: SemanticScholarSettings;
  openalex: OpenAlexSettings;
}

export type MetadataSourcePreference = "file" | "zotero" | "semantic_scholar" | "openalex";

export interface WorkerStatus {
  active: boolean;
  /** "embed" or "generate". A sibling of `engine`, not a replacement. */
  role?: string | null;
  engine: string | null;
  model: string | null;
  device: string | null;
  request_mode: string | null;
  pid: number | null;
  timeout_secs: number;
  generation?: {
    requested_device: string;
    fallback_reason?: string | null;
    model_load_micros?: number | null;
    timings?: {
      prompt_micros: number;
      decode_micros: number;
      constraint_micros: number;
    } | null;
  } | null;
}

export interface Settings {
  favorites: string[];
  recent_dirs: string[];
  last_directory: string | null;
  respect_gitignore: boolean;
  max_file_size: number;
  theme: Theme;
  search_prefer_semantic: boolean;
  grep_use_index: boolean;
  semantic: SemanticSettings;
  generation: GenerationSettings;
  /** Query-vector enhancement for semantic search (HyDE, pseudo-relevance
   *  feedback). Off by default. */
  retrieval: RetrievalSettings;
  integrations: IntegrationsSettings;
  primary_metadata_source?: MetadataSourcePreference;
  supported_extensions: string[];
  /** 0 = unlimited */
  max_results: number;
  bookmarks_dock: BookmarkDock;
  file_sort_key?: FileSortKey;
  file_sort_direction?: FileSortDirection;
  file_display_fields?: FileDisplayField[];
  /** Desired CSS-pixel body-text height used when PDFs are auto-zoomed. */
  pdf_auto_zoom_target_px: number;
  /** Persisted default agent for the chat pane (`SettingsModal`). The in-pane
   *  selector and header split-button dropdown may switch a session to a
   *  different backend transiently without touching this field. */
  chat_backend?: AgentBackend;
  /** Per-backend chat config defaults (model, thought level, mode). Written by
   *  the backend when a config option changes in the chat pane and applied to
   *  new sessions; the UI does not edit this directly. */
  chat_config?: ChatBackendConfig[];
  /** User-authored instructions applied to every chat turn. */
  chat_custom_instructions?: string;
  /** Optional MCP endpoint for regular Claude Code and Codex clients. */
  external_mcp?: ExternalMcpSettings;
  /** Optional HTTP API, served over the workspace this app already has open. */
  http_api?: HttpApiSettings;
  /** Transcription and description of the pictures inside documents. */
  image_analysis?: ImageAnalysisSettings;
}

export interface ExternalMcpSettings {
  enabled: boolean;
  require_token: boolean;
  bind_address: string;
  port: number;
}

export interface ExternalMcpStatus extends ExternalMcpSettings {
  running: boolean;
  url: string | null;
  token: string | null;
  error: string | null;
}

export interface HttpApiSettings {
  enabled: boolean;
  bind_address: string;
  port: number;
}

export interface HttpApiStatus extends HttpApiSettings {
  running: boolean;
  url: string | null;
  error: string | null;
}

export interface ChatBackendConfig {
  backend: AgentBackend;
  values: { id: string; value: string }[];
}

export type Theme = "System" | "Light" | "Dark";
export type BookmarkDock = "Left" | "Right";
export type AgentBackend = "ClaudeCode" | "Codex" | "Nanocoder";

export interface SearchCapabilities {
  supports_regex: boolean;
  supports_case_sensitivity: boolean;
  is_indexed: boolean;
  supported_file_types: string[];
  requires_index: boolean;
  semantic_index_built: boolean;
}

export interface SearchStats {
  files_scanned: number;
  total_matches: number;
  /** Time spent enumerating/filtering the file catalog before worker execution. */
  catalog_elapsed_ms?: number;
  elapsed_ms: number;
  /** PDFs whose stored text was read from the semantic index. */
  indexed_pdf_reads?: number;
  /** PDFs extracted from disk because indexed text was disabled or unavailable. */
  live_pdf_fallbacks?: number;
  /** Live PDF fallbacks caused by an enabled but non-resident index. */
  index_unavailable_fallbacks?: number;
  errors: string[];
  /** Exact generated passages whose embeddings affected semantic ranking. */
  hyde_documents?: string[];
}

export interface IndexStatus {
  indexed_files: number;
  total_chunks: number;
  built_at: number | null;
  build_duration_ms: number | null;
  engine: EmbeddingEngine;
  model_id: string;
  dimension: number;
  root_path: string | null;
  db_size_bytes: number | null;
}
export interface DownloadProgress {
  bytes_received: number;
  total_bytes: number;
  done: boolean;
}

export interface IndexBuildProgress {
  files_processed: number;
  total_files: number;
  message: string;
  done: boolean;
}

export type EmbedProgress =
  | { Download: DownloadProgress }
  | { Build: IndexBuildProgress };

export type EmbedOperation = "Download" | "Build";

export interface EmbedDone {
  operation: EmbedOperation;
}

export interface EmbedError {
  operation: EmbedOperation;
  message: string;
}

// ── Generation model install ─────────────────────────────────────────────────
// Deliberately separate from the embed events: the embed stream drives the
// global "indexing" state, which a generation download has no business
// entering. Same progress shape, different lifecycle.

export interface GenerationDone {
  model: string;
}

export interface GenerationError {
  message: string;
}

// ── Chat (ACP) ───────────────────────────────────────────────────────────────
// Mirrors crates/api/src/commands/chat.rs and wilkes_agent::session::ChatEvent.
// Desktop-only for v1 (see docs/chat-agent-integration-spec.md §11) -- not
// part of `SearchApi`, which is shared with the web/server build.

export interface BackendStatus {
  backend: AgentBackend;
  label: string;
  available: boolean;
  auth_note: string;
  installable: boolean;
  unavailable_reason: string | null;
}

export interface ChatToolLocation {
  path: string;
  line: number | null;
}

export interface ChatContextFileRecord {
  path: string;
  pages: number | null;
}

export interface ChatActiveDocRecord {
  path: string;
  page: number | null;
}

/** A tool call's own reported content -- the detail behind the compact chip. */
export type ChatToolContentBlock =
  | { kind: "text"; text: string }
  | { kind: "diff"; path: string; old_text: string | null; new_text: string }
  | { kind: "terminal"; terminal_id: string };

/** One choice offered for a surfaced permission request, mirrored from the
 *  agent's own `PermissionOption`. */
export interface ChatPermissionOption {
  option_id: string;
  name: string;
  kind: string;
}

/** One `chat/update-<turnId>` payload. Tool fields are a patch: `undefined`
 *  means "unchanged from the last update for this tool_call_id". */
export type ChatUpdate =
  | { kind: "text"; delta: string }
  | { kind: "thought"; delta: string }
  | {
      kind: "tool";
      tool_call_id: string;
      title?: string | null;
      status?: string | null;
      locations?: ChatToolLocation[] | null;
      content?: ChatToolContentBlock[] | null;
      raw_input?: unknown;
      raw_output?: unknown;
    }
  | {
      kind: "permission";
      request_id: string;
      tool_call_id: string;
      title?: string | null;
      options: ChatPermissionOption[];
    }
  | { kind: "error"; message: string };

export interface ChatDone {
  stop_reason: string;
}

/** ACP session configuration (model, mode, thought level, ...), reported by
 *  `session/new` and settable via `session/set_config_option`. Not every
 *  agent exposes every category -- Claude's adapter reports `model`,
 *  `thought_level`, and an uncategorized agent-variant picker, for example. */
export interface ChatConfigChoice {
  value: string;
  name: string;
  group: string | null;
}

export interface ChatConfigOption {
  id: string;
  name: string;
  category: string | null;
  current_value: string;
  choices: ChatConfigChoice[];
}

export interface ChatReplayToolCall {
  tool_call_id: string;
  title: string;
  status: string;
  locations: ChatToolLocation[];
  content: ChatToolContentBlock[];
  raw_input: unknown;
  raw_output: unknown;
}

export type ChatReplayContentBlock =
  | { kind: "text"; text: string }
  | { kind: "tool"; tool: ChatReplayToolCall };

export interface ChatReplayMessage {
  role: "user" | "assistant";
  thought: string;
  content: ChatReplayContentBlock[];
}

export interface ChatMessageRecord extends ChatReplayMessage {
  message_id: string;
  turn_id: string | null;
  error: string | null;
  environment: {
    context_files: ChatContextFileRecord[];
    active_doc: ChatActiveDocRecord | null;
    search_root: string | null;
    config_values: { id: string; value: string }[];
  } | null;
}

export interface ChatConversationRecord {
  conversation_id: string;
  backend: AgentBackend;
  backend_session_id: string;
  cwd: string;
  title: string;
  created_at: string;
  updated_at: string;
  last_opened_at: string;
  context_files: ChatContextFileRecord[];
  active_doc: ChatActiveDocRecord | null;
  config_values: { id: string; value: string }[];
  messages: ChatMessageRecord[];
  parent_conversation_id: string | null;
  forked_from_message_id: string | null;
  branch_history_pending: boolean;
}

export interface ChatStartResult {
  session_id: string;
  conversation_id: string | null;
  backend_session_id: string | null;
  config_options: ChatConfigOption[];
  messages: ChatMessageRecord[];
  context_files: ChatContextFileRecord[];
  active_doc: ChatActiveDocRecord | null;
}

export interface ChatSendResult {
  conversation_id: string | null;
}
