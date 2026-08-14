import type { FileMatches, Match, SearchFieldMatch } from "../types";

export type Row =
  | { kind: "file"; fileMatches: FileMatches; fileIndex: number; path: string }
  | { kind: "field_match"; fieldMatch: SearchFieldMatch; path: string; fileIndex: number }
  | { kind: "match"; match: Match; path: string; matchIndex: number; fileIndex: number }
  | { kind: "expand"; fileIndex: number; totalMatches: number };

export const COLLAPSED_LIMIT = 5;

export function buildRows(results: FileMatches[], expandedFiles: Set<number>): Row[] {
  const rows: Row[] = [];
  for (let fi = 0; fi < results.length; fi++) {
    const fm = results[fi];
    rows.push({ kind: "file", fileMatches: fm, fileIndex: fi, path: fm.path });
    const isExpanded = expandedFiles.has(fi);
    const fieldMatches = fm.field_matches ?? [];
    const totalMatches = fieldMatches.length + fm.matches.length;
    const limit = isExpanded ? totalMatches : COLLAPSED_LIMIT;

    for (const fieldMatch of fieldMatches.slice(0, limit)) {
      rows.push({ kind: "field_match", fieldMatch, path: fm.path, fileIndex: fi });
    }

    const contentLimit = Math.max(0, limit - fieldMatches.length);
    for (let mi = 0; mi < Math.min(fm.matches.length, contentLimit); mi++) {
      rows.push({
        kind: "match",
        match: fm.matches[mi],
        path: fm.path,
        matchIndex: mi,
        fileIndex: fi,
      });
    }
    if (!isExpanded && totalMatches > COLLAPSED_LIMIT) {
      rows.push({ kind: "expand", fileIndex: fi, totalMatches });
    }
  }
  return rows;
}
