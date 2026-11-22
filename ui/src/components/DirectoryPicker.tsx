import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { confirmDialog } from "../lib/utils/dialog";
import { ChevronLeft, ChevronRight, Folder, FolderPlus, Star, X } from "react-feather";
import { useToasts } from "./Toast";
import { ContextMenu, useContextMenu } from "./ContextMenu";
import { api, isTauri, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import { buildFileContextMenuItems, type ContextMenuTarget } from "../lib/fileActions";
import { useFileDragStore } from "../stores/useFileDragStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useTidyStore } from "../stores/useTidyStore";
import { useActiveWorkspaceReadOnly } from "../stores/useWorkspaceStore";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { DirectoryTree, isStrictAncestor, parentPath } from "./DirectoryTree";
import { configuredLibraryRoots, pathsEqual } from "../lib/configuredRoots";
import type { RootCoverage } from "../lib/types";

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
  /** Called with the whole strip in its new order once a root is moved. */
  onReorderRoots?: (order: string[]) => void;
}

function shortPath(p: string): string {
  const home = p.match(/^\/Users\/[^/]+/) ?? p.match(/^\/home\/[^/]+/);
  if (home) return "~" + p.slice(home[0].length);
  return p;
}

function baseName(p: string): string {
  return p.replace(/[/\\]+$/, "").split(/[/\\]/).pop() || p;
}

/**
 * What a root's coverage says, in one sentence.
 *
 * The counts are spelled out because "not indexed" and "indexed except for the
 * forty files added last week" call for the same decision but not the same
 * urgency, and the strip only has room for a dot.
 */
export function describeCoverage(coverage: RootCoverage): string {
  if (coverage.complete) {
    return coverage.indexable === 1
      ? "Indexed — its 1 document is in the semantic index"
      : `Indexed — all ${coverage.indexable} documents are in the semantic index`;
  }
  return `Not fully indexed — ${coverage.covered} of ${coverage.indexable} documents are in the semantic index`;
}

const ROOT_SCROLL_EDGE_TOLERANCE = 2;
const ROOT_DRAG_THRESHOLD = 6;

/**
 * Where a root moved within its group would land, as the strip's whole new
 * order, or null when the move leaves the order as it is.
 *
 * Favorites are always drawn before the other roots, so a root moves only
 * among the roots of its own kind: `slot` counts positions within that group,
 * 0 being before its first member and `members.length` after its last.
 */
