import { useState } from "react";
import { Copy, FolderPlus, RefreshCw, Tag as TagIcon, Trash2, X } from "react-feather";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { confirmDialog } from "../lib/utils/dialog";
import { useResearchStore } from "../stores/useResearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import { useChatStore } from "../stores/useChatStore";
import { useToasts } from "./Toast";
import { fileName } from "./DocumentEntryRow";

interface Props {
  /** Paths currently ticked, in list order. */
  selected: string[];
  /** Every path the list is showing, so "Select all" can reach them. */
  visible: string[];
  onSelectionChange: (paths: string[]) => void;
  /** Roots a selection can be moved into. Empty where moving is unavailable. */
  moveRoots: string[];
  readOnly: boolean;
  onExit: () => void;
}

const BUTTON =
  "flex h-6 flex-shrink-0 items-center gap-1 rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-1.5 text-[11px] text-[var(--text-main)] hover:bg-[var(--bg-hover)] disabled:opacity-40 disabled:hover:bg-[var(--bg-active)]";

/**
 * Applies the file actions the context menu offers per file to a whole
 * selection at once. Selection lives in the result list; this bar only reads it
 * and runs the mutations, refreshing the file list once per action rather than
 * once per file.
 */
export default function BulkActionsBar({
  selected,
  visible,
  onSelectionChange,
  moveRoots,
  readOnly,
  onExit,
}: Props) {
  const { addToast } = useToasts();
  const tags = useResearchStore((state) => state.tags);
  const [busy, setBusy] = useState(false);
  const [tagMenuOpen, setTagMenuOpen] = useState(false);
  const [moveMenuOpen, setMoveMenuOpen] = useState(false);
  const [newTagName, setNewTagName] = useState("");
  const count = selected.length;
  const allSelected = visible.length > 0 && count === visible.length;

  const refresh = () => useSettingsStore.getState().refreshFileList();

  /**
   * Runs `action` for each selected path, reporting how many succeeded. One
   * file that fails must not hide the rest, so failures are collected rather
   * than thrown; each is logged so nothing is suppressed silently.
   */
  const runForEach = async (
    verb: string,
    action: (path: string) => Promise<void>,
    options: { clearSelection?: boolean } = {},
  ) => {
    if (count === 0 || busy) return;
    setBusy(true);
    const failures: string[] = [];
    for (const path of selected) {
      try {
        await action(path);
      } catch (error) {
        console.error(`Bulk ${verb} failed for ${path}:`, error);
        failures.push(fileName(path));
      }
    }
    await refresh();
    setBusy(false);
    if (options.clearSelection) onSelectionChange([]);
    if (failures.length === 0) {
      addToast(`${verb} ${count} file${count === 1 ? "" : "s"}`, { type: "success" });
    } else {
      addToast(
        `${verb} failed for ${failures.length} of ${count} file${count === 1 ? "" : "s"}: ${failures.join(", ")}`,
        { type: "error" },
      );
    }
  };

  const applyTag = async (tagId: string, tagName: string, remove: boolean) => {
    if (count === 0 || busy) return;
    setTagMenuOpen(false);
    setBusy(true);
    try {
      await useResearchStore.getState().updateDocumentTags({
        paths: selected,
        add_tag_ids: remove ? [] : [tagId],
        remove_tag_ids: remove ? [tagId] : [],
      });
      await refresh();
      addToast(
        `${remove ? "Removed" : "Added"} ${tagName} ${remove ? "from" : "to"} ${count} file${count === 1 ? "" : "s"}`,
        { type: "success" },
      );
    } catch (error) {
      console.error("Bulk tag update failed:", error);
      addToast("Failed to update tags", { type: "error" });
    } finally {
      setBusy(false);
    }
  };

  /** Mirrors the context menu's "Create and add tag": with an empty library the
   *  tag list alone would leave the button useless. */
  const createAndApplyTag = async () => {
    const name = newTagName.trim();
    if (!name || count === 0 || busy) return;
    setBusy(true);
    try {
      const research = useResearchStore.getState();
      const created = await research.createTag({ name });
      await research.updateDocumentTags({
        paths: selected,
        add_tag_ids: [created.id],
        remove_tag_ids: [],
      });
      await refresh();
      setNewTagName("");
      setTagMenuOpen(false);
      addToast(`Created ${created.name} and added it to ${count} file${count === 1 ? "" : "s"}`, {
        type: "success",
      });
    } catch (error) {
      console.error("Failed to create and add tag:", error);
      addToast("Failed to create and add tag", { type: "error" });
    } finally {
      setBusy(false);
    }
  };

  const copyPaths = async () => {
    if (count === 0) return;
    try {
      await api.writeClipboard(selected.join("\n"));
      addToast(`Copied ${count} path${count === 1 ? "" : "s"}`, { type: "success" });
    } catch (error) {
      console.error("Failed to copy paths:", error);
      addToast("Failed to copy paths", { type: "error" });
    }
  };

  const deleteSelected = async () => {
    if (count === 0 || busy) return;
    const isTrash = source.deletionKind === "trash";
    const confirmed = await confirmDialog(
      isTrash
        ? `Move ${count} file${count === 1 ? "" : "s"} to Trash? You can restore them from Trash.`
        : `Permanently delete ${count} file${count === 1 ? "" : "s"}? This cannot be undone.`,
    );
    if (!confirmed) return;
    await runForEach(
      isTrash ? "Moved to Trash" : "Deleted",
      async (path) => {
        await source.deleteFile(path);
        useViewerStore.getState().closePath(path);
        useChatStore.getState().removeContext(path);
      },
      { clearSelection: true },
    );
  };

  const moveSelected = async (root: string) => {
    setMoveMenuOpen(false);
    await runForEach(
      "Moved",
      async (path) => {
        await (source as DesktopSourceApi).moveFile(path, root);
        useViewerStore.getState().closePath(path);
      },
      { clearSelection: true },
    );
  };

  return (
    <div
      role="toolbar"
      aria-label="Bulk file actions"
      className="relative flex flex-shrink-0 flex-wrap items-center gap-1 border-b border-[var(--border-main)] bg-[var(--bg-active)] px-2 py-1.5 text-xs text-[var(--text-muted)]"
    >
      <span className="tabular-nums whitespace-nowrap">{count} selected</span>
      <button
        type="button"
        className={BUTTON}
        onClick={() => onSelectionChange(allSelected ? [] : visible)}
      >
        {allSelected ? "Clear" : "Select all"}
      </button>

      <div className="relative">
        <button
          type="button"
          className={BUTTON}
          disabled={count === 0 || busy}
          aria-haspopup="menu"
          aria-expanded={tagMenuOpen}
          onClick={() => setTagMenuOpen((open) => !open)}
        >
          <TagIcon size={11} aria-hidden="true" />
          Tags
        </button>
        {tagMenuOpen && (
          <div
            role="menu"
            className="absolute left-0 top-7 z-[120] max-h-64 w-56 overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-app)] py-1 shadow-xl"
          >
            <form
              className="flex items-center gap-1 border-b border-[var(--border-main)] px-1 pb-1"
              onSubmit={(event) => {
                event.preventDefault();
                void createAndApplyTag();
              }}
            >
              <input
                aria-label="New tag name"
                placeholder="New tag…"
                value={newTagName}
                onChange={(event) => setNewTagName(event.target.value)}
                className="h-6 min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-1.5 text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
              />
              <button
                type="submit"
                disabled={!newTagName.trim() || busy}
                className="rounded px-1.5 py-0.5 text-[10px] text-[var(--accent-blue)] hover:bg-[var(--bg-hover)] disabled:opacity-40"
              >
                Add
              </button>
            </form>
            {tags.length === 0 && (
              <div className="px-2 py-1 text-[10px] italic text-[var(--text-dim)]">
                No tags yet
              </div>
            )}
            {tags.map((tag) => (
              <div key={tag.id} className="flex items-center gap-1 px-1">
                <span className="min-w-0 flex-1 truncate text-[11px] text-[var(--text-main)]">
                  {tag.name}
                </span>
                <button
                  type="button"
                  className="rounded px-1.5 py-0.5 text-[10px] text-[var(--accent-blue)] hover:bg-[var(--bg-hover)]"
                  onClick={() => void applyTag(tag.id, tag.name, false)}
                >
                  Add
                </button>
                <button
                  type="button"
                  className="rounded px-1.5 py-0.5 text-[10px] text-[var(--text-muted)] hover:bg-[var(--bg-hover)]"
                  onClick={() => void applyTag(tag.id, tag.name, true)}
                >
                  Remove
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <Tooltip content="Copy the selected paths, one per line">
        <button type="button" className={BUTTON} disabled={count === 0} onClick={() => void copyPaths()}>
          <Copy size={11} aria-hidden="true" />
        </button>
      </Tooltip>

      <Tooltip content="Refresh metadata for the selection">
        <button
          type="button"
          aria-label="Refresh metadata for the selection"
          className={BUTTON}
          disabled={count === 0 || busy}
          onClick={() => void runForEach("Refreshed metadata for", (path) => api.refreshFileMetadata(path))}
        >
          <RefreshCw size={11} aria-hidden="true" />
        </button>
      </Tooltip>

      {isTauri && !readOnly && moveRoots.length > 0 && (
        <div className="relative">
          <button
            type="button"
            aria-label="Move the selection"
            className={BUTTON}
            disabled={count === 0 || busy}
            aria-haspopup="menu"
            aria-expanded={moveMenuOpen}
            onClick={() => setMoveMenuOpen((open) => !open)}
          >
            <FolderPlus size={11} aria-hidden="true" />
          </button>
          {moveMenuOpen && (
            <div
              role="menu"
              className="absolute left-0 top-7 z-[120] max-h-64 w-64 overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-app)] py-1 shadow-xl"
            >
              {moveRoots.map((root) => (
                <button
                  key={root}
                  type="button"
                  className="block w-full truncate px-2 py-1 text-left font-mono text-[10px] text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
                  onClick={() => void moveSelected(root)}
                >
                  {root}
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      {!readOnly && (
        <Tooltip content={source.deletionKind === "trash" ? "Move the selection to Trash" : "Delete the selection permanently"}>
          <button
            type="button"
            aria-label={source.deletionKind === "trash" ? "Move the selection to Trash" : "Delete the selection permanently"}
            className={`${BUTTON} text-[var(--accent-red)]`}
            disabled={count === 0 || busy}
            onClick={() => void deleteSelected()}
          >
            <Trash2 size={11} aria-hidden="true" />
          </button>
        </Tooltip>
      )}

      <button
        type="button"
        aria-label="Exit selection mode"
        className="ml-auto flex h-6 w-6 flex-shrink-0 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)]"
        onClick={onExit}
      >
        <X size={12} aria-hidden="true" />
      </button>
    </div>
  );
}
