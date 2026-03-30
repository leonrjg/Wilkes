import type {
  FileEntry,
  FileMatches,
  MatchRef,
  PreviewData,
  SearchQuery,
  SearchStats,
  Settings,
} from "../lib/types";

export interface SearchApi {
  /** Start a search. Returns a search_id. Call onResult for each FileMatches,
   *  onComplete when done. */
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

  pickDirectory(): Promise<string | null>;
}
