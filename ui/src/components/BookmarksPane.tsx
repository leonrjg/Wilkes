import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Edit2,
  FileText,
  Layers,
  Sidebar,
  Trash2,
  X,
} from "react-feather";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { activeViewerTab, useViewerStore } from "../stores/useViewerStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { toMarkdown } from "../lib/utils/bookmarkMarkdown";
import { api } from "../services";
import { useToasts } from "./Toast";
import { Tooltip } from "./Tooltip";
import { CopyButton } from "./CopyButton";
import type {
  Bookmark,
  BookmarkClusterGranularity,
  BookmarkClustersResult,
} from "../lib/types";

type ThemeStatus = "idle" | "loading" | "ready";
type BookmarkRow =
  | { kind: "bookmark"; key: string; bookmark: Bookmark }
  | { kind: "theme"; key: string; label: string; count: number; expanded: boolean };

const GRANULARITY_VALUES: readonly BookmarkClusterGranularity[] = [
  "much_fewer",
  "fewer",
  "balanced",
  "more",
  "much_more",
];

const GRANULARITY_LABELS: Record<BookmarkClusterGranularity, string> = {
  much_fewer: "Much fewer",
  fewer: "Fewer",
  balanced: "Balanced",
  more: "More",
  much_more: "Much more",
};

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

function bookmarkPage(bookmark: Bookmark) {
  return "PdfPage" in bookmark.origin ? bookmark.origin.PdfPage.page : null;
}

/** Zotero returns citations as HTML fragments; render them to plain text for
 *  the clipboard (correct entity decoding, no CSL work on our side). */
function htmlToText(html: string): string {
  return new DOMParser().parseFromString(html, "text/html").body.textContent?.trim() ?? "";
}

function themeToggleTooltip(
  preferSemantic: boolean,
  semanticIndexReady: boolean,
  bookmarkCount: number,
  themesEnabled: boolean,
): string {
  if (!preferSemantic) return "Enable semantic search to group bookmarks";
  if (!semanticIndexReady) return "Build the semantic index to group bookmarks";
  if (bookmarkCount < 3) return "At least 3 bookmarks are needed";
  return themesEnabled ? "Show bookmarks as a list" : "Group bookmarks by theme";
}

