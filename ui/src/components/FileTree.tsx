import React, { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Folder, FolderPlus } from "react-feather";
import type { FileEntry } from "../lib/types";
import { pathIsWithinRoot, pathsEqual } from "../lib/configuredRoots";
import { useFileDragStore } from "../stores/useFileDragStore";
import { ContextMenu, useContextMenu } from "./ContextMenu";

export type FileTreeDragProps = Pick<React.ButtonHTMLAttributes<HTMLButtonElement>,
  "draggable" | "onDragStart" | "onPointerDown" | "onClickCapture" | "style"
>;

/** The tree's part in a drag the desktop shell owns: files coming in from
 *  outside the application, which arrive as positions rather than as pointer
 *  events. They land by the same hit test, highlight and spring-loaded folders
 *  as a row dragged within the tree, so a file dropped from the file manager
 *  goes where a file dragged from the row below it would. */
export interface FileTreeHandle {
  /** Track the drag at a viewport point and highlight the folder under it.
   *  Returns that folder so the drop routes to the answer that was shown, or
   *  null when the point is over nothing this tree offers as a destination. */
  externalDragOver(x: number, y: number): string | null;
  /** The drag has left the window, or has been dropped: forget it. */
  externalDragEnd(): void;
}

/** The scroll container under a viewport point, if there is one. A drag that
 *  rests near its edge scrolls it: the sidebar to reach a folder below the
 *  fold, the root strip to reach a root off the side of the window. One rule
 *  for both, since resting near an edge means the same thing in either. */
function scrollerAtPoint(x: number, y: number): HTMLElement | null {
  let element: HTMLElement | null = document.elementFromPoint(x, y) as HTMLElement | null;
  while (element) {
    const style = getComputedStyle(element);
    if (/(auto|scroll)/.test(style.overflowY) && element.scrollHeight > element.clientHeight) {
      return element;
    }
    if (/(auto|scroll)/.test(style.overflowX) && element.scrollWidth > element.clientWidth) {
      return element;
    }
    element = element.parentElement;
  }
  return null;
}

/** A drag hovering over the tree, however it started. */
interface HoverSession {
  /** Move the drag to a new viewport point; returns the folder now shown as
   *  its destination. */
  update(x: number, y: number): string | null;
  /** The folder shown as the destination as of the last hit test. */
  target(): string | null;
  stop(): void;
}

interface FileFolder {
  path: string;
  name: string;
  folders: FileFolder[];
  files: FileEntry[];
}

interface MutableFileFolder extends Omit<FileFolder, "folders"> {
  childMap: Map<string, MutableFileFolder>;
}

interface Props {
  root: string;
  files: FileEntry[];
  directories?: string[];
  movable: boolean;
  expandAll?: boolean;
  renderFile: (entry: FileEntry, drag: FileTreeDragProps) => React.ReactNode;
  onMove: (path: string, targetDirectory: string) => Promise<void>;
  /** Create a folder named `name` inside `directory`. Absent where the tree may
   *  not be rearranged, and then the tree offers no menu of its own. */
  onCreateFolder?: (directory: string, name: string) => Promise<void>;
}

function normalized(path: string): string {
  let value = path.replace(/\\/g, "/");
  while (value.length > 1 && value.endsWith("/") && !/^[A-Za-z]:\/$/.test(value)) {
    value = value.slice(0, -1);
  }
  return value;
}

function baseName(path: string): string {
  const value = normalized(path);
  return value.split("/").pop() || value;
}

function joinPath(root: string, segments: string[]): string {
  if (segments.length === 0) return root;
  const separator = root.includes("\\") && !root.includes("/") ? "\\" : "/";
  return root.replace(/[/\\]+$/, "") + separator + segments.join(separator);
}

function relativeSegments(root: string, path: string): string[] | null {
  if (!pathIsWithinRoot(path, root)) return null;
  if (pathsEqual(path, root)) return [];
  const rootPath = normalized(root);
  const filePath = normalized(path);
  return filePath.slice(rootPath.length).replace(/^\/+/, "").split("/").filter(Boolean);
}

