import React, { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { buildRows, COLLAPSED_LIMIT, type Row } from "../lib/utils/flattenResults";
import { useToasts } from "./Toast";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import type { Match, MatchRef, SourceOrigin, FileEntry, OmittedFileEntry } from "../lib/types";

function originLabel(origin: SourceOrigin): string {
  if ("TextFile" in origin) return `L${origin.TextFile.line}`;
  if ("PdfPage" in origin) return `p.${origin.PdfPage.page}`;
  return "";
}

function highlightMatch(contextBefore: string, matchedText: string, contextAfter: string): React.ReactNode {
  if (!contextBefore && !contextAfter) {
    return <span className="text-[var(--text-muted)]">{matchedText}</span>;
  }
  return (
    <>
      <span className="text-[var(--text-muted)]">{contextBefore}</span>
      <mark className="match-highlight text-[var(--text-main)] bg-transparent">{matchedText}</mark>
      <span className="text-[var(--text-muted)]">{contextAfter}</span>
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

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function isSelected(row: Row, selectedMatch: MatchRef | null): boolean {
  if (!selectedMatch || row.kind !== "match") return false;
  if (row.path !== selectedMatch.path) return false;
  return (
    JSON.stringify(row.match.origin) === JSON.stringify(selectedMatch.origin) &&
    JSON.stringify(row.match.text_range) === JSON.stringify(selectedMatch.text_range)
  );
}

interface Props {
  onMatchClick: (ref: MatchRef) => void;
  onFileClick: (path: string) => void;
}

export default function ResultList({ onMatchClick, onFileClick }: Props) {
  const results = useSearchStore((s) => s.results);
  const stats = useSearchStore((s) => s.stats);
  const searching = useSearchStore((s) => s.searching);
  const hasQuery = useSearchStore((s) => s.hasQuery);
  const selectedMatch = useSearchStore((s) => s.selectedMatch);
  const { addToast } = useToasts();

  const excluded = useSettingsStore((s) => s.excluded);
  const fileList = useSettingsStore((s) => s.fileList);
  const omittedFileList = useSettingsStore((s) => s.omittedFileList);
  const filterText = useSettingsStore((s) => s.filterText);
  const setFilterText = useSettingsStore((s) => s.setFilterText);
  const indexing = useSettingsStore((s) => s.indexing);

  const parentRef = useRef<HTMLDivElement>(null);
  const [expandedFiles, setExpandedFiles] = useState<Set<number>>(new Set());
  const [showOmittedFiles, setShowOmittedFiles] = useState(false);

  useEffect(() => {
    if (results.length === 0) setExpandedFiles(new Set());
  }, [results.length]);

  useEffect(() => {
    if (omittedFileList.length === 0) setShowOmittedFiles(false);
  }, [omittedFileList.length]);

  useEffect(() => {
    if (!stats || stats.errors.length === 0) return;
    addToast(stats.errors[0], { type: "error" });
  }, [addToast, stats]);

  const filteredFileList = fileList.filter((f) => !excluded.has(f.extension));
  const rows = buildRows(results, expandedFiles);

  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (index) => (rows[index].kind === "file" ? 40 : 28),
    overscan: 20,
  });

  const expandFile = (fileIndex: number) => {
    setExpandedFiles((prev) => {
      const next = new Set(prev);
      next.add(fileIndex);
      return next;
    });
  };

  const totalCount = results.reduce((n, fm) => n + fm.matches.length, 0);

  if (!hasQuery) {
    const omittedFiles = omittedFileList.filter((f) => !excluded.has(f.extension));
    const matchesFilter = (entry: FileEntry) => {
      if (!filterText) return true;
      const search = filterText.toLowerCase();
      return entry.path.toLowerCase().includes(search);
    };
    const filteredVisibleFiles = filteredFileList.filter(matchesFilter);
    const filteredOmittedFiles = omittedFiles.filter((entry) => matchesFilter(entry));

    return (
      <div className="flex flex-col h-full overflow-hidden relative bg-[var(--bg-app)]">
        {indexing && (
          <div className="absolute inset-0 z-10 pointer-events-none overflow-hidden">
            <div className="absolute inset-0 bg-[var(--bg-app)] opacity-30" />
            <div
              className="absolute inset-y-0 w-1/2"
              style={{
                background: "linear-gradient(90deg, transparent, var(--shimmer-highlight), transparent)",
                animation: "shimmer 1.5s ease-in-out infinite",
              }}
            />
          </div>
        )}
        <div className="px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border-main)] flex-shrink-0 flex items-center gap-2">
          <div className="flex-shrink-0 whitespace-nowrap">
            {indexing ? "Indexing..." : `${filteredFileList.length} file${filteredFileList.length === 1 ? "" : "s"}`}
          </div>
          <span className="text-[var(--text-dim)]">/</span>
          <input
            type="text"
            placeholder="Filter files..."
            value={filterText}
            onChange={(e) => setFilterText(e.target.value)}
            className="flex-1 min-w-0 bg-transparent border-none outline-none text-[11px] text-[var(--text-main)] placeholder-[var(--text-dim)]"
          />
        </div>
        <div className="flex-1 overflow-y-auto">
          {filteredVisibleFiles.map((entry) => (
            <FileEntryRow
              key={entry.path}
              entry={entry}
              selected={selectedMatch?.path === entry.path}
              onClick={() => onFileClick(entry.path)}
            />
          ))}
          {filteredVisibleFiles.length === 0 && filteredFileList.length > 0 && (
            <div className="px-3 py-8 text-center text-xs text-[var(--text-dim)] italic">
              No files match "{filterText}"
            </div>
          )}
          {filteredOmittedFiles.length > 0 && (
            <div className="mt-2 px-3 text-[11px] text-[var(--text-dim)]">
              <button
                type="button"
                onClick={() => setShowOmittedFiles((shown) => !shown)}
                className="w-full flex items-center justify-between gap-3 text-left hover:text-[var(--text-muted)] transition-colors"
              >
                <span>
                  {filteredOmittedFiles.length} file{filteredOmittedFiles.length === 1 ? "" : "s"} omitted from this list
                </span>
                <span className="text-[10px] uppercase tracking-wider">
                  {showOmittedFiles ? "Hide" : "Show"}
                </span>
              </button>
              {showOmittedFiles && (
                <div className="mt-2">
                  {filteredOmittedFiles.map((entry) => (
                    <div key={entry.path} className="py-1.5">
                      <div className="truncate">{fileName(entry.path)}</div>
                      <div className="text-[10px] opacity-75 truncate">
                        {formatOmittedReason(entry)}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-hidden bg-[var(--bg-app)] relative">
      {(searching || indexing) && (
        <div className="absolute inset-0 z-10 pointer-events-none overflow-hidden">
          <div className="absolute inset-0 bg-[var(--bg-app)] opacity-30" />
          <div
            className="absolute inset-y-0 w-1/2"
            style={{
              background: "linear-gradient(90deg, transparent, var(--shimmer-highlight), transparent)",
              animation: "shimmer 1.5s ease-in-out infinite",
            }}
          />
        </div>
      )}
      <div className="px-3 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border-main)] flex-shrink-0 flex flex-col gap-0.5 bg-[var(--bg-header)]">
        <span>
          {searching
            ? `${totalCount} matches…`
            : indexing
              ? "Indexing files…"
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

      <div className="flex-1 overflow-hidden relative">
        <div ref={parentRef} className="h-full overflow-y-auto">
        {rows.length === 0 && !searching && (
          <div className="text-[var(--text-dim)] text-sm p-4 text-center">
            {stats ? "No results" : "Type to search"}
          </div>
        )}

        <div
          style={{
            height: `${rowVirtualizer.getTotalSize()}px`,
            width: "100%",
            position: "relative",
          }}
        >
          <div
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${rowVirtualizer.getVirtualItems()[0]?.start ?? 0}px)`,
            }}
          >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const row = rows[virtualRow.index];
              return (
                <div
                  key={virtualRow.key}
                  data-index={virtualRow.index}
                  ref={rowVirtualizer.measureElement}
                >
                  {row.kind === "file" ? (
                    <FileHeader
                      path={row.path}
                      count={row.fileMatches.matches.length}
                      onClick={() => onFileClick(row.path)}
                    />
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
                        onMatchClick({
                          path: row.path,
                          origin: row.match.origin,
                          text_range: row.match.text_range ?? undefined,
                        })
                      }
                    />
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>
      </div>
    </div>
  );
}

function formatOmittedReason(entry: OmittedFileEntry): string {
  if (entry.reason === "TooLarge") {
    return `${formatSize(entry.size_bytes)} exceeds current file size limit`;
  }
  return entry.extension
    ? `.${entry.extension} is not in the allowed extensions`
    : "File extension is not in the allowed extensions";
}

function FileHeader({ path, count, onClick }: { path: string; count: number; onClick: () => void }) {
  return (
    <div
      className="flex items-center gap-2 px-3 py-2 bg-[var(--bg-sidebar)] border-y border-[var(--border-main)] cursor-pointer hover:bg-[var(--bg-hover)] transition-colors"
      onClick={onClick}
    >
      <span className="text-xs font-semibold text-[var(--text-main)] truncate">{fileName(path)}</span>
      <span className="text-[10px] text-[var(--text-muted)] bg-[var(--bg-active)] px-1.5 py-0.5 rounded-full">
        {count}
      </span>
      <span className="text-[10px] text-[var(--text-dim)] truncate">{path}</span>
    </div>
  );
}

function ExpandStrip({ remaining, onExpand }: { remaining: number; onExpand: () => void }) {
  return (
    <button
      onClick={onExpand}
      className="w-full py-1 text-[10px] text-[var(--accent-blue)] hover:bg-[var(--accent-blue-muted)] transition-colors border-b border-[var(--border-main)]"
    >
      Show {remaining} more matches...
    </button>
  );
}

function FileEntryRow({ entry, selected, onClick }: { entry: FileEntry; selected: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`w-full flex items-baseline gap-2 px-3 py-1.5 text-left hover:bg-[var(--bg-hover)] transition-colors selectable ${
        selected ? "bg-[var(--bg-active)]" : ""
      }`}
    >
      <span className="text-sm font-medium text-[var(--text-main)] truncate">{fileName(entry.path)}</span>
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
        {highlightMatch(match.context_before, match.matched_text, match.context_after)}
      </span>
    </button>
  );
}
