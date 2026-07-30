import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, Folder } from "react-feather";

interface Props {
  roots: string[];
  selected: string;
  onSelect: (path: string) => void;
  loadChildren: (path: string) => Promise<string[]>;
  /** Overrides the displayed name for specific paths (keyed by path). */
  labels?: Record<string, string>;
}

function baseName(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  return trimmed.split(/[/\\]/).pop() || path;
}

function normalize(path: string): string {
  return path.replace(/[/\\]+$/, "");
}

/** The containing directory of `path` (empty when it has no parent segment). */
export function parentPath(path: string): string {
  return normalize(path).replace(/[/\\][^/\\]*$/, "");
}

/** True when `ancestor` is a strict parent (at any depth) of `path`. */
export function isStrictAncestor(ancestor: string, path: string): boolean {
  const a = normalize(ancestor);
  const p = normalize(path);
  return p !== a && (p.startsWith(`${a}/`) || p.startsWith(`${a}\\`));
}

function DirectoryNode({
  path,
  depth,
  selected,
  onSelect,
  loadChildren,
  allRoots,
  labels,
}: {
  path: string;
  depth: number;
  selected: string;
  onSelect: (path: string) => void;
  loadChildren: (path: string) => Promise<string[]>;
  allRoots: string[];
  labels?: Record<string, string>;
}) {
  const label = labels?.[path] ?? baseName(path);
  // Auto-expand when another root lives inside this one, so the nested root is
  // revealed in place instead of being duplicated at the top level.
  const autoExpand = allRoots.some((root) => isStrictAncestor(path, root));
  const [expanded, setExpanded] = useState(autoExpand);
  const [children, setChildren] = useState<string[] | null>(null);
  const [error, setError] = useState(false);

  const load = async () => {
    setError(false);
    try {
      setChildren(await loadChildren(path));
    } catch {
      setChildren([]);
      setError(true);
    }
  };

  useEffect(() => {
    if (autoExpand && children === null) load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoExpand]);

  const toggle = async () => {
    if (expanded) {
      setExpanded(false);
      return;
    }
    setExpanded(true);
    if (children !== null) return;
    await load();
  };

  return (
    <li role="treeitem" aria-expanded={expanded} aria-selected={selected === path}>
      <div
        className={`flex h-8 items-center rounded text-sm ${
          selected === path
            ? "bg-[var(--accent-blue)] text-white"
            : "text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
        }`}
        style={{ paddingLeft: `${depth * 16 + 4}px` }}
      >
        <button
          type="button"
          aria-label={`${expanded ? "Collapse" : "Expand"} ${label}`}
          onClick={toggle}
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded hover:bg-black/10"
        >
          {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
        </button>
        <button
          type="button"
          onClick={() => onSelect(path)}
          title={path}
          className="flex min-w-0 flex-1 items-center gap-2 self-stretch text-left"
        >
          <Folder size={15} className="shrink-0" />
          <span className="truncate">{label}</span>
        </button>
      </div>
      {expanded && (
        <ul role="group">
          {children === null && (
            <li className="py-1 text-xs text-[var(--text-muted)]" style={{ paddingLeft: `${(depth + 2) * 16}px` }}>
              Loading…
            </li>
          )}
          {error && (
            <li className="py-1 text-xs text-[var(--text-muted)]" style={{ paddingLeft: `${(depth + 2) * 16}px` }}>
              Folder can’t be read
            </li>
          )}
          {children?.map((child) => (
            <DirectoryNode
              key={child}
              path={child}
              depth={depth + 1}
              selected={selected}
              onSelect={onSelect}
              loadChildren={loadChildren}
              allRoots={allRoots}
              labels={labels}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

export function DirectoryTree({ roots, selected, onSelect, loadChildren, labels }: Props) {
  // A root nested inside another root is reached by expanding its ancestor, so
  // only surface roots that aren't descendants of any other root.
  const topRoots = roots.filter(
    (root) => !roots.some((other) => isStrictAncestor(other, root)),
  );

  return (
    <div className="mb-3 max-h-72 overflow-auto rounded border border-[var(--border-main)] bg-[var(--bg-active)] p-1">
      <ul role="tree" aria-label="Destination directory">
        {topRoots.map((root) => (
          <DirectoryNode
            key={root}
            path={root}
            depth={0}
            selected={selected}
            onSelect={onSelect}
            loadChildren={loadChildren}
            allRoots={roots}
            labels={labels}
          />
        ))}
      </ul>
    </div>
  );
}
