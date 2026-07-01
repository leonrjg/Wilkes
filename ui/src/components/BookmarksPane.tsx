import { useEffect, useMemo, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Copy, Sidebar, Trash2 } from "react-feather";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { toMarkdown } from "../lib/utils/bookmarkMarkdown";
import type { Bookmark } from "../lib/types";

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

function bookmarkPage(bookmark: Bookmark) {
  return "PdfPage" in bookmark.origin ? bookmark.origin.PdfPage.page : null;
}

export default function BookmarksPane() {
  const parentRef = useRef<HTMLDivElement>(null);
  const bookmarks = useBookmarksStore((s) => s.bookmarks);
  const filterText = useBookmarksStore((s) => s.filterText);
  const scopePath = useBookmarksStore((s) => s.scopePath);
  const setFilter = useBookmarksStore((s) => s.setFilter);
  const setScope = useBookmarksStore((s) => s.setScope);
  const remove = useBookmarksStore((s) => s.remove);
  const selectedPath = useSearchStore((s) => s.selectedMatch?.path ?? null);
  const selectMatch = useSearchStore((s) => s.selectMatch);
  const dock = useSettingsStore((s) => s.bookmarksDock);
  const setDock = useSettingsStore((s) => s.setBookmarksDock);

  useEffect(() => {
    setScope(selectedPath);
  }, [selectedPath, setScope]);

  const scopedToCurrentFile = scopePath !== null;
  const filtered = useMemo(() => {
    const query = filterText.trim().toLowerCase();
    return bookmarks.filter((bookmark) => {
      if (scopePath && bookmark.path !== scopePath) return false;
      if (!query) return true;
      return (
        bookmark.quote.toLowerCase().includes(query) ||
        bookmark.path.toLowerCase().includes(query)
      );
    });
  }, [bookmarks, filterText, scopePath]);

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 104,
    overscan: 5,
  });

  return (
    <div className="h-full flex flex-col bg-[var(--bg-sidebar)] border-l border-[var(--border-main)]">
      <div className="p-2 border-b border-[var(--border-main)] flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <input
            value={filterText}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter bookmarks"
            className="min-w-0 flex-1 bg-[var(--bg-app)] border border-[var(--border-main)] rounded px-2 py-1 text-xs outline-none focus:border-[var(--accent-blue)]"
          />
          <button
            type="button"
            onClick={() => setDock(dock === "Left" ? "Right" : "Left")}
            title={dock === "Left" ? "Dock right" : "Dock left"}
            className="w-7 h-7 flex items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
          >
            <Sidebar size={13} />
          </button>
        </div>
        <div className="inline-flex rounded border border-[var(--border-main)] overflow-hidden self-start bg-[var(--bg-active)]">
          <button
            type="button"
            disabled={!selectedPath}
            onClick={() => setScope(selectedPath)}
            className={`px-2 py-1 text-[11px] ${scopedToCurrentFile ? "text-[var(--text-main)] bg-[var(--bg-header)]" : "text-[var(--text-muted)]"} disabled:opacity-40`}
          >
            This file
          </button>
          <button
            type="button"
            onClick={() => setScope(null)}
            className={`px-2 py-1 text-[11px] border-l border-[var(--border-main)] ${!scopedToCurrentFile ? "text-[var(--text-main)] bg-[var(--bg-header)]" : "text-[var(--text-muted)]"}`}
          >
            All
          </button>
        </div>
      </div>

      <div ref={parentRef} className="flex-1 overflow-auto custom-scrollbar">
        <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => {
            const bookmark = filtered[item.index];
            const page = bookmarkPage(bookmark);
            return (
              <div
                key={bookmark.id}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${item.start}px)`,
                }}
                className="p-2"
              >
                <div
                  role="button"
                  tabIndex={0}
                  onClick={() => selectMatch({ path: bookmark.path, origin: bookmark.origin })}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      selectMatch({ path: bookmark.path, origin: bookmark.origin });
                    }
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
                  <div className="mt-2 flex items-center gap-1 text-[10px] text-[var(--text-dim)]">
                    <span className="truncate flex-1">{fileName(bookmark.path)}</span>
                    <button
                      type="button"
                      onClick={(event) => {
                        event.stopPropagation();
                        navigator.clipboard?.writeText(toMarkdown(bookmark)).catch(console.error);
                      }}
                      title="Copy as markdown"
                      className="p-1 hover:text-[var(--accent-blue)]"
                    >
                      <Copy size={12} />
                    </button>
                    <button
                      type="button"
                      onClick={(event) => {
                        event.stopPropagation();
                        remove(bookmark.id).catch(console.error);
                      }}
                      title="Delete bookmark"
                      className="p-1 hover:text-[var(--text-error)]"
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
        {filtered.length === 0 && (
          <div className="p-4 text-xs text-[var(--text-dim)]">No bookmarks</div>
        )}
      </div>
    </div>
  );
}
