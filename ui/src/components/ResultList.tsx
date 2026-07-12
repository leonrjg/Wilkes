import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ArrowDown,
  ArrowUp,
  Calendar,
  ChevronDown,
  Clock,
  File,
  FileText,
  Folder,
  HardDrive,
  Hash,
  Info,
  RefreshCw,
  User,
} from "react-feather";
import { buildRows, COLLAPSED_LIMIT, type Row } from "../lib/utils/flattenResults";
import { useToasts } from "./Toast";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { Tooltip } from "./Tooltip";
import { useSearchStore } from "../stores/useSearchStore";
import { useChatStore } from "../stores/useChatStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { MetadataField } from "../lib/types";
import type {
  FileDisplayField,
  FileEntry,
  FileSortDirection,
  FileSortKey,
  Match,
  MatchRef,
  OmittedFileEntry,
  SourceOrigin,
} from "../lib/types";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { confirmDialog } from "../lib/utils/dialog";
import { DirectoryTree } from "./DirectoryTree";
import {
  formatDocumentFullDate,
  formatDocumentMonthYear,
  formatTimestampFullDate,
} from "../lib/dateFormatting";
import {
  DocumentEntryRow,
  fileName,
  type DetailIcon,
  type DocumentDetail,
} from "./DocumentEntryRow";

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

function dirName(path: string): string {
  return path.replace(/[/\\][^/\\]*$/, "");
}

function editableNameEnd(name: string): number {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? dot : name.length;
}

