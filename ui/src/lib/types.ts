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

export interface ByteRange {
  start: number;
  end: number;
}

export type SearchMode = "Grep" | "Semantic";

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
  supported_extensions: string[];
}

export type FileType = "PlainText" | "Pdf";

export type SourceOrigin =
  | { TextFile: { line: number; col: number } }
  | { PdfPage: { page: number; bbox: BoundingBox | null } };

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
  matches: Match[];
}

export interface RelatedDocumentsQuery {
  root: string;
  path: string;
  limit?: number | null;
}

export interface RelatedDocument {
  path: string;
  file_type: FileType;
  score: number;
  indexed_chunks: number;
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

export interface NewBookmark {
  path: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
  quote: string;
  note?: string | null;
  rects: BoundingBox[];
}

export interface BoundingBox {
  x: number;
  y: number;
  width: number;
  height: number;
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
      };
    };

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
  /** Document publication date ("YYYY-MM") from cached extracted metadata.
   *  Absent until the metadata cache has processed this file. */
  publication_date?: string | null;
  /** Semantic Scholar citation count from cached extracted metadata. */
  citation_count?: number | null;
  metadata_conflicts?: Record<string, MetadataConflictValue[]>;
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

export interface ModelDescriptor {
  model_id: string;
  display_name: string;
  description: string;
  dimension: number;
  is_cached: boolean;
  is_default: boolean;
  is_recommended: boolean;
  /** Total bytes of all model files. Null for uncached models until fetched. */
  size_bytes: number | null;
  preferred_batch_size: number | null;
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
  worker_timeout_secs: number;
}

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
  engine: string | null;
  model: string | null;
  device: string | null;
  request_mode: string | null;
  pid: number | null;
  timeout_secs: number;
}

export interface Settings {
  favorites: string[];
  recent_dirs: string[];
  last_directory: string | null;
  respect_gitignore: boolean;
  max_file_size: number;
  theme: Theme;
  search_prefer_semantic: boolean;
  semantic: SemanticSettings;
  integrations: IntegrationsSettings;
  primary_metadata_source?: MetadataSourcePreference;
  supported_extensions: string[];
  /** 0 = unlimited */
  max_results: number;
  bookmarks_dock: BookmarkDock;
  file_sort_key?: FileSortKey;
  file_sort_direction?: FileSortDirection;
  file_display_fields?: FileDisplayField[];
  /** Persisted default agent for the chat pane (`SettingsModal`). The in-pane
   *  selector and header split-button dropdown may switch a session to a
   *  different backend transiently without touching this field. */
  chat_backend?: AgentBackend;
  /** Per-backend chat config defaults (model, thought level, mode). Written by
   *  the backend when a config option changes in the chat pane and applied to
   *  new sessions; the UI does not edit this directly. */
  chat_config?: ChatBackendConfig[];
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
  elapsed_ms: number;
  errors: string[];
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

export interface ChatReplayMessage {
  role: "user" | "assistant";
  text: string;
  thought: string;
  tools: ChatReplayToolCall[];
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
}

export interface ChatStartResult {
  session_id: string;
  conversation_id: string | null;
  backend_session_id: string | null;
  config_options: ChatConfigOption[];
  replay_messages: ChatReplayMessage[];
  context_files: ChatContextFileRecord[];
  active_doc: ChatActiveDocRecord | null;
}

export interface ChatSendResult {
  conversation_id: string | null;
}
