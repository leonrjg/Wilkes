import React, { useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Folder } from "react-feather";
import type { FileEntry } from "../lib/types";
import { pathIsWithinRoot, pathsEqual } from "../lib/configuredRoots";

export const FILE_TREE_DRAG_TYPE = "application/x-wilkes-file-path";

export interface FileTreeDragProps {
  draggable: boolean;
  onDragStart: (event: React.DragEvent<HTMLButtonElement>) => void;
  onDrag: (event: React.DragEvent<HTMLButtonElement>) => void;
  onDragEnd: (event: React.DragEvent<HTMLButtonElement>) => void;
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
 * Folder identity remains the real filesystem path; no shadow IDs are made. */
export function buildFileTree(root: string, files: FileEntry[], directories: string[] = []): FileFolder {
  const tree: MutableFileFolder = {
    path: root,
    name: baseName(root),
    childMap: new Map(),
    files: [],
  };

  const ensureFolder = (path: string): MutableFileFolder => {
    const segments = relativeSegments(root, path);
    if (segments === null) {
      throw new Error(`File-list entry is outside its root: ${path} is not under ${root}`);
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
      throw new Error(`File-list entry is outside its root: ${file.path} is not under ${root}`);
    }
    if (segments.length < 2) {
      tree.files.push(file);
      continue;
    }
    const directoryPath = joinPath(root, segments.slice(0, -1));
    const folder = ensureFolder(directoryPath);
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
  const draggedPathRef = useRef<string | null>(null);
  const dropTargetRef = useRef<string | null>(null);
  const moveDispatchedRef = useRef(false);
  const dragCancelledRef = useRef(false);

  useEffect(() => {
    setCollapsed(new Set());
  }, [root]);

  const toggle = (path: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const setActiveDropTarget = (path: string | null) => {
    dropTargetRef.current = path;
    setDropTarget(path);
  };

  useEffect(() => {
    if (!draggedPath) return;
    const cancelDrag = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      dragCancelledRef.current = true;
      setActiveDropTarget(null);
    };
    window.addEventListener("keydown", cancelDrag, true);
    return () => window.removeEventListener("keydown", cancelDrag, true);
  }, [draggedPath]);

  const folderAtPoint = (clientX: number, clientY: number): string | null => {
    if (
      !Number.isFinite(clientX)
      || !Number.isFinite(clientY)
      || (clientX === 0 && clientY === 0)
      || typeof document.elementFromPoint !== "function"
    ) return null;
    const element = document.elementFromPoint(clientX, clientY);
    return element
      ?.closest<HTMLElement>("[data-file-tree-folder-path]")
      ?.dataset.fileTreeFolderPath ?? null;
  };

  const dispatchMove = (targetDirectory: string | null) => {
    const path = draggedPathRef.current;
    if (!path || !targetDirectory || moveDispatchedRef.current) return;
    if (pathsEqual(parentPath(path), targetDirectory)) return;
    moveDispatchedRef.current = true;
    void onMove(path, targetDirectory);
  };

  const finishDrag = () => {
    draggedPathRef.current = null;
    setDraggedPath(null);
    setActiveDropTarget(null);
  };

  const dragProps = (entry: FileEntry): FileTreeDragProps => ({
    draggable: movable,
    onDragStart: (event) => {
      draggedPathRef.current = entry.path;
      moveDispatchedRef.current = false;
      dragCancelledRef.current = false;
      setDraggedPath(entry.path);
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData(FILE_TREE_DRAG_TYPE, entry.path);
      event.dataTransfer.setData("text/plain", entry.path);
    },
    onDrag: (event) => {
      const target = folderAtPoint(event.clientX, event.clientY);
      if (target !== dropTargetRef.current) setActiveDropTarget(target);
    },
    onDragEnd: (event) => {
      // Some desktop webviews consume the DOM `drop` event. Resolve the folder
      // under the pointer once more, falling back to the last folder that was
      // visibly highlighted when the webview omits drag-end coordinates.
      if (!moveDispatchedRef.current && !dragCancelledRef.current) {
        dispatchMove(
          folderAtPoint(event.clientX, event.clientY) ?? dropTargetRef.current,
        );
      }
      finishDrag();
    },
  });

  // Drag-and-drop for one folder, applied to the folder's own tree item and, for
  // the root — which has no row of its own — to the tree container.
  const dropHandlers = (folderPath: string) => movable ? {
    onDragEnter: (event: React.DragEvent<HTMLElement>) => {
      if (!draggedPathRef.current) return;
      event.preventDefault();
      event.stopPropagation();
      setActiveDropTarget(folderPath);
      // Revealing a closed destination on hover makes its eventual parent
      // unambiguous and lets the user continue into a deeper folder.
      setCollapsed((current) => {
        if (!current.has(folderPath)) return current;
        const next = new Set(current);
        next.delete(folderPath);
        return next;
      });
    },
    onDragOver: (event: React.DragEvent<HTMLElement>) => {
      if (!draggedPathRef.current) return;
      event.preventDefault();
      event.stopPropagation();
      event.dataTransfer.dropEffect = pathsEqual(
        parentPath(draggedPathRef.current),
        folderPath,
      ) ? "none" : "move";
      if (dropTargetRef.current !== folderPath) setActiveDropTarget(folderPath);
    },
    onDragLeave: (event: React.DragEvent<HTMLElement>) => {
      event.stopPropagation();
      if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
        if (dropTargetRef.current === folderPath) setActiveDropTarget(null);
      }
    },
    onDrop: (event: React.DragEvent<HTMLElement>) => {
      if (!draggedPathRef.current) return;
      event.preventDefault();
      event.stopPropagation();
      dispatchMove(folderPath);
      finishDrag();
    },
  } : {};

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
        {...dropHandlers(folder.path)}
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
      data-file-tree-folder-path={tree.path}
      className={`min-h-full py-0.5 ${
        rootIsTarget && !rootAlreadyHere
          ? "rounded ring-2 ring-inset ring-[var(--accent-blue)]"
          : ""
      }`}
      {...dropHandlers(tree.path)}
    >
      {renderChildren(tree, 0)}
    </ul>
  );
}
