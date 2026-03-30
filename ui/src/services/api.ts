import type {
  FileEntry,
  FileMatches,
  MatchRef,
  PreviewData,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";

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
  listFiles(root: string): Promise<FileEntry[]>;
  openFile(path: string): Promise<PreviewData>;
  resolvePdfUrl(path: string): string;
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