function validateNewFileName(name: string): string | null {
  if (!name) return "File name cannot be empty";
  if (name === "." || name === "..") return "Invalid file name";
  if (/[\\/]/.test(name)) return "File name cannot contain path separators";
  return null;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Optional document-metadata columns for the file list, offered in the merged
 * sort/visibility dropdown. To surface a new field: project it onto FileEntry
 * (backend + type), then add one entry here, the matching `FileDisplayField`
 * union member, and a `FileSortKey` entry if it should also be sortable.
 */
interface FileDisplayFieldDef {
  key: FileDisplayField;
  label: string;
  get: (entry: FileEntry) => string | null | undefined;
  fullWidth?: boolean;
  monospace?: boolean;
  hideWhenMissing?: boolean;
}

const FILE_DISPLAY_FIELDS: FileDisplayFieldDef[] = [
  { key: "title", label: "Title", get: (e) => e.title, fullWidth: true, monospace: false, hideWhenMissing: true },
  { key: "author", label: "Author", get: (e) => e.author, fullWidth: false, monospace: false, hideWhenMissing: true },
  { key: "created", label: "Created", get: (e) => (e.created_at_ms != null ? formatTimestampFullDate(e.created_at_ms) : null) },
  { key: "modified", label: "Modified", get: (e) => (e.modified_at_ms != null ? formatTimestampFullDate(e.modified_at_ms) : null) },
  { key: "publication", label: "Publication date", get: (e) => formatDocumentMonthYear(e.publication_date) },
  {
    key: "citations",
    label: "Citations",
    get: (e) =>
      e.citation_count != null
        ? e.citation_count.toLocaleString()
        : null,
  },
  { key: "size", label: "Size", get: (e) => formatSize(e.size_bytes) },
];

const FILE_DETAIL_ICONS: Record<FileDisplayField, DetailIcon> = {
  title: FileText,
  author: User,
  created: Calendar,
  modified: Clock,
  publication: Calendar,
  citations: Hash,
  size: HardDrive,
};

const SORT_KEYS: FileSortKey[] = ["filename", "title", "author", "created", "modified", "publication", "citations", "size"];
const SORT_KEY_LABELS: Record<FileSortKey, string> = {
  filename: "Name",
  title: "Title",
  author: "Author",
  created: "Created",
  modified: "Modified",
  publication: "Publication date",
  citations: "Citations",
  size: "Size",
};

function displayFieldValue(entry: FileEntry, field: FileDisplayField): string | null {
  const def = FILE_DISPLAY_FIELDS.find((f) => f.key === field);
  const value = def?.get(entry);
  return value && value.trim() !== "" ? value : null;
}

function displayFieldTitle(entry: FileEntry, field: FileDisplayField): string | undefined {
  if (field === "title") return normalizedDetailValue(entry.title) ?? undefined;
  if (field === "author") return normalizedDetailValue(entry.author) ?? undefined;
  if (field !== "publication") return undefined;
  return formatDocumentFullDate(entry.publication_date) ?? undefined;
}

const DISPLAY_FIELD_METADATA_KEYS: Partial<Record<FileDisplayField, string>> = {
  title: MetadataField.Title,
  author: MetadataField.Author,
  publication: MetadataField.PublicationDate,
  citations: MetadataField.CitationCount,
};

const METADATA_SOURCE_LABELS: Record<string, string> = {
  file: "File",
  zotero: "Zotero",
  semantic_scholar: "Semantic Scholar",
  openalex: "OpenAlex",
};

interface MetadataConflictGroup {
  value: string;
  sources: string[];
  selected: boolean;
}

function groupMetadataConflictValues(
  values: Array<{ source: string; value: string }>,
  displayedValue: string,
): MetadataConflictGroup[] {
  const groups = new Map<string, MetadataConflictGroup>();
  for (const item of values) {
    const value = String(item.value);
    const sourceLabel = METADATA_SOURCE_LABELS[item.source] ?? item.source;
    const group = groups.get(value);
    if (group) {
      group.sources.push(sourceLabel);
    } else {
      groups.set(value, {
        value,
        sources: [sourceLabel],
        selected: value === displayedValue,
      });
    }
  }
  return Array.from(groups.values());
}

function metadataConflictTooltip(
  entry: FileEntry,
  field: FileDisplayField,
  displayedValue: string,
): React.ReactNode | undefined {
  const metadataKey = DISPLAY_FIELD_METADATA_KEYS[field];
  const values = metadataKey ? entry.metadata_conflicts?.[metadataKey] : undefined;
  if (!values || values.length < 2) return undefined;
  const groupedValues = groupMetadataConflictValues(values, displayedValue);

  return (
    <div className="flex min-w-[220px] flex-col gap-1.5">
      <div className="font-semibold text-[var(--text-main)]">Sources</div>
      <div className="mt-0.5 flex flex-col gap-1">
        {groupedValues.map((group) => (
          <div key={group.value} className="grid grid-cols-[minmax(7rem,0.45fr)_minmax(0,1fr)] gap-x-2 gap-y-0.5">
            <span
              className={`min-w-0 break-words ${
                group.selected ? "text-[var(--accent-blue)]" : "text-[var(--text-dim)]"
              }`}
            >
              {group.sources.join(", ")}:{" "}
            </span>
            <span className="min-w-0 break-words text-[var(--text-main)]">
              {group.value}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function normalizedDetailValue(value: string | null | undefined): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

function compareFileNames(a: string, b: string): number {
  return fileName(a).localeCompare(fileName(b), undefined, {
    numeric: true,
    sensitivity: "base",
  });
}

function compareOptionalNumber(a: number | null | undefined, b: number | null | undefined): number {
  const aMissing = a == null;
  const bMissing = b == null;
  if (aMissing && bMissing) return 0;
  if (aMissing) return 1;
  if (bMissing) return -1;
  return a - b;
}

function isBlank(value: string | null | undefined): boolean {
  return value == null || value === "";
}

function compareOptionalString(a: string | null | undefined, b: string | null | undefined): number {
  const aMissing = isBlank(a);
  const bMissing = isBlank(b);
  if (aMissing && bMissing) return 0;
  if (aMissing) return 1;
  if (bMissing) return -1;
  return (a as string).localeCompare(b as string);
}

function sortFileEntries<T extends FileEntry>(
  entries: T[],
  key: FileSortKey,
  direction: FileSortDirection,
): T[] {
  return [...entries].sort((a, b) => {
    let result = 0;
    if (key === "filename") {
      result = compareFileNames(a.path, b.path);
      if (direction === "desc") result *= -1;
    } else if (key === "size") {
      result = a.size_bytes - b.size_bytes;
      if (direction === "desc") result *= -1;
    } else if (key === "title" || key === "author" || key === "publication") {
      const left = key === "title" ? a.title : key === "author" ? a.author : a.publication_date;
      const right = key === "title" ? b.title : key === "author" ? b.author : b.publication_date;
      result = compareOptionalString(left, right);
      // Keep files with no value last regardless of direction.
      if (direction === "desc" && !isBlank(left) && !isBlank(right)) {
        result *= -1;
      }
    } else if (key === "citations") {
      result = compareOptionalNumber(
        a.citation_count,
        b.citation_count,
      );
      if (
        direction === "desc" &&
        a.citation_count != null &&
        b.citation_count != null
      ) {
        result *= -1;
      }
    } else {
      result = compareOptionalNumber(
        key === "created" ? a.created_at_ms : a.modified_at_ms,
        key === "created" ? b.created_at_ms : b.modified_at_ms,
      );
      if (
        direction === "desc" &&
        (key === "created" ? a.created_at_ms : a.modified_at_ms) != null &&
        (key === "created" ? b.created_at_ms : b.modified_at_ms) != null
      ) {
        result *= -1;
      }
    }

    return result || compareFileNames(a.path, b.path) || a.path.localeCompare(b.path);
  });
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
  documents?: FileEntry[];
  preserveDocumentOrder?: boolean;
  documentDetails?: (entry: FileEntry) => DocumentDetail[];
}

export default function ResultList({
  onMatchClick,
  onFileClick,
  documents,
  preserveDocumentOrder = false,
  documentDetails,
}: Props) {
  const results = useSearchStore((s) => s.results);
  const stats = useSearchStore((s) => s.stats);
  const searching = useSearchStore((s) => s.searching);
  const storeHasQuery = useSearchStore((s) => s.hasQuery);
  const hasQuery = documents ? false : storeHasQuery;
  const selectedMatch = useSearchStore((s) => s.selectedMatch);
  const replaySearch = useSearchStore((s) => s.replaySearch);
  const clearPreview = useSearchStore((s) => s.clearPreview);
  const { addToast } = useToasts();

  const fileList = useSettingsStore((s) => s.fileList);
  const omittedFileList = useSettingsStore((s) => s.omittedFileList);
  const filterText = useSettingsStore((s) => s.filterText);
  const setFilterText = useSettingsStore((s) => s.setFilterText);
  const indexing = useSettingsStore((s) => s.indexing);
  const settings = useSettingsStore((s) => s.settings);
  const refreshFileList = useSettingsStore((s) => s.refreshFileList);
  const fileSortKey = useSettingsStore((s) => s.fileSortKey);
  const fileSortDirection = useSettingsStore((s) => s.fileSortDirection);
  const setFileSortKey = useSettingsStore((s) => s.setFileSortKey);
  const setFileSortDirection = useSettingsStore((s) => s.setFileSortDirection);
  const fileDisplayFields = useSettingsStore((s) => s.fileDisplayFields);
  const toggleFileDisplayField = useSettingsStore((s) => s.toggleFileDisplayField);
  const directory = useSettingsStore((s) => s.directory);
  const favorites = useSettingsStore((s) => s.favorites);
  const recentDirs = useSettingsStore((s) => s.recentDirs);
  const { menu, openMenu, closeMenu } = useContextMenu<ContextMenuTarget>();

  const parentRef = useRef<HTMLDivElement>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);
  const sortMenuTriggerRef = useRef<HTMLButtonElement>(null);
  const [sortMenuOpen, setSortMenuOpen] = useState(false);
  const [expandedFiles, setExpandedFiles] = useState<Set<number>>(new Set());
  const [showOmittedFiles, setShowOmittedFiles] = useState(false);
  const [renameTarget, setRenameTarget] = useState<{ path: string; name: string } | null>(null);
  const [moveTarget, setMoveTarget] = useState<{
    path: string;
    root: string;
    roots: string[];
  } | null>(null);

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

  useEffect(() => {
    if (!renameTarget) return;
    requestAnimationFrame(() => {
      const input = renameInputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(0, editableNameEnd(renameTarget.name));
    });
  }, [renameTarget?.path]);

  const displayedFileList = documents ?? fileList;
  const displayedOmittedFileList = documents ? [] : omittedFileList;
  const sortedFileList = preserveDocumentOrder
    ? displayedFileList
    : sortFileEntries(displayedFileList, fileSortKey, fileSortDirection);
  const sortedOmittedFileList = sortFileEntries(displayedOmittedFileList, fileSortKey, fileSortDirection);
  const rows = buildRows(results, expandedFiles);
  const onToast = (message: string, type: "success" | "error") => addToast(message, { type });

  const handleRowContextMenu = (
    event: React.MouseEvent,
    target: ContextMenuTarget,
  ) => {
    const otherRoots = Array.from(new Set([...favorites, ...recentDirs, directory])).filter(
      (root) => root && root !== dirName(target.path),
    );
    openMenu({
      event,
      target,
      items: buildFileContextMenuItems({
        target,
        api,
        capabilities: { canOpenInFileManager: isTauri },
        settings,
        onToast,
        onRenameRequest: (path) => setRenameTarget({ path, name: fileName(path) }),
        availableRoots: otherRoots,
        onMoveRequest: (path) =>
          setMoveTarget({ path, root: otherRoots[0] ?? "", roots: otherRoots }),
        deletionKind: source.deletionKind,
        onDeleteRequest: handleDeleteRequest,
      }),
    });
  };

  const handleDeleteRequest = async (path: string) => {
    const name = fileName(path);
    const isTrash = source.deletionKind === "trash";
    const confirmed = await confirmDialog(
      isTrash
        ? `Move "${name}" to Trash? You can restore it from Trash.`
        : `Permanently delete "${name}"? This cannot be undone.`,
    );
    if (!confirmed) return;

    try {
      await source.deleteFile(path);
      if (selectedMatch?.path === path) clearPreview();
      useChatStore.getState().removeContext(path);
      if (hasQuery) {
        await replaySearch();
      } else {
        refreshFileList();
      }
      onToast(
        isTrash ? `Moved "${name}" to Trash` : `Permanently deleted "${name}"`,
        "success",
      );
    } catch (error) {
      console.error("Failed to delete file:", error);
      onToast(error instanceof Error ? error.message : "Failed to delete file", "error");
    }
  };

  const handleRenameSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!renameTarget) return;

    const oldPath = renameTarget.path;
    const oldName = fileName(oldPath);
    const nextName = renameTarget.name.trim();
    if (nextName === oldName) {
      setRenameTarget(null);
      return;
    }

    const validationError = validateNewFileName(nextName);
    if (validationError) {
      onToast(validationError, "error");
      return;
    }

    try {
      await api.renameFile(oldPath, nextName);
      if (selectedMatch?.path === oldPath) {
        clearPreview();
      }
      if (hasQuery) {
        await replaySearch();
      } else {
        refreshFileList();
      }
      setRenameTarget(null);
      onToast("File renamed", "success");
    } catch (error) {
      console.error("Failed to rename file:", error);
      onToast("Failed to rename file", "error");
    }
  };

  const handleMoveSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!moveTarget || !moveTarget.root) return;

    const oldPath = moveTarget.path;
    try {
      await (source as DesktopSourceApi).moveFile(oldPath, moveTarget.root);
      if (selectedMatch?.path === oldPath) {
        clearPreview();
      }
      if (hasQuery) {
        await replaySearch();
      } else {
        refreshFileList();
      }
      setMoveTarget(null);
      onToast("File moved", "success");
    } catch (error) {
      console.error("Failed to move file:", error);
      onToast(error instanceof Error ? error.message : "Failed to move file", "error");
    }
  };

  const renameDialog = renameTarget && (
    <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/35 px-4">
      <form
        role="dialog"
        aria-modal="true"
        aria-labelledby="rename-file-title"
        onSubmit={handleRenameSubmit}
        className="w-full max-w-sm rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-3 shadow-2xl"
      >
        <div id="rename-file-title" className="mb-2 text-sm font-semibold text-[var(--text-main)]">
          Rename file
        </div>
        <input
          ref={renameInputRef}
          aria-label="File name"
          value={renameTarget.name}
          onChange={(event) =>
            setRenameTarget((target) =>
              target ? { ...target, name: event.target.value } : target,
            )
          }
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              setRenameTarget(null);
            }
          }}
          className="mb-3 h-8 w-full rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-2 text-sm text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
        />
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={() => setRenameTarget(null)}
            className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)]"
          >
            Cancel
          </button>
          <button
            type="submit"
            className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Rename
          </button>
        </div>
      </form>
    </div>
  );

  const moveDialog = moveTarget && (
    <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/35 px-4">
      <form
        role="dialog"
        aria-modal="true"
        aria-labelledby="move-file-title"
        onSubmit={handleMoveSubmit}
        className="w-full max-w-md rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-3 shadow-2xl"
      >
        <div id="move-file-title" className="mb-2 text-sm font-semibold text-[var(--text-main)]">
          Move "{fileName(moveTarget.path)}" to...
        </div>
        <DirectoryTree
          roots={moveTarget.roots}
          selected={moveTarget.root}
          onSelect={(root) =>
            setMoveTarget((target) => (target ? { ...target, root } : target))
          }
          loadChildren={(path) => (source as DesktopSourceApi).listDirectories(path)}
        />
        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={() => setMoveTarget(null)}
            className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)]"
          >
            Cancel
          </button>
          <button
            type="submit"
            className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90"
          >
            Move
          </button>
        </div>
      </form>
    </div>
  );

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
    const matchesFilter = (entry: FileEntry) => {
      if (!filterText) return true;
      const search = filterText.toLowerCase();
      return [entry.path, entry.title, entry.author]
        .some((value) => value?.toLowerCase().includes(search));
    };
    const filteredVisibleFiles = sortedFileList.filter(matchesFilter);
    const filteredOmittedFiles = sortedOmittedFileList.filter((entry) => matchesFilter(entry));

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
        <div className="px-2 py-1.5 text-xs text-[var(--text-muted)] border-b border-[var(--border-main)] flex-shrink-0 flex items-center gap-1">
          <Tooltip
            content={
              indexing
                ? "Indexing files"
                : `${sortedFileList.length} file${sortedFileList.length === 1 ? "" : "s"}`
            }
          >
            <div
              className="flex flex-shrink-0 items-center gap-1 whitespace-nowrap"
              aria-label={
                indexing
                  ? "Indexing files"
                  : `${sortedFileList.length} file${sortedFileList.length === 1 ? "" : "s"}`
              }
            >
              {indexing ? (
                "Indexing..."
              ) : (
                <>
                  <File size={12} aria-hidden="true" />
                  <span className="tabular-nums">{sortedFileList.length}</span>
                </>
              )}
            </div>
          </Tooltip>
          <input
            type="text"
            placeholder="Filter files..."
            value={filterText}
            onChange={(e) => setFilterText(e.target.value)}
            className="flex-1 min-w-0 bg-transparent border-none outline-none text-[11px] text-[var(--text-main)] placeholder-[var(--text-dim)]"
          />
          {!preserveDocumentOrder && (
            <>
              <button
                ref={sortMenuTriggerRef}
                type="button"
                aria-label="Sort and column visibility"
                aria-haspopup="menu"
                aria-expanded={sortMenuOpen}
                onClick={() => setSortMenuOpen((open) => !open)}
                className="flex h-6 flex-shrink-0 items-center gap-0.5 rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-1 text-[11px] text-[var(--text-main)] outline-none hover:bg-[var(--bg-hover)]"
              >
                <span className="max-w-[68px] truncate">{SORT_KEY_LABELS[fileSortKey]}</span>
                <ChevronDown size={10} aria-hidden="true" />
              </button>
              <SortVisibilityMenu
                anchorRef={sortMenuTriggerRef}
                open={sortMenuOpen}
                onClose={() => setSortMenuOpen(false)}
                sortKey={fileSortKey}
                onSortKeyChange={setFileSortKey}
                displayFields={fileDisplayFields}
                onToggleDisplayField={toggleFileDisplayField}
              />
              <Tooltip content={`Sort ${fileSortDirection === "asc" ? "ascending" : "descending"}`}>
                <button
                  type="button"
                  aria-label="Toggle file sort direction"
                  onClick={() => setFileSortDirection(fileSortDirection === "asc" ? "desc" : "asc")}
                  className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
                >
                  {fileSortDirection === "asc" ? (
                    <ArrowUp size={12} aria-hidden="true" />
                  ) : (
                    <ArrowDown size={12} aria-hidden="true" />
                  )}
                </button>
              </Tooltip>
            </>
          )}
          <Tooltip content="Refresh metadata (re-derive titles, publication dates, Zotero)">
            <button
              type="button"
              aria-label="Refresh file metadata"
              onClick={() => {
                void api
                  .refreshFileMetadata()
                  .catch(() => {})
                  .finally(() => useSettingsStore.getState().refreshFileList());
              }}
              className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
            >
              <RefreshCw size={12} aria-hidden="true" />
            </button>
          </Tooltip>
        </div>
        <div className="flex-1 overflow-y-auto">
          {filteredVisibleFiles.map((entry) => (
            <FileEntryRowAdapter
              key={entry.path}
              entry={entry}
              leadingDetails={documentDetails?.(entry) ?? []}
              displayFields={fileDisplayFields}
              selected={selectedMatch?.path === entry.path}
              onClick={() => onFileClick(entry.path)}
              onContextMenu={(event) =>
                handleRowContextMenu(event, {
                  kind: "file",
                  path: entry.path,
                  open: () => onFileClick(entry.path),
                })}
            />
          ))}
          {filteredVisibleFiles.length === 0 && sortedFileList.length > 0 && (
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
                    <FileEntryRowAdapter
                      key={entry.path}
                      entry={entry}
                      displayFields={[]}
                      selected={selectedMatch?.path === entry.path}
                      detail={formatOmittedReason(entry)}
                      muted
                      onClick={() => onFileClick(entry.path)}
                      onContextMenu={(event) =>
                        handleRowContextMenu(event, {
                          kind: "file",
                          path: entry.path,
                          open: () => onFileClick(entry.path),
                        })}
                    />
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
        <ContextMenu menu={menu} onClose={closeMenu} />
        {renameDialog}
        {moveDialog}
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
          <Tooltip content={<span className="whitespace-pre-line">{stats.errors.join("\n")}</span>}>
            <span className="text-red-500 font-medium">
              {stats.errors.length} file{stats.errors.length === 1 ? "" : "s"} failed (hover for details)
            </span>
          </Tooltip>
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
                      onContextMenu={(event) =>
                        handleRowContextMenu(event, {
                          kind: "file",
                          path: row.path,
                          open: () => onFileClick(row.path),
                        })}
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
                      onContextMenu={(event) =>
                        handleRowContextMenu(event, {
                          kind: "match",
                          path: row.path,
                          open: () =>
                            onMatchClick({
                              path: row.path,
                              origin: row.match.origin,
                              text_range: row.match.text_range ?? undefined,
                            }),
                        })}
                    />
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>
      </div>
      <ContextMenu menu={menu} onClose={closeMenu} />
      {renameDialog}
      {moveDialog}
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

function FileHeader({
  path,
  count,
  onClick,
  onContextMenu,
}: {
  path: string;
  count: number;
  onClick: () => void;
  onContextMenu: (event: React.MouseEvent) => void;
}) {
  return (
    <div
      className="flex select-none items-center gap-2 px-3 py-2 bg-[var(--bg-sidebar)] border-y border-[var(--border-main)] cursor-pointer hover:bg-[var(--bg-hover)] transition-colors"
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      <span className="text-xs font-semibold text-[var(--text-main)] truncate">{fileName(path)}</span>
      <span className="text-[10px] text-[var(--text-muted)] bg-[var(--bg-active)] px-1.5 py-0.5 rounded-full">
        {count}
      </span>
      <Tooltip content={path} className="font-mono break-all">
        <span
          className="flex h-5 w-5 flex-shrink-0 items-center justify-center text-[var(--text-dim)]"
          aria-label={`Path: ${path}`}
        >
          <Folder size={12} aria-hidden="true" />
        </span>
      </Tooltip>
    </div>
  );
}

function ExpandStrip({ remaining, onExpand }: { remaining: number; onExpand: () => void }) {
  return (
    <button
      onClick={onExpand}
      className="w-full select-none py-1 text-[10px] text-[var(--accent-blue)] hover:bg-[var(--accent-blue-muted)] transition-colors border-b border-[var(--border-main)]"
    >
      Show {remaining} more matches...
    </button>
  );
}

function SortVisibilityMenu({
  anchorRef,
  open,
  onClose,
  sortKey,
  onSortKeyChange,
  displayFields,
  onToggleDisplayField,
}: {
  anchorRef: React.RefObject<HTMLButtonElement | null>;
  open: boolean;
  onClose: () => void;
  sortKey: FileSortKey;
  onSortKeyChange: (key: FileSortKey) => void;
  displayFields: FileDisplayField[];
  onToggleDisplayField: (field: FileDisplayField) => void;
}) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ x: number; y: number } | null>(null);

  useEffect(() => {
    if (!open) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!menuRef.current?.contains(target) && !anchorRef.current?.contains(target)) {
        onClose();
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };

    document.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [open, onClose, anchorRef]);

  useEffect(() => {
    if (!open) setPosition(null);
  }, [open]);

  useLayoutEffect(() => {
    if (!open || !anchorRef.current || !menuRef.current) return;

    const anchorRect = anchorRef.current.getBoundingClientRect();
    const menuRect = menuRef.current.getBoundingClientRect();
    const margin = 8;
    const x = Math.min(
      Math.max(anchorRect.right - menuRect.width, margin),
      window.innerWidth - menuRect.width - margin,
    );
    const y = Math.min(anchorRect.bottom + 4, window.innerHeight - menuRect.height - margin);

    setPosition({ x, y });
  }, [open, anchorRef]);

  if (!open) return null;

  return createPortal(
    <div
      ref={menuRef}
      role="menu"
      aria-label="Sort and column visibility"
      className="fixed z-[150] w-48 rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-1 shadow-2xl"
      style={{
        left: `${position?.x ?? 0}px`,
        top: `${position?.y ?? 0}px`,
        visibility: position ? "visible" : "hidden",
      }}
    >
      {SORT_KEYS.map((key) => {
        const isFilename = key === "filename";
        const checked = isFilename || displayFields.includes(key as FileDisplayField);
        return (
          <div
            key={key}
            role="menuitemradio"
            aria-checked={sortKey === key}
            onClick={() => onSortKeyChange(key)}
            className={`flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-xs hover:bg-[var(--bg-hover)] ${
              sortKey === key ? "text-[var(--accent-blue)]" : "text-[var(--text-main)]"
            }`}
          >
            <input
              type="checkbox"
              aria-label={`Show ${SORT_KEY_LABELS[key]} column`}
              checked={checked}
              disabled={isFilename}
              onClick={(event) => event.stopPropagation()}
              onChange={() => onToggleDisplayField(key as FileDisplayField)}
              className="h-3 w-3 flex-shrink-0 rounded border-[var(--border-strong)] disabled:opacity-50"
            />
            <span className="flex-1 truncate">{SORT_KEY_LABELS[key]}</span>
          </div>
        );
      })}
    </div>,
    document.body,
  );
}

