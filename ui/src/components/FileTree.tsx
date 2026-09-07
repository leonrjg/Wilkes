import React, { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Folder } from "react-feather";
import type { FileEntry } from "../lib/types";
import { pathIsWithinRoot, pathsEqual } from "../lib/configuredRoots";

export type FileTreeDragProps = Pick<React.ButtonHTMLAttributes<HTMLButtonElement>,
  "draggable" | "onDragStart" | "onPointerDown" | "onClickCapture" | "style"
>;

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

export default function FileTree({
  root,
  files,
  directories = [],
  movable,
  expandAll = false,
  renderFile,
  onMove,
}: Props) {
  const tree = useMemo(
    () => buildFileTree(root, files, directories),
    [root, files, directories],
  );
  // A folder is open unless the user explicitly closes it. New folders from a
  // refresh therefore inherit the required default-open behaviour without
  // erasing the user's choices for folders that were already present.
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [draggedPath, setDraggedPath] = useState<string | null>(null);
  const treeRef = useRef<HTMLUListElement>(null);
  const cancelDragRef = useRef<(() => void) | null>(null);
  const suppressClickRef = useRef(false);

  useEffect(() => {
    setCollapsed(new Set());
  }, [root]);

  // A gesture belongs to the tree and permissions it started with.
  useEffect(() => () => cancelDragRef.current?.(), [root, movable]);

  const toggle = (path: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const folderAtPoint = (x: number, y: number): string | null => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    const element = document.elementFromPoint(x, y);
    const container = treeRef.current;
    if (!element || !container?.contains(element)) return null;
    const folder = element.closest<HTMLElement>("[data-file-tree-folder-path]");
    if (folder) return folder.dataset.fileTreeFolderPath ?? null;
    // Root-level file rows are not implicit targets for the undrawn root.
    return element === container ? root : null;
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
      let target: string | null = null;
      let hoverSince = performance.now();
      let frame = 0;
      let previousFrame = performance.now();

      const updateTarget = () => {
        const next = folderAtPoint(x, y);
        if (next !== target) {
          target = next;
          hoverSince = performance.now();
          setDropTarget(next);
        }
      };
      const finish = () => {
        cancelDragRef.current = null;
        cancelAnimationFrame(frame);
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
      const tick = (now: number) => {
        const seconds = Math.min(now - previousFrame, 32) / 1000;
        previousFrame = now;
        // Scroll the sidebar's existing scroll container, then hit-test the
        // new layout even when the pointer has not moved.
        let scroller: HTMLElement | null = treeRef.current;
        while (scroller) {
          if (/(auto|scroll)/.test(getComputedStyle(scroller).overflowY)
            && scroller.scrollHeight > scroller.clientHeight) {
            const rect = scroller.getBoundingClientRect();
            if (x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom) {
              const edge = Math.min(32, rect.height / 3);
              const direction = y < rect.top + edge ? -1 : y > rect.bottom - edge ? 1 : 0;
              scroller.scrollTop += direction * 360 * seconds;
            }
            break;
          }
          scroller = scroller.parentElement;
        }
        updateTarget();
        if (target && now - hoverSince >= 600) {
          const path = target;
          setCollapsed((current) => {
            if (!current.has(path)) return current;
            const next = new Set(current);
            next.delete(path);
            return next;
          });
        }
        frame = requestAnimationFrame(tick);
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
          previousFrame = performance.now();
          frame = requestAnimationFrame(tick);
        }
        updateTarget();
      };
      const release = (e: PointerEvent) => {
        if (e.pointerId !== pointerId) return;
        const destination = folderAtPoint(e.clientX, e.clientY);
        // Never substitute a remembered hover for a release. A layout change
        // that has not yet been highlighted cancels rather than surprises.
        const accepted = active && destination !== null && destination === target
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
    >
      {renderChildren(tree, 0)}
    </ul>
  );
}