export default function BookmarksPane() {
  const parentRef = useRef<HTMLDivElement>(null);
  const bookmarks = useBookmarksStore((s) => s.bookmarks);
  const filterText = useBookmarksStore((s) => s.filterText);
  const scope = useBookmarksStore((s) => s.scope);
  const setFilter = useBookmarksStore((s) => s.setFilter);
  const setScope = useBookmarksStore((s) => s.setScope);
  const remove = useBookmarksStore((s) => s.remove);
  const updateNote = useBookmarksStore((s) => s.updateNote);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftNote, setDraftNote] = useState("");
  const [themesEnabled, setThemesEnabled] = useState(false);
  const [themeStatus, setThemeStatus] = useState<ThemeStatus>("idle");
  const [themeResult, setThemeResult] = useState<BookmarkClustersResult | null>(null);
  const [themeResultInputKey, setThemeResultInputKey] = useState<string | null>(null);
  const [expandedThemes, setExpandedThemes] = useState<Set<string>>(() => new Set());
  const [granularity, setGranularity] =
    useState<BookmarkClusterGranularity>("balanced");
  const selectedPath = useViewerStore((state) => activeViewerTab(state)?.path ?? null);
  const openMatch = useViewerStore((state) => state.openMatch);
  const dock = useSettingsStore((s) => s.bookmarksDock);
  const setDock = useSettingsStore((s) => s.setBookmarksDock);
  const preferSemantic = useSettingsStore((s) => s.preferSemantic);
  const closePane = useBookmarksStore((s) => s.closePane);
  const semanticIndexReady = useSemanticStore((s) => s.readyForCurrentRoot);
  const semanticReady = preferSemantic && semanticIndexReady;
  const zoteroEnabled = useSettingsStore(
    (s) => s.settings?.integrations.zotero.enabled ?? false,
  );
  const { addToast, notifyPending } = useToasts();

  const copyCitation = async (bookmark: Bookmark) => {
    const dismissPending = notifyPending("Fetching citation…");
    let notified = false;
    try {
      const result = await api.zoteroGenerateCitation(bookmark.path);
      const citation = result.citation ? htmlToText(result.citation) : "";
      if (!citation) {
        addToast("Zotero returned no in-text citation for this file", { type: "error" });
        notified = true;
        throw new Error("Zotero returned no in-text citation for this file");
      }
      await api.writeClipboard(`"${bookmark.quote}" ${citation}`);
      addToast(
        result.low_confidence
          ? "Citation copied — from a low-confidence Zotero match"
          : "Citation copied",
        { type: result.low_confidence ? "warning" : "success" },
      );
    } catch (error) {
      console.error("Failed to get Zotero citation:", error);
      if (!notified) {
        addToast(
          error instanceof Error && error.message ? error.message : "No Zotero citation found",
          { type: "error" },
        );
      }
      throw error;
    } finally {
      dismissPending();
    }
  };

  const startEditingNote = (bookmark: Bookmark) => {
    setEditingId(bookmark.id);
    setDraftNote(bookmark.note ?? "");
  };

  const cancelEditingNote = () => {
    setEditingId(null);
    setDraftNote("");
  };

  const saveNote = async (id: string) => {
    const next = draftNote.trim();
    try {
      await updateNote(id, next.length > 0 ? next : null);
      setEditingId(null);
      setDraftNote("");
    } catch (error) {
      console.error("Failed to update bookmark note:", error);
      addToast("Failed to save note", { type: "error" });
    }
  };

  const toggleTheme = (key: string) => {
    setExpandedThemes((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  const updateGranularity = (next: BookmarkClusterGranularity) => {
    setGranularity(next);
    setExpandedThemes(new Set());
  };

  const scopedToCurrentFile = scope === "current";
  const scoped = useMemo(
    () =>
      bookmarks.filter(
        (bookmark) => scope !== "current" || bookmark.path === selectedPath,
      ),
    [bookmarks, scope, selectedPath],
  );
  const filtered = useMemo(() => {
    const query = filterText.trim().toLowerCase();
    return scoped.filter((bookmark) => {
      if (!query) return true;
      return (
        bookmark.quote.toLowerCase().includes(query) ||
        bookmark.path.toLowerCase().includes(query)
      );
    });
  }, [scoped, filterText]);
  const themesAvailable = semanticReady && scoped.length >= 3;
  const themeInputKey = useMemo(
    () =>
      JSON.stringify(
        scoped.map((bookmark) => [bookmark.id, bookmark.quote, bookmark.note ?? ""]),
      ),
    [scoped],
  );
  const displayedThemeResult =
    themeResultInputKey === themeInputKey ? themeResult : null;

  useEffect(() => {
    setExpandedThemes(new Set());
  }, [themeInputKey]);

  useEffect(() => {
    if (themesEnabled && !themesAvailable) {
      setThemesEnabled(false);
      setThemeStatus("idle");
      setThemeResult(null);
      setThemeResultInputKey(null);
    }
  }, [themesAvailable, themesEnabled]);

  useEffect(() => {
    if (!themesEnabled || !themesAvailable) {
      setThemeStatus("idle");
      setThemeResult(null);
      setThemeResultInputKey(null);
      setExpandedThemes(new Set());
      return;
    }

    let cancelled = false;
    setThemeStatus("loading");
    const timeout = window.setTimeout(() => {
      api.clusterBookmarks({
        bookmark_ids: scoped.map((bookmark) => bookmark.id),
        granularity,
      })
        .then((result) => {
          if (cancelled) return;
          setThemeResult(result);
          setThemeResultInputKey(themeInputKey);
          setThemeStatus("ready");
        })
        .catch((error) => {
          if (cancelled) return;
          console.error("Failed to group bookmarks by theme:", error);
          addToast("Failed to group bookmarks by theme", { type: "error" });
          setThemesEnabled(false);
          setThemeStatus("idle");
          setThemeResult(null);
          setThemeResultInputKey(null);
        });
    }, 150);

    return () => {
      cancelled = true;
      window.clearTimeout(timeout);
    };
  }, [granularity, themesAvailable, themesEnabled, themeInputKey]);

  const rows = useMemo<BookmarkRow[]>(() => {
    if (!themesEnabled || !displayedThemeResult) {
      return filtered.map((bookmark) => ({
        kind: "bookmark",
        key: bookmark.id,
        bookmark,
      }));
    }

    const bookmarksById = new Map(scoped.map((bookmark) => [bookmark.id, bookmark]));
    const visibleIds = new Set(filtered.map((bookmark) => bookmark.id));
    const assignedIds = new Set<string>();
    const groupedRows: BookmarkRow[] = [];
    const appendGroup = (
      key: string,
      label: string,
      ids: string[],
    ) => {
      const groupBookmarks = ids.flatMap((id) => {
        if (assignedIds.has(id)) return [];
        const bookmark = bookmarksById.get(id);
        if (!bookmark) return [];
        assignedIds.add(id);
        return visibleIds.has(id) ? [bookmark] : [];
      });
      if (groupBookmarks.length === 0) return;
      const themeKey = `theme:${key}`;
      const expanded = expandedThemes.has(themeKey);
      groupedRows.push({
        kind: "theme",
        key: themeKey,
        label,
        count: groupBookmarks.length,
        expanded,
      });
      if (expanded) {
        groupedRows.push(
          ...groupBookmarks.map((bookmark) => ({
            kind: "bookmark" as const,
            key: bookmark.id,
            bookmark,
          })),
        );
      }
    };

    for (const cluster of displayedThemeResult.clusters) {
      const representative = bookmarksById.get(cluster.representative_bookmark_id);
      const quote = representative?.quote.trim();
      appendGroup(
        cluster.representative_bookmark_id,
        quote ? `Around “${quote}”` : "Theme",
        cluster.bookmark_ids,
      );
    }

    const unclusteredIds = [
      ...displayedThemeResult.unclustered_bookmark_ids,
      ...scoped
        .map((bookmark) => bookmark.id)
        .filter((id) => !assignedIds.has(id)),
    ];
    appendGroup(
      "unclustered",
      displayedThemeResult.clusters.length > 0 ? "Unclustered" : "No clear themes",
      unclusteredIds,
    );
    return groupedRows;
  }, [displayedThemeResult, expandedThemes, filtered, scoped, themesEnabled]);

  const bookmarkCountLabel =
    `${filtered.length} ${filtered.length === 1 ? "bookmark" : "bookmarks"}`;
  const themesTooltip = themeToggleTooltip(
    preferSemantic,
    semanticIndexReady,
    scoped.length,
    themesEnabled,
  );
  const granularityIndex = GRANULARITY_VALUES.indexOf(granularity);
  const granularityStatus =
    themeStatus === "loading"
      ? displayedThemeResult
        ? "Adjusting…"
        : "Finding…"
      : displayedThemeResult
        ? `${displayedThemeResult.clusters.length} ${
            displayedThemeResult.clusters.length === 1 ? "theme" : "themes"
          }`
        : "";

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    getItemKey: (index) => rows[index]?.key ?? index,
    estimateSize: (index) => rows[index]?.kind === "theme" ? 34 : 104,
    overscan: 5,
    // Rows vary in height (note text, inline editor); measure the real DOM so
    // positions stay correct as notes are added/edited.
    measureElement: (el) => el.getBoundingClientRect().height,
  });

  return (
    <div className="h-full flex flex-col bg-[var(--bg-sidebar)] border-l border-[var(--border-main)]">
      <div className="p-2 border-b border-[var(--border-main)] flex flex-col gap-2">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <h2 className="truncate text-xs font-semibold text-[var(--text-main)]">Bookmarks</h2>
            <span
              aria-label={bookmarkCountLabel}
              className="inline-flex min-w-5 items-center justify-center rounded-full bg-[var(--bg-active)] px-1.5 py-0.5 text-[10px] tabular-nums text-[var(--text-muted)]"
            >
              {filtered.length}
            </span>
          </div>
          <Tooltip content="Close bookmarks">
            <button
              type="button"
              onClick={closePane}
              className="w-7 h-7 flex items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
            >
              <X size={14} />
            </button>
          </Tooltip>
        </div>
        <input
          value={filterText}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter bookmarks"
          className="min-w-0 w-full bg-[var(--bg-app)] border border-[var(--border-main)] rounded px-2 py-1 text-xs outline-none focus:border-[var(--accent-blue)]"
        />
        <div className="flex items-center justify-between gap-2">
          <div className="inline-flex rounded border border-[var(--border-main)] overflow-hidden bg-[var(--bg-active)]">
            <button
              type="button"
              disabled={!selectedPath}
              onClick={() => setScope("current")}
              className={`px-2 py-1 text-[11px] ${scopedToCurrentFile ? "text-[var(--text-main)] bg-[var(--bg-header)]" : "text-[var(--text-muted)]"} disabled:opacity-40`}
            >
              This file
            </button>
            <button
              type="button"
              onClick={() => setScope("all")}
              className={`px-2 py-1 text-[11px] border-l border-[var(--border-main)] ${!scopedToCurrentFile ? "text-[var(--text-main)] bg-[var(--bg-header)]" : "text-[var(--text-muted)]"}`}
            >
              All
            </button>
          </div>
          <div className="flex items-center gap-1">
            <Tooltip content={themesTooltip}>
              <button
                type="button"
                disabled={!themesAvailable}
                aria-pressed={themesEnabled}
                onClick={() => setThemesEnabled((enabled) => !enabled)}
                className={`w-7 h-7 flex-shrink-0 flex items-center justify-center rounded border border-[var(--border-main)] hover:text-[var(--text-main)] disabled:opacity-40 ${
                  themesEnabled
                    ? "bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]"
                    : "bg-[var(--bg-active)] text-[var(--text-muted)]"
                }`}
              >
                <Layers size={13} />
              </button>
            </Tooltip>
            <Tooltip content={dock === "Left" ? "Dock right" : "Dock left"}>
              <button
                type="button"
                onClick={() => setDock(dock === "Left" ? "Right" : "Left")}
                className="w-7 h-7 flex-shrink-0 flex items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <Sidebar size={13} />
              </button>
            </Tooltip>
          </div>
        </div>
        {themesEnabled && (
          <div className="flex flex-col gap-1 border-t border-[var(--border-main)] pt-2">
            <div className="flex items-center justify-between gap-2 text-[10px] text-[var(--text-dim)]">
              <span>{GRANULARITY_LABELS[granularity]}</span>
              <span aria-live="polite">{granularityStatus}</span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-[9px] text-[var(--text-dim)]">Fewer</span>
              <input
                type="range"
                min={0}
                max={GRANULARITY_VALUES.length - 1}
                step={1}
                value={granularityIndex}
                aria-label="Theme granularity"
                aria-valuetext={GRANULARITY_LABELS[granularity]}
                onChange={(event) =>
                  updateGranularity(
                    GRANULARITY_VALUES[Number(event.currentTarget.value)] ?? "balanced",
                  )
                }
                className="min-w-0 flex-1 accent-[var(--accent-blue)]"
              />
              <span className="text-[9px] text-[var(--text-dim)]">More</span>
            </div>
          </div>
        )}
      </div>

      <div ref={parentRef} className="flex-1 overflow-auto custom-scrollbar">
        {themesEnabled && themeStatus === "loading" && (
          <div className="p-4 text-xs text-[var(--text-dim)]">
            {displayedThemeResult ? "Adjusting themes…" : "Finding themes…"}
          </div>
        )}
        <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => {
            const row = rows[item.index];
            return (
              <div
                key={row.key}
                data-index={item.index}
                ref={virtualizer.measureElement}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${item.start}px)`,
                }}
                className={row.kind === "theme" ? "px-2 pt-3 pb-1" : "p-2"}
              >
                {row.kind === "theme" ? (
                  <button
                    type="button"
                    aria-expanded={row.expanded}
                    aria-label={`${row.expanded ? "Collapse" : "Expand"} cluster: ${row.label}`}
                    onClick={() => toggleTheme(row.key)}
                    className="flex w-full min-w-0 items-start gap-1.5 rounded px-1 py-1 text-left text-[var(--text-dim)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
                  >
                    {row.expanded ? (
                      <ChevronDown size={12} className="mt-0.5 flex-shrink-0" />
                    ) : (
                      <ChevronRight size={12} className="mt-0.5 flex-shrink-0" />
                    )}
                    <span className="min-w-0 flex-1 whitespace-pre-wrap break-words text-[10px] font-medium leading-snug">
                      {row.label}
                    </span>
                    <span className="flex-shrink-0 text-[10px] tabular-nums">{row.count}</span>
                  </button>
                ) : (
                  <BookmarkCard
                    bookmark={row.bookmark}
                    isEditing={editingId === row.bookmark.id}
                    draftNote={draftNote}
                    zoteroEnabled={zoteroEnabled}
                    onDraftNoteChange={setDraftNote}
                    onOpen={() =>
                      openMatch({
                        path: row.bookmark.path,
                        origin: row.bookmark.origin,
                        text_range: row.bookmark.text_range,
                      })
                    }
                    onStartEditing={() => startEditingNote(row.bookmark)}
                    onCancelEditing={cancelEditingNote}
                    onSave={() => saveNote(row.bookmark.id)}
                    onCopyCitation={() => copyCitation(row.bookmark)}
                    onRemove={() => remove(row.bookmark.id)}
                  />
                )}
              </div>
            );
          })}
        </div>
        {themeStatus !== "loading" && filtered.length === 0 && (
          <div className="p-4 text-xs text-[var(--text-dim)]">No bookmarks</div>
        )}
      </div>
    </div>
  );
}

interface BookmarkCardProps {
  bookmark: Bookmark;
  isEditing: boolean;
  draftNote: string;
  zoteroEnabled: boolean;
  onDraftNoteChange: (note: string) => void;
  onOpen: () => void;
  onStartEditing: () => void;
  onCancelEditing: () => void;
  onSave: () => Promise<void>;
  onCopyCitation: () => Promise<void>;
  onRemove: () => Promise<void>;
}

function BookmarkCard({
  bookmark,
  isEditing,
  draftNote,
  zoteroEnabled,
  onDraftNoteChange,
  onOpen,
  onStartEditing,
  onCancelEditing,
  onSave,
  onCopyCitation,
  onRemove,
}: BookmarkCardProps) {
  const page = bookmarkPage(bookmark);
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(event) => {
        if (event.key === "Enter") onOpen();
      }}
      className="border border-[var(--border-main)] bg-[var(--bg-app)] rounded p-2 text-left hover:border-[var(--border-strong)] cursor-pointer"
    >
      <div className="flex items-start justify-between gap-2">
        <p className="text-xs text-[var(--text-main)] line-clamp-3">{bookmark.quote}</p>
        {page && (
          <span className="text-[10px] px-1.5 py-0.5 rounded bg-[var(--bg-active)] text-[var(--text-muted)] flex-shrink-0">
            p.{page}
          </span>
        )}
      </div>
      {isEditing ? (
        <div className="mt-2" onClick={(event) => event.stopPropagation()}>
          <textarea
            autoFocus
            value={draftNote}
            onChange={(event) => onDraftNoteChange(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                event.preventDefault();
                onSave().catch(console.error);
              } else if (event.key === "Escape") {
                event.preventDefault();
                onCancelEditing();
              }
            }}
            placeholder="Add a note…"
            rows={2}
            className="w-full resize-y bg-[var(--bg-sidebar)] border border-[var(--border-main)] rounded px-2 py-1 text-xs text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
          />
          <div className="mt-1 flex justify-end gap-1">
            <button
              type="button"
              onClick={onCancelEditing}
              className="px-2 py-0.5 text-[10px] rounded border border-[var(--border-main)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
            >
              Cancel
            </button>
            <button
              type="button"
              onClick={() => onSave().catch(console.error)}
              className="px-2 py-0.5 text-[10px] rounded bg-[var(--accent-blue)] text-white hover:opacity-90"
            >
              Save
            </button>
          </div>
        </div>
      ) : (
        bookmark.note && (
          <p className="mt-2 text-[11px] text-[var(--text-muted)] whitespace-pre-wrap border-l-2 border-[var(--border-strong)] pl-2">
            {bookmark.note}
          </p>
        )
      )}
      <div className="mt-2 flex items-center gap-1 text-[10px] text-[var(--text-dim)]">
        <span className="truncate flex-1">{fileName(bookmark.path)}</span>
        <Tooltip content={bookmark.note ? "Edit note" : "Add note"}>
          <button
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              onStartEditing();
            }}
            className={`p-1 hover:text-[var(--accent-blue)] ${bookmark.note ? "text-[var(--accent-blue)]" : ""}`}
          >
            <Edit2 size={12} />
          </button>
        </Tooltip>
        {zoteroEnabled && (
          <Tooltip content="Get citation from Zotero">
            <CopyButton
              copy={onCopyCitation}
              onClick={(event) => event.stopPropagation()}
              copiedChildren={<Check size={12} />}
              className="p-1 hover:text-[var(--accent-blue)]"
            >
              <FileText size={12} />
            </CopyButton>
          </Tooltip>
        )}
        <Tooltip content="Copy as markdown">
          <CopyButton
            copy={() => api.writeClipboard(toMarkdown(bookmark))}
            onClick={(event) => event.stopPropagation()}
            copiedChildren={<Check size={12} />}
            className="p-1 hover:text-[var(--accent-blue)]"
          >
            <Copy size={12} />
          </CopyButton>
        </Tooltip>
        <Tooltip content="Delete bookmark">
          <button
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              onRemove().catch(console.error);
            }}
            className="p-1 hover:text-[var(--text-error)]"
          >
            <Trash2 size={12} />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