function FileEntryRowAdapter({
  entry,
  leadingDetails = [],
  displayFields,
  selected,
  detail,
  muted = false,
  onClick,
  onContextMenu,
}: {
  entry: FileEntry;
  leadingDetails?: DocumentDetail[];
  displayFields: FileDisplayField[];
  selected: boolean;
  detail?: string;
  muted?: boolean;
  onClick: () => void;
  onContextMenu: (event: React.MouseEvent) => void;
}) {
  const details: DocumentDetail[] = [
    ...leadingDetails,
    ...(detail ? [{ key: "detail", label: "Detail", value: detail, icon: Info }] : []),
    ...FILE_DISPLAY_FIELDS.filter((f) => displayFields.includes(f.key)).map((field) => {
      const value = displayFieldValue(entry, field.key) ?? "—";
      return {
        key: field.key,
        label: field.label,
        value,
        valueTitle: displayFieldTitle(entry, field.key),
        icon: FILE_DETAIL_ICONS[field.key],
        fullWidth: field.fullWidth,
        monospace: field.monospace ?? true,
        hideWhenMissing: field.hideWhenMissing ?? false,
        conflictTooltip: metadataConflictTooltip(entry, field.key, value),
      };
    }),
  ];

  return (
    <DocumentEntryRow
      entry={entry}
      details={details}
      selected={selected}
      muted={muted}
      onClick={onClick}
      onContextMenu={onContextMenu}
    />
  );
}

function MatchRow({
  match,
  path: _path,
  selected,
  onClick,
  onContextMenu,
}: {
  match: Match;
  path: string;
  selected: boolean;
  onClick: () => void;
  onContextMenu: (event: React.MouseEvent) => void;
}) {
  return (
    <button
      onClick={onClick}
      onContextMenu={onContextMenu}
      className={`w-full flex select-none items-start gap-2 px-3 py-1 text-left hover:bg-[var(--bg-hover)] transition-colors ${
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
