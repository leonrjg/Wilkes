import type { FileMatches, Match } from "../types";

export type Row =
  | { kind: "file"; fileMatches: FileMatches; fileIndex: number; path: string }
  | { kind: "match"; match: Match; path: string; matchIndex: number; fileIndex: number }
  | { kind: "expand"; fileIndex: number; totalMatches: number };

export const COLLAPSED_LIMIT = 5;

export function buildRows(results: FileMatches[], expandedFiles: Set<number>): Row[] {
  const rows: Row[] = [];
  for (let fi = 0; fi < results.length; fi++) {
    const fm = results[fi];
    rows.push({ kind: "file", fileMatches: fm, fileIndex: fi, path: fm.path });
    const isExpanded = expandedFiles.has(fi);
    const limit = isExpanded ? fm.matches.length : COLLAPSED_LIMIT;

    for (let mi = 0; mi < Math.min(fm.matches.length, limit); mi++) {
      rows.push({
        kind: "match",
        match: fm.matches[mi],
        path: fm.path,
        matchIndex: mi,
        fileIndex: fi,
      });
    }
    if (!isExpanded && fm.matches.length > COLLAPSED_LIMIT) {
      rows.push({ kind: "expand", fileIndex: fi, totalMatches: fm.matches.length });
    }
  }
  return rows;
}
