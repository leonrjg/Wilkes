// Auto-generated from Rust types (manually maintained until tauri-specta is wired up).
// Keep in sync with crates/core/src/types.rs.

export interface ByteRange {
  start: number;
  end: number;
}

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
}

export type FileType = "PlainText" | "Pdf";

export type SourceOrigin =
  | { TextFile: { line: number; col: number } }
  | { PdfPage: { page: number; bbox: BoundingBox | null } };

export interface Match {
  text_range: ByteRange;
  matched_text: string;
  context_before: string;
  context_after: string;
  origin: SourceOrigin;
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

export interface Settings {
  bookmarked_dirs: string[];
  last_directory: string | null;
  respect_gitignore: boolean;
  max_file_size: number;
  context_lines: number;
  theme: Theme;
}

export type Theme = "System" | "Light" | "Dark";

export interface SearchStats {
  files_scanned: number;
  total_matches: number;
  elapsed_ms: number;
  errors: string[];
}