/** Build the visible hierarchy from the authoritative recursive file list.
 * Folder identity remains the real filesystem path; no shadow IDs are made.
 *
 * Entries outside the root are skipped rather than rejected: switching roots
 * re-renders the tree with the new root before the file list for it has
 * arrived, so the previous root's entries — including whatever the reader
 * still holds open — are legitimately present for a frame. Throwing there
 * blanked the application. They are logged, not silently dropped. */
export function buildFileTree(root: string, files: FileEntry[], directories: string[] = []): FileFolder {
  const outsideRoot = (path: string) => {
    console.debug(`File-tree entry is outside its root, skipped: ${path} is not under ${root}`);
  };
  const tree: MutableFileFolder = {
    path: root,
    name: baseName(root),
    childMap: new Map(),
    files: [],
  };

  const ensureFolder = (path: string): MutableFileFolder | null => {
    const segments = relativeSegments(root, path);
    if (segments === null) {
      outsideRoot(path);
      return null;
    }
    let folder = tree;
    segments.forEach((segment, index) => {
      let child = folder.childMap.get(segment);
      if (!child) {
        child = {
          path: joinPath(root, segments.slice(0, index + 1)),
          name: segment,
          childMap: new Map(),
          files: [],
        };
        folder.childMap.set(segment, child);
      }
      folder = child;
    });
    return folder;
  };

  for (const directory of directories) ensureFolder(directory);

  for (const file of files) {
    const segments = relativeSegments(root, file.path);
    if (segments === null) {
      outsideRoot(file.path);
      continue;
    }
    if (segments.length < 2) {
      tree.files.push(file);
      continue;
    }
    const directoryPath = joinPath(root, segments.slice(0, -1));
    const folder = ensureFolder(directoryPath);
    if (!folder) continue;
    folder.files.push(file);
  }

  const freeze = (folder: MutableFileFolder): FileFolder => ({
    path: folder.path,
    name: folder.name,
    files: folder.files,
    folders: [...folder.childMap.values()]
      .sort((left, right) => left.name.localeCompare(right.name, undefined, { sensitivity: "base" }))
      .map(freeze),
  });
  return freeze(tree);
}

function parentPath(path: string): string {
  return path.replace(/[/\\][^/\\]*$/, "");
}

/** Where `folder` sits relative to `root`, as the `/`-separated path the import
 *  command takes for a destination beneath a root. Null when the folder is the
 *  root itself, which the command expresses by naming no folder at all.
 *
 *  Throws when the folder is not under the root. The tree only ever names
 *  folders under its own root, so a caller holding one that is not is holding
 *  an answer from a tree that has since changed; importing into the root
 *  regardless would silently put the files somewhere nobody pointed at. */
export function relativeFolderPath(root: string, folder: string): string | null {
  if (pathsEqual(folder, root)) return null;
  const segments = relativeSegments(root, folder);
  if (segments === null || segments.length === 0) {
    throw new Error(`${folder} is not a folder under ${root}`);
  }
  return segments.join("/");
}

