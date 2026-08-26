import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { confirmDialog } from "../lib/utils/dialog";
import { ChevronLeft, ChevronRight, Folder, FolderPlus, Star, X } from "react-feather";
import { useToasts } from "./Toast";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { useSettingsStore } from "../stores/useSettingsStore";
import { Tooltip } from "./preview";
import { DirectoryTree, isStrictAncestor, parentPath } from "./DirectoryTree";
import { configuredLibraryRoots } from "../lib/configuredRoots";

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

const ROOT_SCROLL_EDGE_TOLERANCE = 2;

function RootCarousel({
  activeRoot,
  contentKey,
  children,
}: {
  activeRoot: string;
  contentKey: string;
  children: React.ReactNode;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);

  const updateScrollBounds = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    const maxScrollLeft = Math.max(0, element.scrollWidth - element.clientWidth);
    setCanScrollLeft(element.scrollLeft > ROOT_SCROLL_EDGE_TOLERANCE);
    setCanScrollRight(
      element.scrollLeft < maxScrollLeft - ROOT_SCROLL_EDGE_TOLERANCE,
    );
  }, []);

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (!element) return;

    const resizeObserver = new ResizeObserver(updateScrollBounds);
    resizeObserver.observe(element);
    Array.from(element.children).forEach((child) => resizeObserver.observe(child));
    element.addEventListener("scroll", updateScrollBounds, { passive: true });
    window.addEventListener("resize", updateScrollBounds);
    updateScrollBounds();

    let boundsFrame: number | null = null;
    const activeFrame = requestAnimationFrame(() => {
      const active = element.querySelector<HTMLElement>('[data-root-active="true"]');
      active?.scrollIntoView?.({ behavior: "smooth", block: "nearest", inline: "nearest" });
      boundsFrame = requestAnimationFrame(updateScrollBounds);
    });

    return () => {
      cancelAnimationFrame(activeFrame);
      if (boundsFrame !== null) cancelAnimationFrame(boundsFrame);
      resizeObserver.disconnect();
      element.removeEventListener("scroll", updateScrollBounds);
      window.removeEventListener("resize", updateScrollBounds);
    };
  }, [activeRoot, contentKey, updateScrollBounds]);

  const scrollPage = (direction: -1 | 1) => {
    const element = scrollRef.current;
    if (!element) return;
    const distance = Math.max(120, element.clientWidth * 0.8);
    element.scrollBy({ left: direction * distance, behavior: "smooth" });
  };

  return (
    <div className="relative min-w-0 flex-1">
      <div
        ref={scrollRef}
        role="region"
        aria-label="Workspace roots"
        className="folder-strip-carousel flex min-w-0 items-center gap-1 overflow-x-auto"
      >
        {children}
      </div>
      {canScrollLeft && (
        <button
          type="button"
          aria-label="Scroll roots left"
          onClick={() => scrollPage(-1)}
          className="absolute inset-y-0 left-0 z-10 flex w-6 items-center justify-center rounded-r bg-[var(--bg-app)]/95 text-[var(--text-muted)] shadow-[7px_0_18px_-3px_rgba(0,0,0,0.24)] hover:text-[var(--text-main)]"
        >
          <ChevronLeft size={13} />
        </button>
      )}
      {canScrollRight && (
        <button
          type="button"
          aria-label="Scroll roots right"
          onClick={() => scrollPage(1)}
          className="absolute inset-y-0 right-0 z-10 flex w-6 items-center justify-center rounded-l bg-[var(--bg-app)]/95 text-[var(--text-muted)] shadow-[-7px_0_18px_-3px_rgba(0,0,0,0.24)] hover:text-[var(--text-main)]"
        >
          <ChevronRight size={13} />
        </button>
      )}
    </div>
  );
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

  const displayDirs = useMemo(
    () => configuredLibraryRoots({ directory, favorites, recentDirs }),
    [directory, favorites, recentDirs],
  );

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
            <span>Open</span>
          </button>
        </Tooltip>
      </div>

      {/* Folders list (Favorites + History) */}
      {displayDirs.length > 0 && (
        <RootCarousel activeRoot={directory} contentKey={displayDirs.join("\0")}>
          {displayDirs.map((b) => {
            const favorite = isFavorite(b);
            const active = b === directory;

            return (
              <div
                key={b}
                data-root-active={active ? "true" : undefined}
                className="group flex h-6 items-center rounded bg-[var(--bg-active)] transition-colors"
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
                      className="h-full pl-1 pr-0.5 text-[10px] text-[var(--text-dim)] transition-colors hover:text-[var(--text-error)]"
                    >
                      <X size={10} />
                    </button>
                  </Tooltip>
                )}
                <Tooltip content={b} className="font-mono break-all">
                  <button
                    onClick={() => onChange(b)}
                    className={`h-full max-w-[88px] flex-shrink-0 select-none truncate px-1.5 text-xs transition-colors ${
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
                      className={`h-full px-1 text-[10px] transition-colors ${
                        favorite
                          ? "text-[var(--accent-blue)]"
                          : "text-[var(--text-dim)] hover:text-[var(--accent-blue)]"
                      }`}
                    >
                      <Star size={9} fill={favorite ? "currentColor" : "none"} />
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
        </RootCarousel>
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
