import { useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Check, Copy, Edit2, FileText, Sidebar, Trash2, X } from "react-feather";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { activeViewerTab, useViewerStore } from "../stores/useViewerStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { toMarkdown } from "../lib/utils/bookmarkMarkdown";
import { api } from "../services";
import { useToasts } from "./Toast";
import { Tooltip } from "./Tooltip";
import { CopyButton } from "./CopyButton";
import type { Bookmark } from "../lib/types";

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
  const selectedPath = useViewerStore((state) => activeViewerTab(state)?.path ?? null);
  const openMatch = useViewerStore((state) => state.openMatch);
  const dock = useSettingsStore((s) => s.bookmarksDock);
  const setDock = useSettingsStore((s) => s.setBookmarksDock);
  const closePane = useBookmarksStore((s) => s.closePane);
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

  const scopedToCurrentFile = scope === "current";
  const filtered = useMemo(() => {
    const query = filterText.trim().toLowerCase();
    return bookmarks.filter((bookmark) => {
      if (scope === "current" && bookmark.path !== selectedPath) return false;
      if (!query) return true;
      return (
        bookmark.quote.toLowerCase().includes(query) ||
        bookmark.path.toLowerCase().includes(query)
      );
    });
  }, [bookmarks, filterText, scope, selectedPath]);
  const bookmarkCountLabel =
    `${filtered.length} ${filtered.length === 1 ? "bookmark" : "bookmarks"}`;

  const virtualizer = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 104,
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

      <div ref={parentRef} className="flex-1 overflow-auto custom-scrollbar">
        <div style={{ height: `${virtualizer.getTotalSize()}px`, position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => {
            const bookmark = filtered[item.index];
            const page = bookmarkPage(bookmark);
            const isEditing = editingId === bookmark.id;
            return (
              <div
                key={bookmark.id}
                data-index={item.index}
                ref={virtualizer.measureElement}
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
                  onClick={() =>
                    openMatch({
                      path: bookmark.path,
                      origin: bookmark.origin,
                      text_range: bookmark.text_range,
                    })
                  }
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      openMatch({
                        path: bookmark.path,
                        origin: bookmark.origin,
                        text_range: bookmark.text_range,
                      });
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
                  {isEditing ? (
                    <div className="mt-2" onClick={(event) => event.stopPropagation()}>
                      <textarea
                        autoFocus
                        value={draftNote}
                        onChange={(event) => setDraftNote(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                            event.preventDefault();
                            saveNote(bookmark.id).catch(console.error);
                          } else if (event.key === "Escape") {
                            event.preventDefault();
                            cancelEditingNote();
                          }
                        }}
                        placeholder="Add a note…"
                        rows={2}
                        className="w-full resize-y bg-[var(--bg-sidebar)] border border-[var(--border-main)] rounded px-2 py-1 text-xs text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
                      />
                      <div className="mt-1 flex justify-end gap-1">
                        <button
                          type="button"
                          onClick={cancelEditingNote}
                          className="px-2 py-0.5 text-[10px] rounded border border-[var(--border-main)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
                        >
                          Cancel
                        </button>
                        <button
                          type="button"
                          onClick={() => saveNote(bookmark.id).catch(console.error)}
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
                          startEditingNote(bookmark);
                        }}
                        className={`p-1 hover:text-[var(--accent-blue)] ${bookmark.note ? "text-[var(--accent-blue)]" : ""}`}
                      >
                        <Edit2 size={12} />
                      </button>
                    </Tooltip>
                    {zoteroEnabled && (
                      <Tooltip content="Get citation from Zotero">
                        <CopyButton
                          copy={() => copyCitation(bookmark)}
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
                          remove(bookmark.id).catch(console.error);
                        }}
                        className="p-1 hover:text-[var(--text-error)]"
                      >
                        <Trash2 size={12} />
                      </button>
                    </Tooltip>
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