function FileTree({
  root,
  files,
  directories = [],
  movable,
  expandAll = false,
  renderFile,
  onMove,
  onCreateFolder,
}: Props, ref: React.ForwardedRef<FileTreeHandle>) {
  const tree = useMemo(
    () => buildFileTree(root, files, directories),
    [root, files, directories],
  );
  // A folder is open unless the user explicitly closes it. New folders from a
  // refresh therefore inherit the required default-open behaviour without
  // erasing the user's choices for folders that were already present.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // The drag's own state is shared, because the root strip draws it too.
  const dropTarget = useFileDragStore((state) => state.target);
  const draggedPath = useFileDragStore((state) => state.path);
  const setDropTarget = useFileDragStore((state) => state.setTarget);
  const setDraggedPath = useFileDragStore((state) => state.setPath);
  const { menu, openMenu, closeMenu } = useContextMenu<string>();
  const treeRef = useRef<HTMLUListElement>(null);
  const cancelDragRef = useRef<(() => void) | null>(null);
  const externalHoverRef = useRef<HoverSession | null>(null);
  const suppressClickRef = useRef(false);

  useEffect(() => {
    setCollapsed(new Set());
  }, [root]);

  // A gesture belongs to the tree and permissions it started with.
  useEffect(() => () => {
    cancelDragRef.current?.();
    externalHoverRef.current?.stop();
    externalHoverRef.current = null;
  }, [root, movable]);

  const toggle = (path: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  /** The directory under a viewport point, or null where there is none.
   *
   *  `acceptRoots` says whether the roots in the strip at the top of the window
   *  count. A move may go to any of them; an import may not, because the
   *  backend admits an import only into the root that is open. Rather than let
   *  the two disagree about what the highlight meant, the drag that cannot use
   *  a root does not see one. */
  const folderAtPoint = (x: number, y: number, acceptRoots = true): string | null => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    const element = document.elementFromPoint(x, y);
    if (!element) return null;
    if (acceptRoots) {
      const strip = element.closest<HTMLElement>("[data-file-drop-root-path]");
      if (strip) return strip.dataset.fileDropRootPath ?? null;
    }
    const container = treeRef.current;
    if (!container?.contains(element)) return null;
    const folder = element.closest<HTMLElement>("[data-file-tree-folder-path]");
    if (folder) return folder.dataset.fileTreeFolderPath ?? null;
    // Root-level file rows are not implicit targets for the undrawn root.
    return element === container ? root : null;
  };

  /** One hovering drag. Its frame loop scrolls the sidebar while the drag rests
   *  near an edge, re-runs the hit test so the highlight follows a layout that
   *  is moving underneath it, and springs a collapsed folder open when the drag
   *  lingers on it. Both drags run this one: a row dragged within the tree, and
   *  a file dragged in from outside the application. */
  const startHover = (startX: number, startY: number, acceptRoots = true): HoverSession => {
    let x = startX;
    let y = startY;
    let target: string | null = null;
    let hoverSince = performance.now();
    let previousFrame = performance.now();
    let frame = 0;

    const updateTarget = () => {
      const next = folderAtPoint(x, y, acceptRoots);
      if (next !== target) {
        target = next;
        hoverSince = performance.now();
        setDropTarget(next);
      }
    };
    const tick = (now: number) => {
      const seconds = Math.min(now - previousFrame, 32) / 1000;
      previousFrame = now;
      // Scroll whichever existing scroll container the drag is over — the
      // sidebar's, or the root strip's once the drag has gone up to it — then
      // hit-test the new layout even when the pointer has not moved.
      const scroller = scrollerAtPoint(x, y);
      if (scroller) {
        const rect = scroller.getBoundingClientRect();
        if (scroller.scrollHeight > scroller.clientHeight) {
          const edge = Math.min(32, rect.height / 3);
          const direction = y < rect.top + edge ? -1 : y > rect.bottom - edge ? 1 : 0;
          scroller.scrollTop += direction * 360 * seconds;
        }
        if (scroller.scrollWidth > scroller.clientWidth) {
          const edge = Math.min(32, rect.width / 3);
          const direction = x < rect.left + edge ? -1 : x > rect.right - edge ? 1 : 0;
          scroller.scrollLeft += direction * 360 * seconds;
        }
      }
      updateTarget();
      if (target && now - hoverSince >= 600) expand(target);
      frame = requestAnimationFrame(tick);
    };

    updateTarget();
    frame = requestAnimationFrame(tick);
    return {
      update: (nextX, nextY) => {
        x = nextX;
        y = nextY;
        updateTarget();
        return target;
      },
      target: () => target,
      stop: () => {
        cancelAnimationFrame(frame);
        setDropTarget(null);
      },
    };
  };

  // A drag the desktop shell owns is reported by position, so it has no gesture
  // of its own to end it: the shell says when it leaves or lands.
  useImperativeHandle(ref, (): FileTreeHandle => ({
    externalDragOver: (x, y) => {
      // `movable` is the same permission an in-tree move needs; a tree that
      // cannot be rearranged must not offer itself as a destination either.
      if (!movable) return null;
      // An import is admitted only into the root that is open, so this drag is
      // offered the tree's folders and not the strip's other roots.
      if (!externalHoverRef.current) externalHoverRef.current = startHover(x, y, false);
      return externalHoverRef.current.update(x, y);
    },
    externalDragEnd: () => {
      externalHoverRef.current?.stop();
      externalHoverRef.current = null;
    },
  }));

  const expand = (path: string) => {
    setCollapsed((current) => {
      if (!current.has(path)) return current;
      const next = new Set(current);
      next.delete(path);
      return next;
    });
  };

  /** A right-click that no row answered belongs to the folder it landed in:
   *  the space around a folder's entries is that folder, and the space around
   *  the tree's own is the root. It is the hit test a drag uses, asked of the
   *  element the click already names rather than of a point.
   *
   *  A file row opens its own menu and prevents the default, which is what
   *  says the click has been answered; the tree does not answer it twice. */
  const openTreeMenu = (event: React.MouseEvent) => {
    if (!onCreateFolder || event.defaultPrevented) return;
    const element = event.target instanceof Element ? event.target : null;
    const folder = element?.closest<HTMLElement>("[data-file-tree-folder-path]");
    const directory = folder?.dataset.fileTreeFolderPath ?? tree.path;
    openMenu({
      event,
      target: directory,
      items: [{
        id: "new-folder",
        label: `New folder in ${baseName(directory)}`,
        icon: FolderPlus,
        inlineInput: {
          placeholder: "Folder name…",
          submitLabel: "Create",
          submit: async (name) => {
            await onCreateFolder(directory, name);
            // The folder it was created in has to be open for it to be seen.
            expand(directory);
          },
        },
      }],
    });
  };

  const dragProps = (entry: FileEntry): FileTreeDragProps => ({
    draggable: false,
    // Suppress the browser's native drag session, including drags of children.
    onDragStart: (event) => event.preventDefault(),
    style: movable ? { touchAction: "none" } : undefined,
    onClickCapture: (event) => {
      if (!suppressClickRef.current || event.detail === 0) return;
      suppressClickRef.current = false;
      event.preventDefault();
      event.stopPropagation();
    },
    onPointerDown: movable ? (event) => {
      if (event.button !== 0 || !event.isPrimary || cancelDragRef.current) return;
      suppressClickRef.current = false;
      const source = event.currentTarget;
      // Tags and detail disclosures keep their own click behaviour.
      if (event.target instanceof Element
        && event.target.closest('[role="button"], a, input, select, textarea')
        && event.target !== source) return;

      const pointerId = event.pointerId;
      const startX = event.clientX;
      const startY = event.clientY;
      let x = startX;
      let y = startY;
      let active = false;
      let hover: HoverSession | null = null;

      const finish = () => {
        cancelDragRef.current = null;
        hover?.stop();
        hover = null;
        window.removeEventListener("pointermove", move, true);
        window.removeEventListener("pointerup", release, true);
        window.removeEventListener("pointercancel", cancelPointer, true);
        window.removeEventListener("keydown", keydown, true);
        window.removeEventListener("blur", finish);
        source.removeEventListener("lostpointercapture", cancelPointer);
        if (source.hasPointerCapture(pointerId)) source.releasePointerCapture(pointerId);
        setDraggedPath(null);
        setDropTarget(null);
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
        y = e.clientY;
        if (!active && Math.hypot(x - startX, y - startY) < 6) return;
        e.preventDefault();
        if (!active) {
          active = true;
          suppressClickRef.current = true;
          setDraggedPath(entry.path);
          hover = startHover(x, y);
          return;
        }
        hover?.update(x, y);
      };
      const release = (e: PointerEvent) => {
        if (e.pointerId !== pointerId) return;
        const destination = folderAtPoint(e.clientX, e.clientY);
        // Never substitute a remembered hover for a release. A layout change
        // that has not yet been highlighted cancels rather than surprises.
        const accepted = active && destination !== null && destination === hover?.target()
          && !pathsEqual(parentPath(entry.path), destination);
        finish();
        if (accepted) void onMove(entry.path, destination);
      };

      source.setPointerCapture(pointerId);
      cancelDragRef.current = finish;
      window.addEventListener("pointermove", move, true);
      window.addEventListener("pointerup", release, true);
      window.addEventListener("pointercancel", cancelPointer, true);
      window.addEventListener("keydown", keydown, true);
      window.addEventListener("blur", finish);
      source.addEventListener("lostpointercapture", cancelPointer);
    } : undefined,
  });

  const renderChildren = (folder: FileFolder, depth: number): React.ReactNode => (
    <>
      {folder.folders.map((child) => renderFolder(child, depth))}
      {folder.files.map((entry) => (
        <li
          key={entry.path}
          role="treeitem"
          className={draggedPath === entry.path ? "opacity-45" : ""}
          style={{ paddingLeft: `${depth * 14}px` }}
        >
          {renderFile(entry, dragProps(entry))}
        </li>
      ))}
    </>
  );

  const renderFolder = (folder: FileFolder, depth: number): React.ReactNode => {
    const open = expandAll || !collapsed.has(folder.path);
    const activeTarget = dropTarget === folder.path;
    const alreadyHere = activeTarget && !!draggedPath && pathsEqual(
      parentPath(draggedPath),
      folder.path,
    );
    return (
      <li
        key={folder.path}
        role="treeitem"
        aria-expanded={open}
        data-file-tree-folder-path={folder.path}
      >
        <button
          type="button"
          aria-label={`${open ? "Collapse" : "Expand"} folder ${folder.name}`}
          title={folder.path}
          onClick={() => toggle(folder.path)}
          className={`flex h-8 w-full items-center gap-1.5 rounded pr-2 text-left text-xs transition-colors ${
            activeTarget && !alreadyHere
              ? "bg-[var(--accent-blue)] text-white shadow-sm ring-2 ring-inset ring-white/40"
              : activeTarget
                ? "bg-[var(--bg-active)] text-[var(--text-muted)] ring-1 ring-inset ring-[var(--border-strong)]"
                : "text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
          }`}
          style={{ paddingLeft: `${depth * 14 + 5}px` }}
        >
          {open ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />}
          <Folder
            size={14}
            className={`shrink-0 ${activeTarget && !alreadyHere ? "text-white" : "text-[var(--accent-blue)]"}`}
            aria-hidden="true"
          />
          <span className="truncate">{folder.name}</span>
          {activeTarget && (
            <span className="ml-auto shrink-0 text-[10px] font-semibold uppercase tracking-wide">
              {alreadyHere ? "Already here" : "Drop here"}
            </span>
          )}
        </button>
        {open && <ul role="group">{renderChildren(folder, depth + 1)}</ul>}
      </li>
    );
  };

  // The root itself is not drawn: its name is already shown by the sidebar, and
  // a row for it would only add a level of indentation to everything below.
  // Its drop target is the tree container, so a file can still be moved out to
  // the top level by dropping it on the empty space around the entries.
  const rootIsTarget = dropTarget === tree.path;
  const rootAlreadyHere = rootIsTarget && !!draggedPath && pathsEqual(
    parentPath(draggedPath),
    tree.path,
  );
  return (
    <>
      <ul
        role="tree"
        aria-label="Files and folders"
        data-file-tree-root-path={tree.path}
        className={`min-h-full py-0.5 ${
          rootIsTarget && !rootAlreadyHere
            ? "rounded ring-2 ring-inset ring-[var(--accent-blue)]"
            : ""
        }`}
        ref={treeRef}
        onContextMenu={openTreeMenu}
      >
        {renderChildren(tree, 0)}
      </ul>
      <ContextMenu menu={menu} onClose={closeMenu} />
    </>
  );
}

export default forwardRef(FileTree);
