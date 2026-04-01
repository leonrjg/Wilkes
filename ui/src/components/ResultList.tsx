import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { FileEntry, FileMatches, Match, MatchRef, SearchStats, SourceOrigin } from "../lib/types";

const COLLAPSED_LIMIT = 5;

interface Props {
  results: FileMatches[];
  stats: SearchStats | null;
  searching: boolean;
  hasQuery: boolean;
  fileList: FileEntry[];
  selectedMatch: MatchRef | null;
  onMatchClick: (ref: MatchRef) => void;
  onFileClick: (path: string) => void;
}

// Flatten the results tree into a list of rows for the virtualizer.
type Row =
  | { kind: "file"; fileMatches: FileMatches; fileIndex: number }
  | { kind: "match"; match: Match; path: string; matchIndex: number; fileIndex: number }
  | { kind: "expand"; fileIndex: number; totalMatches: number };

function buildRows(results: FileMatches[], expandedFiles: Set<number>): Row[] {
  const rows: Row[] = [];
  for (let fi = 0; fi < results.length; fi++) {
    const fm = results[fi];
    rows.push({ kind: "file", fileMatches: fm, fileIndex: fi });
    const isExpanded = expandedFiles.has(fi);
    const limit = isExpanded ? fm.matches.length : Math.min(COLLAPSED_LIMIT, fm.matches.length);
    for (let mi = 0; mi < limit; mi++) {
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

function originLabel(origin: SourceOrigin): string {
  if ("TextFile" in origin) return `L${origin.TextFile.line}`;
  if ("PdfPage" in origin) return `p.${origin.PdfPage.page}`;
  return "";
}

function highlightMatch(line: string, matchedText: string): React.ReactNode {
  const idx = line.indexOf(matchedText);
  if (idx === -1) return <span>{line}</span>;
  return (
    <>
      <span className="text-[var(--text-muted)]">{line.slice(0, idx)}</span>
      <mark className="match-highlight text-[var(--text-main)] bg-transparent">{matchedText}</mark>
      <span className="text-[var(--text-muted)]">{line.slice(idx + matchedText.length)}</span>
    </>
  );
}

function fileName(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

function dirName(path: string): string {
  const parts = path.split(/[/\\]/);
  parts.pop();
  return parts.join("/");
}

function isSelected(row: Row, selectedMatch: MatchRef | null): boolean {
  if (!selectedMatch || row.kind !== "match") return false;
  if (row.path !== selectedMatch.path) return false;
  const origin = row.match.origin;
  const sel = selectedMatch.origin;
  if ("TextFile" in origin && "TextFile" in sel) {
    return origin.TextFile.line === sel.TextFile.line;
  }
  if ("PdfPage" in origin && "PdfPage" in sel) {
    return origin.PdfPage.page === sel.PdfPage.page;
  }
  return false;
}

export default function ResultList({
  results,
  stats,
  searching,
  hasQuery,
  fileList,
  selectedMatch,
  onMatchClick,
  onFileClick,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null);
  const [expandedFiles, setExpandedFiles] = useState<Set<number>>(new Set());

  useEffect(() => {
    if (results.length === 0) setExpandedFiles(new Set());
  }, [results.length]);

  const rows = buildRows(results, expandedFiles);

  function expandFile(fileIndex: number) {
    setExpandedFiles((prev) => new Set([...prev, fileIndex]));
  }

  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (i) => (rows[i].kind === "file" ? 36 : rows[i].kind === "match" ? 60 : 28),
    overscan: 10,
  });

  const totalCount = results.reduce((n, fm) => n + fm.matches.length, 0);

  if (!hasQuery) {
    return (
      <div className="flex flex-col h-full overflow-hidden">
        <div className="px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border-main)] flex-shrink-0">
          {fileList.length} files
        </div>
        <div className="flex-1 overflow-y-auto">
          {fileList.map((entry) => (
            <FileEntryRow
              key={entry.path}
              entry={entry}
              selected={selectedMatch?.path === entry.path}
              onClick={() => onFileClick(entry.path)}
            />
          ))}
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden bg-[var(--bg-app)]">
      {/* Status bar */}
      <div className="px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border-main)] flex-shrink-0 flex flex-col gap-0.5 bg-[var(--bg-header)]">
        <span>
          {searching
            ? `${totalCount} matches…`
            : stats
              ? `${stats.total_matches} matches in ${stats.files_scanned} files (${stats.elapsed_ms}ms)`
              : "Ready"}
        </span>
        {stats && stats.errors.length > 0 && (
          <span className="text-red-500 font-medium" title={stats.errors.join("\n")}>
            {stats.errors.length} file{stats.errors.length === 1 ? "" : "s"} failed (hover for details)
          </span>
        )}
      </div>
    ...
      {/* Virtual list */}
      <div ref={parentRef} className="flex-1 overflow-y-auto">
        {rows.length === 0 && !searching && (
          <div className="text-[var(--text-dim)] text-sm p-4 text-center">
            {stats ? "No results" : "Type to search"}
          </div>
        )}


        <div
          style={{ height: `${rowVirtualizer.getTotalSize()}px`, position: "relative" }}
        >
          {rowVirtualizer.getVirtualItems().map((virtualRow) => {
            const row = rows[virtualRow.index];
            return (
              <div
                key={virtualRow.key}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${virtualRow.start}px)`,
                  height: `${virtualRow.size}px`,
                }}
              >
                {row.kind === "file" ? (
                  <FileHeader fm={row.fileMatches} />
                ) : row.kind === "expand" ? (
                  <ExpandStrip
                    remaining={row.totalMatches - COLLAPSED_LIMIT}
                    onExpand={() => expandFile(row.fileIndex)}
                  />
                ) : (
                  <MatchRow
                    match={row.match}
                    path={row.path}
                    selected={isSelected(row, selectedMatch)}
                    onClick={() =>
                      onMatchClick({ path: row.path, origin: row.match.origin })
                    }
                  />
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function FileHeader({ fm }: { fm: FileMatches }) {
  return (
    <div className="flex items-baseline gap-2 px-3 py-1.5 bg-[var(--bg-header)] border-b border-[var(--border-main)] selectable">
      <span className="text-sm font-medium text-[var(--text-main)] truncate">
        {fileName(fm.path)}
      </span>
      <span className="text-xs text-[var(--text-muted)] truncate flex-1">{dirName(fm.path)}</span>
      <span className="text-xs text-[var(--text-muted)] flex-shrink-0">
        {fm.matches.length} {fm.matches.length === 1 ? "match" : "matches"}
      </span>
    </div>
  );
}

function ExpandStrip({ remaining, onExpand }: { remaining: number; onExpand: () => void }) {
  return (
    <button
      onClick={onExpand}
      className="w-full flex items-center gap-2 px-3 py-1 text-left hover:bg-[var(--bg-hover)] transition-colors text-xs text-[var(--text-muted)] hover:text-[var(--text-main)]"
    >
      <span className="w-10 flex-shrink-0" />
      <span>
        Show {remaining} more {remaining === 1 ? "match" : "matches"}
      </span>
    </button>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function FileEntryRow({
  entry,
  selected,
  onClick,
}: {
  entry: FileEntry;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-baseline gap-2 px-3 py-1.5 text-left hover:bg-[var(--bg-hover)] transition-colors selectable ${
        selected ? "bg-[var(--bg-active)]" : ""
      }`}
    >
      <span className="text-sm font-medium text-[var(--text-main)] truncate">
        {fileName(entry.path)}
      </span>
      <span className="text-xs text-[var(--text-muted)] truncate flex-1">{dirName(entry.path)}</span>
      <span className="text-xs text-[var(--text-muted)] flex-shrink-0 font-mono">
        {entry.file_type === "Pdf" && <span className="text-[var(--accent-blue)] mr-1.5">PDF</span>}
        {formatSize(entry.size_bytes)}
      </span>
    </button>
  );
}

function MatchRow({
  match,
  path: _path,
  selected,
  onClick,
}: {
  match: Match;
  path: string;
  selected: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-start gap-2 px-3 py-1 text-left hover:bg-[var(--bg-hover)] transition-colors selectable ${
        selected ? "bg-[var(--bg-active)]" : ""
      }`}
    >
      <span className="text-xs text-[var(--accent-blue)] w-10 flex-shrink-0 font-mono text-right pt-px">
        {originLabel(match.origin)}
      </span>
      {match.score != null && (
        <span className="text-xs text-[var(--text-muted)] flex-shrink-0 font-mono pt-px">
          {(match.score * 100).toFixed(0)}%
        </span>
      )}
      <span className="text-xs line-clamp-3 flex-1 font-mono break-all">
        {highlightMatch(
          match.context_before + match.matched_text + match.context_after,
          match.matched_text,
        )}
      </span>
    </button>
  );
}
