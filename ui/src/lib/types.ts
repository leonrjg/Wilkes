// Auto-generated from Rust types (manually maintained until tauri-specta is wired up).
// Keep in sync with crates/core/src/types.rs.

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
  file_type_filters: string[];
  /** 0 = unlimited */
  max_results: number;
  respect_gitignore: boolean;
  /** 0 = unlimited */
  max_file_size: number;
  context_lines: number;
  /** Defaults to "Grep" */
  mode: SearchMode;
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

export interface MatchRef {
  path: string;
  origin: SourceOrigin;
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

export interface FileEntry {
  path: string;
  size_bytes: number;
  file_type: FileType;
  extension: string;
}

/** HuggingFace model code, e.g. "BAAI/bge-base-en-v1.5". */
export type EmbedderModel = string;

export interface ModelDescriptor {
  model_id: string;
  display_name: string;
  description: string;
  dimension: number;
  is_cached: boolean;
  /** Total bytes of all model files. Null for uncached models until fetched. */
  size_bytes: number | null;
}

export interface SemanticSettings {
  enabled: boolean;
  model: EmbedderModel;
  index_path: string | null;
}

export interface Settings {
  bookmarked_dirs: string[];
  last_directory: string | null;
  respect_gitignore: boolean;
  max_file_size: number;
  context_lines: number;
  theme: Theme;
  semantic: SemanticSettings;
}

export type Theme = "System" | "Light" | "Dark";

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
  model_id: string;
  dimension: number;
}

export interface DownloadProgress {
  bytes_received: number;
  total_bytes: number;
  done: boolean;
}

export interface IndexBuildProgress {
  files_processed: number;
  total_files: number;
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