export function rootOrderAfterMove(
  order: string[],
  favorites: string[],
  path: string,
  slot: number,
): string[] | null {
  const favorite = favorites.includes(path);
  const members = order.filter((root) => favorites.includes(root) === favorite);
  const others = order.filter((root) => favorites.includes(root) !== favorite);
  const from = members.indexOf(path);
  if (from === -1) throw new Error(`${path} is not a root of the strip`);
  const clamped = Math.max(0, Math.min(slot, members.length));
  const to = clamped > from ? clamped - 1 : clamped;
  if (to === from) return null;
  const moved = members.filter((root) => root !== path);
  moved.splice(to, 0, path);
  return favorite ? [...moved, ...others] : [...others, ...moved];
}

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
  // A drag holds the pointer, so the arrows cannot be pressed while one is in
  // progress. They sit over the strip's own edges, which is exactly where a
  // drag has to rest to scroll it, so for the length of the drag they step out
  // of the way of the hit test rather than swallowing it.
  const dragging = useFileDragStore((state) => state.path !== null);

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
          className={`${dragging ? "pointer-events-none " : ""}absolute inset-y-0 left-0 z-10 flex w-6 items-center justify-center rounded-r bg-[var(--bg-app)]/95 text-[var(--text-muted)] shadow-[7px_0_18px_-3px_rgba(0,0,0,0.24)] hover:text-[var(--text-main)]`}
        >
          <ChevronLeft size={13} />
        </button>
      )}
      {canScrollRight && (
        <button
          type="button"
          aria-label="Scroll roots right"
          onClick={() => scrollPage(1)}
          className={`${dragging ? "pointer-events-none " : ""}absolute inset-y-0 right-0 z-10 flex w-6 items-center justify-center rounded-l bg-[var(--bg-app)]/95 text-[var(--text-muted)] shadow-[-7px_0_18px_-3px_rgba(0,0,0,0.24)] hover:text-[var(--text-main)]`}
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
  onReorderRoots,
}: Props) {
  const { addToast } = useToasts();
  const { menu, openMenu, closeMenu } = useContextMenu<ContextMenuTarget>();
  // A root is a destination for a file being dragged out of the sidebar. The
  // tree owns that gesture and says where it is pointing; the strip's part is
  // to offer its roots to the hit test and to look like what was chosen.
  const dragTarget = useFileDragStore((state) => state.target);
  const draggedPath = useFileDragStore((state) => state.path);
  const settings = useSettingsStore((s) => s.settings);
  const readOnly = useActiveWorkspaceReadOnly();
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

  // Moving a root is a write to the workspace manifest, like every other
  // change to the strip, so a workspace another application owns does not
  // offer it.
  const reorderable = !!onReorderRoots && !readOnly;
  const chipRefs = useRef(new Map<string, HTMLDivElement>());
  const cancelRootDragRef = useRef<(() => void) | null>(null);
  const suppressRootClickRef = useRef(false);
  const [rootDrag, setRootDrag] = useState<{ path: string; slot: number | null } | null>(null);

  /** The members of `path`'s group, in strip order, with where each is drawn. */
  const groupOf = (path: string) => {
    const favorite = favorites.includes(path);
    return displayDirs.filter((root) => favorites.includes(root) === favorite);
  };

  /** The slot within `path`'s group a pointer at `x` points at: how many of
   *  the group's roots are drawn with their middle to the left of it. */
  const slotAt = (path: string, x: number): number | null => {
    if (!Number.isFinite(x)) return null;
    let slot = 0;
    for (const root of groupOf(path)) {
      const element = chipRefs.current.get(root);
      if (!element) return null;
      const rect = element.getBoundingClientRect();
      if (rect.left + rect.width / 2 < x) slot += 1;
    }
    return slot;
  };

  // A drag begun on a strip that has since changed — a root added, removed or
  // renamed by another window — is measuring roots that are no longer the
  // ones drawn, so it is abandoned rather than allowed to land.
  const rootsKey = displayDirs.join("\0");
  useEffect(() => {
    cancelRootDragRef.current?.();
  }, [rootsKey, reorderable]);
  useEffect(() => () => cancelRootDragRef.current?.(), []);

  const startRootDrag = (event: React.PointerEvent<HTMLDivElement>, path: string) => {
    if (!reorderable || event.button !== 0 || !event.isPrimary || cancelRootDragRef.current) return;
    suppressRootClickRef.current = false;
    // Only the root's name starts a drag; its remove and favorite buttons
    // keep their own clicks.
    if (!(event.target instanceof Element) || !event.target.closest("[data-root-name]")) return;

    const source = event.currentTarget;
    const scroller = source.parentElement;
    const pointerId = event.pointerId;
    const startX = event.clientX;
    const startY = event.clientY;
    let x = startX;
    let active = false;
    let slot: number | null = null;
    let frame = 0;
    let previousFrame = performance.now();

    const show = () => {
      const next = slotAt(path, x);
      const order = next === null ? null : rootOrderAfterMove(displayDirs, favorites, path, next);
      slot = order ? next : null;
      setRootDrag({ path, slot });
    };
    // Scrolls the strip while the drag rests near one of its edges, and
    // re-measures the slot under a pointer that has not moved while the roots
    // slide beneath it.
    const tick = (now: number) => {
      const seconds = Math.min(now - previousFrame, 32) / 1000;
      previousFrame = now;
      if (scroller && scroller.scrollWidth > scroller.clientWidth) {
        const rect = scroller.getBoundingClientRect();
        const edge = Math.min(32, rect.width / 3);
        const direction = x < rect.left + edge ? -1 : x > rect.right - edge ? 1 : 0;
        scroller.scrollLeft += direction * 360 * seconds;
      }
      show();
      frame = requestAnimationFrame(tick);
    };

    const finish = () => {
      cancelRootDragRef.current = null;
      cancelAnimationFrame(frame);
      window.removeEventListener("pointermove", move, true);
      window.removeEventListener("pointerup", release, true);
      window.removeEventListener("pointercancel", cancelPointer, true);
      window.removeEventListener("keydown", keydown, true);
      window.removeEventListener("blur", finish);
      source.removeEventListener("lostpointercapture", cancelPointer);
      if (source.hasPointerCapture(pointerId)) source.releasePointerCapture(pointerId);
      setRootDrag(null);
    };
    const cancelPointer = (e: PointerEvent) => {
      if (e.pointerId === pointerId) finish();
    };
    const keydown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish();
      }
    };
    const move = (e: PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      if ((e.buttons & 1) === 0) { finish(); return; }
      x = e.clientX;
      if (!active && Math.hypot(x - startX, e.clientY - startY) < ROOT_DRAG_THRESHOLD) return;
      e.preventDefault();
      if (!active) {
        active = true;
        suppressRootClickRef.current = true;
        previousFrame = performance.now();
        frame = requestAnimationFrame(tick);
      }
      show();
    };
    const release = (e: PointerEvent) => {
      if (e.pointerId !== pointerId) return;
      // The release lands where it is, not where the last frame drew it: a
      // layout that moved without being redrawn cancels rather than surprises.
      const landed = active ? slotAt(path, e.clientX) : null;
      const order = landed !== null && landed === slot
        ? rootOrderAfterMove(displayDirs, favorites, path, landed)
        : null;
      finish();
      if (order) onReorderRoots?.(order);
    };

    source.setPointerCapture(pointerId);
    cancelRootDragRef.current = finish;
    window.addEventListener("pointermove", move, true);
    window.addEventListener("pointerup", release, true);
    window.addEventListener("pointercancel", cancelPointer, true);
    window.addEventListener("keydown", keydown, true);
    window.addEventListener("blur", finish);
    source.addEventListener("lostpointercapture", cancelPointer);
  };

  /** Alt+Left and Alt+Right move the focused root one place within its group. */
  const moveRootByKey = (event: React.KeyboardEvent<HTMLButtonElement>, path: string) => {
    if (!reorderable || !event.altKey || event.metaKey || event.ctrlKey || event.shiftKey) return;
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const members = groupOf(path);
    const from = members.indexOf(path);
    // The slot after the neighbour on the right is two places on.
    const order = rootOrderAfterMove(
      displayDirs,
      favorites,
      path,
      event.key === "ArrowLeft" ? from - 1 : from + 2,
    );
    if (!order) return;
    const button = event.currentTarget;
    onReorderRoots?.(order);
    // Moving a node in the document takes focus off it in some engines.
    requestAnimationFrame(() => button.focus());
  };

  // The strip is what knows which roots are on screen, so it is what asks how
  // much of each the index holds. Keyed on the set rather than the array, so
  // making an already-listed root active does not re-walk every directory.
  const coverage = useSemanticStore((state) => state.coverage);
  const refreshCoverage = useSemanticStore((state) => state.refreshCoverage);
  useEffect(() => {
    refreshCoverage(rootsKey ? rootsKey.split("\0") : []).catch(console.error);
  }, [rootsKey, refreshCoverage]);

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

  // Roots and the active directory live in the workspace manifest, which the
  // backend refuses to rewrite for a workspace another application owns. The
  // controls that would attempt it are withheld rather than left to fail.
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
        <Tooltip content={readOnly
          ? "This workspace's directories are managed by another application"
          : directory || "Choose directory"}
        >
          <button
            onClick={onPickDirectory}
            disabled={readOnly}
            className="h-full text-xs text-[var(--text-muted)] hover:text-[var(--text-main)] px-3 flex-shrink-0 flex items-center gap-1.5 disabled:opacity-40"
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
            const rootCoverage = coverage[b];
            const dropTarget = dragTarget !== null && pathsEqual(b, dragTarget);
            // Dropping a file where it already is moves nothing, so that root
            // says so rather than inviting the drop.
            const alreadyHere = dropTarget && !!draggedPath
              && pathsEqual(parentPath(draggedPath), b);
            const group = rootDrag ? groupOf(rootDrag.path) : [];
            const insertBefore = rootDrag?.slot != null && group[rootDrag.slot] === b;
            const insertAfter = rootDrag?.slot != null
              && rootDrag.slot === group.length
              && group[group.length - 1] === b;

            return (
              <div
                key={b}
                ref={(element) => {
                  if (element) chipRefs.current.set(b, element);
                  else chipRefs.current.delete(b);
                }}
                data-root-active={active ? "true" : undefined}
                data-root-dragging={rootDrag?.path === b ? "true" : undefined}
                data-root-insert={insertBefore ? "before" : insertAfter ? "after" : undefined}
                data-file-drop-root-path={b}
                data-file-drop-target={dropTarget ? "true" : undefined}
                style={reorderable ? { touchAction: "none" } : undefined}
                onDragStart={(event) => event.preventDefault()}
                onPointerDown={(event) => startRootDrag(event, b)}
                onClickCapture={(event) => {
                  if (!suppressRootClickRef.current || event.detail === 0) return;
                  suppressRootClickRef.current = false;
                  event.preventDefault();
                  event.stopPropagation();
                }}
                className={`group relative flex h-6 items-center rounded bg-[var(--bg-active)] transition-colors ${
                  rootDrag?.path === b ? "opacity-45 " : ""
                }${
                  dropTarget && !alreadyHere
                    ? "ring-2 ring-inset ring-[var(--accent-blue)]"
                    : dropTarget
                      ? "ring-1 ring-inset ring-[var(--border-strong)]"
                      : ""
                }`}
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
                      onRenameRequest: onRenameDirectory && !readOnly
                        ? (path) => setRenameTarget({ path, name: baseName(path) })
                        : undefined,
                      onTidyRequest: (path) => useTidyStore.getState().open(path),
                    }),
                  })}
              >
                {(insertBefore || insertAfter) && (
                  <span
                    aria-hidden="true"
                    className={`pointer-events-none absolute inset-y-0.5 w-0.5 rounded-full bg-[var(--accent-blue)] ${
                      insertBefore ? "left-0" : "right-0"
                    }`}
                  />
                )}
                {onForgetDirectory && !readOnly && (
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
                {rootCoverage && (
                  <Tooltip content={describeCoverage(rootCoverage)}>
                    <span
                      role="img"
                      aria-label={describeCoverage(rootCoverage)}
                      data-root-coverage={rootCoverage.complete ? "complete" : "incomplete"}
                      className={`ml-1 block h-1.5 w-1.5 flex-shrink-0 rounded-full ${
                        rootCoverage.complete
                          ? "bg-green-400"
                          : "border border-amber-400 bg-transparent"
                      }`}
                    />
                  </Tooltip>
                )}
                <Tooltip content={b} className="font-mono break-all">
                  <button
                    data-root-name
                    onClick={() => onChange(b)}
                    onKeyDown={(event) => moveRootByKey(event, b)}
                    aria-keyshortcuts={reorderable ? "Alt+ArrowLeft Alt+ArrowRight" : undefined}
                    className={`h-full max-w-[88px] flex-shrink-0 select-none truncate px-1.5 text-xs transition-colors ${
                      active
                        ? "text-[var(--text-main)] font-bold"
                        : "text-[var(--text-muted)] hover:text-[var(--text-main)]"
                    }`}
                  >
                    {shortPath(b).split("/").pop() || shortPath(b)}
                  </button>
                </Tooltip>
                {onFavoriteAdd && onFavoriteRemove && !readOnly && (
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
          {!readOnly && (
            <Tooltip content="New folder">
              <button
                onClick={openCreateDialog}
                aria-label="New folder"
                className="flex h-6 w-6 flex-shrink-0 items-center justify-center rounded bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <FolderPlus size={12} />
              </button>
            </Tooltip>
          )}
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
