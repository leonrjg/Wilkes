import { useMemo, useState } from "react";
import { confirmDialog } from "../lib/utils/dialog";
import { Folder, FolderPlus, Star, X } from "react-feather";
import { useToasts } from "./Toast";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { useSettingsStore } from "../stores/useSettingsStore";
import { Tooltip } from "./Tooltip";
import { DirectoryTree, isStrictAncestor, parentPath } from "./DirectoryTree";

interface Props {
  directory: string;
  favorites: string[];
  recentDirs: string[];
  onChange: (dir: string) => void;
  onPickDirectory: () => void;
  onFavoriteAdd?: (dir: string) => void;
  onFavoriteRemove?: (dir: string) => void;
  onForgetDirectory?: (dir: string) => void;
  onRenameDirectory?: (oldPath: string, newPath: string) => void;
}

function shortPath(p: string): string {
  const home = p.match(/^\/Users\/[^/]+/) ?? p.match(/^\/home\/[^/]+/);
  if (home) return "~" + p.slice(home[0].length);
  return p;
}

function baseName(p: string): string {
  return p.replace(/[/\\]+$/, "").split(/[/\\]/).pop() || p;
}

export default function DirectoryPicker({
  directory,
  favorites,
  recentDirs,
  onChange,
  onPickDirectory,
  onFavoriteAdd,
  onFavoriteRemove,
  onForgetDirectory,
  onRenameDirectory,
}: Props) {
  const { addToast } = useToasts();
  const { menu, openMenu, closeMenu } = useContextMenu<ContextMenuTarget>();
  const settings = useSettingsStore((s) => s.settings);
  const isFavorite = (dir: string) => favorites.includes(dir);
  const onToast = (message: string, type: "success" | "error") => addToast(message, { type });

  const [createTarget, setCreateTarget] = useState<{
    destination: string;
    name: string;
  } | null>(null);

  const [renameTarget, setRenameTarget] = useState<{
    path: string;
    name: string;
  } | null>(null);

  // Combine favorites and recent dirs for the list, prioritizing favorites
  // and removing duplicates.
  const displayDirs = useMemo(() => {
    const combined = [...favorites];
    for (const d of recentDirs) {
      if (!combined.includes(d)) {
        combined.push(d);
      }
    }
    // Always ensure the current directory is in the list if it's not empty
    if (directory && !combined.includes(directory)) {
      combined.push(directory);
    }
    return combined;
  }, [favorites, recentDirs, directory]);

  // Destinations for the "new folder" dialog. We surface each current root plus
  // a synthetic "[parent]" node per distinct parent of the top-level roots, so a
  // folder can be created as a sibling of the roots. The parent nodes only ever
  // list the roots beneath them — never the parent's arbitrary, non-Wilkes
  // contents. DirectoryTree folds nested roots under their ancestor.
  const { createRoots, parentLabels, rootsByParent } = useMemo(() => {
    const topLevel = displayDirs.filter(
      (dir) => !displayDirs.some((other) => isStrictAncestor(other, dir)),
    );
    const rootsByParent = new Map<string, string[]>();
    for (const root of topLevel) {
      const parent = parentPath(root);
      if (!parent) continue;
      const siblings = rootsByParent.get(parent) ?? [];
      siblings.push(root);
      rootsByParent.set(parent, siblings);
    }
    const parents = Array.from(rootsByParent.keys());
    const parentLabels = Object.fromEntries(
      parents.map((parent) => [parent, `[${parent.split(/[/\\]/).pop() || parent}]`]),
    );
    return {
      createRoots: Array.from(new Set([...parents, ...displayDirs])),
      parentLabels,
      rootsByParent,
    };
  }, [displayDirs]);

  // A parent node reveals only the roots under it; every other node lists its
  // real subdirectories (staying within a Wilkes root).
  const loadCreateChildren = (path: string): Promise<string[]> => {
    const roots = rootsByParent.get(path);
    if (roots) return Promise.resolve(roots);
    return (source as DesktopSourceApi).listDirectories(path);
  };

  const openCreateDialog = () => {
    setCreateTarget({ destination: createRoots[0] ?? "", name: "" });
  };

  const handleCreateSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!createTarget) return;
    const name = createTarget.name.trim();
    if (!createTarget.destination || !name) return;
    try {
      const created = await (source as DesktopSourceApi).createDirectory(
        createTarget.destination,
        name,
      );
      setCreateTarget(null);
      onChange(created);
      onToast(`Created folder "${name}"`, "success");
    } catch (error) {
      onToast(
        error instanceof Error ? error.message : "Could not create folder",
        "error",
      );
    }
  };

  const handleRenameSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!renameTarget) return;
    const oldPath = renameTarget.path;
    const name = renameTarget.name.trim();
    if (!name || name === baseName(oldPath)) {
      setRenameTarget(null);
      return;
    }
    if (/[/\\]/.test(name) || name === "." || name === "..") {
      onToast("Folder name cannot contain path separators", "error");
      return;
    }
    try {
      const newPath = await api.renameFile(oldPath, name);
      setRenameTarget(null);
      onRenameDirectory?.(oldPath, newPath);
      onToast(`Renamed folder to "${name}"`, "success");
    } catch (error) {
      onToast(
        error instanceof Error ? error.message : "Could not rename folder",
        "error",
      );
    }
  };

  return (
    <div className="flex items-center gap-1 min-w-0 w-full">
      <div className="flex h-6 items-center gap-0.5 bg-[var(--bg-active)] rounded overflow-hidden">
        <Tooltip content={directory || "Choose directory"}>
          <button
            onClick={onPickDirectory}
            className="h-full text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] px-3 flex-shrink-0 flex items-center gap-1.5"
          >
            <Folder size={12} />
            <span>Open folder</span>
          </button>
        </Tooltip>
      </div>

      {/* Folders list (Favorites + History) */}
      {displayDirs.length > 0 && (
        <div className="flex items-center gap-1 overflow-x-auto flex-1 min-w-0 custom-scrollbar">
          {displayDirs.map((b) => {
            const favorite = isFavorite(b);
            const active = b === directory;

            return (
              <div
                key={b}
                className={`flex h-6 items-center gap-0.5 rounded transition-colors group bg-[var(--bg-active)]`}
                onContextMenu={(event) =>
                  openMenu({
                    event,
                    target: { kind: "directory", path: b, open: () => onChange(b) },
                    items: buildFileContextMenuItems({
                      target: { kind: "directory", path: b, open: () => onChange(b) },
                      api,
                      capabilities: { canOpenInFileManager: isTauri },
                      settings,
                      onToast,
                      onRenameRequest: onRenameDirectory
                        ? (path) => setRenameTarget({ path, name: baseName(path) })
                        : undefined,
                    }),
                  })}
              >
                {onForgetDirectory && (
                  <Tooltip content="Remove from history">
                    <button
                      onClick={async (e) => {
                        e.stopPropagation();
                        const confirmed = await confirmDialog(`Remove "${shortPath(b)}" from your history?`);
                        if (confirmed) onForgetDirectory(b);
                      }}
                      className="h-full text-[10px] pl-1.5 pr-1 text-[var(--text-dim)] hover:text-[var(--text-error)] transition-colors"
                    >
                      <X size={12} />
                    </button>
                  </Tooltip>
                )}
                <Tooltip content={b} className="font-mono break-all">
                  <button
                    onClick={() => onChange(b)}
                    className={`h-full select-none text-xs px-2 flex-shrink-0 truncate max-w-[100px] transition-colors ${
                      active
                        ? "text-[var(--text-main)] font-bold"
                        : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
                    }`}
                  >
                    {shortPath(b).split("/").pop() || shortPath(b)}
                  </button>
                </Tooltip>
                {onFavoriteAdd && onFavoriteRemove && (
                  <Tooltip content={favorite ? "Remove favorite" : "Favorite this directory"}>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        favorite ? onFavoriteRemove(b) : onFavoriteAdd(b);
                      }}
                      className={`h-full text-[10px] px-1.5 transition-colors ${
                        favorite
                          ? "text-[var(--accent-blue)]"
                          : "text-[var(--text-dim)] hover:text-[var(--accent-blue)]"
                      }`}
                    >
                      <Star size={10} fill={favorite ? "currentColor" : "none"} />
                    </button>
                  </Tooltip>
                )}
              </div>
            );
          })}

          {/* Create a new folder as a sibling of, or within, the roots */}
          <Tooltip content="New folder">
            <button
              onClick={openCreateDialog}
              aria-label="New folder"
              className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
            >
              <FolderPlus size={12} />
            </button>
          </Tooltip>
        </div>
      )}

      {createTarget && (
        <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/35 px-4">
          <form
            role="dialog"
            aria-modal="true"
            aria-labelledby="create-folder-title"
            onSubmit={handleCreateSubmit}
            className="w-full max-w-md rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-3 shadow-2xl"
          >
            <div
              id="create-folder-title"
              className="mb-2 text-sm font-semibold text-[var(--text-main)]"
            >
              New folder in...
            </div>
            <DirectoryTree
              roots={createRoots}
              selected={createTarget.destination}
              onSelect={(destination) =>
                setCreateTarget((target) => (target ? { ...target, destination } : target))
              }
              loadChildren={loadCreateChildren}
              labels={parentLabels}
            />
            <input
              type="text"
              autoFocus
              value={createTarget.name}
              onChange={(e) =>
                setCreateTarget((target) => (target ? { ...target, name: e.target.value } : target))
              }
              placeholder="Folder name"
              aria-label="Folder name"
              className="mb-3 w-full rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-2 py-1.5 text-sm text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
            />
            <div className="flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setCreateTarget(null)}
                className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)]"
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={!createTarget.destination || !createTarget.name.trim()}
                className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
              >
                Create
              </button>
            </div>
          </form>
        </div>
      )}

      {renameTarget && (
        <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/35 px-4">
          <form
            role="dialog"
            aria-modal="true"
            aria-labelledby="rename-folder-title"
            onSubmit={handleRenameSubmit}
            className="w-full max-w-md rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-3 shadow-2xl"
          >
            <div
              id="rename-folder-title"
              className="mb-2 text-sm font-semibold text-[var(--text-main)]"
            >
              Rename "{baseName(renameTarget.path)}" to...
            </div>
            <input
              type="text"
              autoFocus
              value={renameTarget.name}
              onChange={(e) =>
                setRenameTarget((target) => (target ? { ...target, name: e.target.value } : target))
              }
              placeholder="Folder name"
              aria-label="New folder name"
              className="mb-3 w-full rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-2 py-1.5 text-sm text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
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
                disabled={!renameTarget.name.trim()}
                className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
              >
                Rename
              </button>
            </div>
          </form>
        </div>
      )}
      <ContextMenu menu={menu} onClose={closeMenu} />
    </div>
  );
}
