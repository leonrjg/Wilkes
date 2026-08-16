import React, { useEffect, useRef, useState } from "react";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { configuredLibraryRoots, pathsEqual } from "../lib/configuredRoots";
import { confirmDialog } from "../lib/utils/dialog";
import type { FileEntry } from "../lib/types";
import { useChatStore } from "../stores/useChatStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { DirectoryTree } from "./DirectoryTree";
import { fileName } from "./DocumentEntryRow";
import { useToasts } from "./Toast";

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

function sanitizeSuggestedNamePart(value: string): string {
  const sanitized = value
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/[. ]+$/g, "");
  return /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(sanitized)
    ? `_${sanitized}`
    : sanitized;
}

function suggestedFileName(entry: FileEntry): string | null {
  const title = entry.title?.trim();
  if (!title) return null;

  const author = entry.author?.trim();
  const year = entry.publication_date?.match(/^\d{4}/)?.[0];
  const descriptiveName = [author, title].filter(Boolean).join(" - ");
  const stem = sanitizeSuggestedNamePart(
    year ? `${descriptiveName} (${year})` : descriptiveName,
  );
  if (!stem) return null;

  const currentName = fileName(entry.path);
  const extensionStart = currentName.lastIndexOf(".");
  const extension = extensionStart > 0 ? currentName.slice(extensionStart) : "";
  const suggestion = `${stem}${extension}`;
  return suggestion === currentName ? null : suggestion;
}

interface UseFileContextMenuOptions {
  /**
   * Resolves the indexed entry behind a path so Rename can suggest a name from
   * document metadata. Panes that display documents which are not part of the
   * current file list (related documents, for instance) pass their own lookup.
   */
  entryForPath?: (path: string) => FileEntry | undefined;
}

/**
 * Owns the file context menu together with the dialogs and mutations its items
 * trigger (rename, move, delete). Every surface that offers per-file actions —
 * the result list, the viewer tabs — renders the same menu from here rather
 * than assembling its own items and dialogs.
 */
export function useFileContextMenu({ entryForPath }: UseFileContextMenuOptions = {}) {
  const { addToast } = useToasts();
  const { menu, openMenu, closeMenu } = useContextMenu<ContextMenuTarget>();
  const renameInputRef = useRef<HTMLInputElement>(null);
  const [renameTarget, setRenameTarget] = useState<{
    path: string;
    name: string;
    suggestion: string | null;
  } | null>(null);
  const [moveTarget, setMoveTarget] = useState<{
    path: string;
    root: string;
    roots: string[];
  } | null>(null);

  useEffect(() => {
    if (!renameTarget) return;
    requestAnimationFrame(() => {
      const input = renameInputRef.current;
      if (!input) return;
      input.focus();
      input.setSelectionRange(0, editableNameEnd(renameTarget.name));
    });
  }, [renameTarget?.path]);

  const onToast = (message: string, type: "success" | "error") => addToast(message, { type });

  // A mutation invalidates whichever listing is on screen: results when a query
  // is active, the file list otherwise.
  const refreshAfterMutation = async () => {
    if (useSearchStore.getState().hasQuery) {
      await useSearchStore.getState().replaySearch();
    } else {
      useSettingsStore.getState().refreshFileList();
    }
  };

  const resolveEntry = (path: string): FileEntry | undefined => {
    if (entryForPath) return entryForPath(path);
    const settingsState = useSettingsStore.getState();
    return [...settingsState.fileList, ...settingsState.omittedFileList].find(
      (candidate) => candidate.path === path,
    );
  };

  const openRenameDialog = (path: string) => {
    const entry = resolveEntry(path);
    setRenameTarget({
      path,
      name: fileName(path),
      suggestion: entry ? suggestedFileName(entry) : null,
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
      useViewerStore.getState().closePath(path);
      useChatStore.getState().removeContext(path);
      await refreshAfterMutation();
      onToast(
        isTrash ? `Moved "${name}" to Trash` : `Permanently deleted "${name}"`,
        "success",
      );
    } catch (error) {
      console.error("Failed to delete file:", error);
      onToast(error instanceof Error ? error.message : "Failed to delete file", "error");
    }
  };

  const openFileMenu = (event: React.MouseEvent, target: ContextMenuTarget) => {
    const settingsState = useSettingsStore.getState();
    const otherRoots = configuredLibraryRoots(settingsState).filter(
      (root) => !pathsEqual(root, dirName(target.path)),
    );
    openMenu({
      event,
      target,
      items: buildFileContextMenuItems({
        target,
        api,
        capabilities: { canOpenInFileManager: isTauri },
        settings: settingsState.settings,
        onToast,
        onRenameRequest: openRenameDialog,
        availableRoots: otherRoots,
        onMoveRequest: (path) =>
          setMoveTarget({ path, root: otherRoots[0] ?? "", roots: otherRoots }),
        deletionKind: source.deletionKind,
        onDeleteRequest: handleDeleteRequest,
      }),
    });
  };

  const renameFile = async (nextName: string) => {
    if (!renameTarget) return;

    const oldPath = renameTarget.path;
    const oldName = fileName(oldPath);
    nextName = nextName.trim();
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
      useViewerStore.getState().closePath(oldPath);
      await refreshAfterMutation();
      setRenameTarget(null);
      onToast("File renamed", "success");
    } catch (error) {
      console.error("Failed to rename file:", error);
      onToast("Failed to rename file", "error");
    }
  };

  const handleRenameSubmit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!renameTarget) return;
    void renameFile(renameTarget.name);
  };

  const handleSuggestedRename = () => {
    if (!renameTarget?.suggestion) return;
    void renameFile(renameTarget.suggestion);
  };

  const handleMoveSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!moveTarget || !moveTarget.root) return;

    const oldPath = moveTarget.path;
    try {
      await (source as DesktopSourceApi).moveFile(oldPath, moveTarget.root);
      useViewerStore.getState().closePath(oldPath);
      await refreshAfterMutation();
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
        {renameTarget.suggestion && (
          <div className="mb-3 rounded border border-[var(--border-main)] bg-[var(--bg-active)] p-2">
            <div className="mb-1 text-[10px] font-medium uppercase tracking-wide text-[var(--text-dim)]">
              Suggested from metadata
            </div>
            <div className="break-words text-xs text-[var(--text-main)]">
              {renameTarget.suggestion}
            </div>
            <button
              type="button"
              onClick={handleSuggestedRename}
              className="mt-2 rounded border border-[var(--accent-blue)] px-2.5 py-1 text-xs font-medium text-[var(--accent-blue)] hover:bg-[var(--bg-hover)]"
            >
              Rename to suggestion
            </button>
          </div>
        )}
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

  /** Menu and dialogs; render once inside the surface that owns the rows. */
  const fileMenu = (
    <>
      <ContextMenu menu={menu} onClose={closeMenu} />
      {renameDialog}
      {moveDialog}
    </>
  );

  return { openFileMenu, fileMenu };
}
