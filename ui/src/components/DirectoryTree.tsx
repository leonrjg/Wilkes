import { useState } from "react";
import { ChevronDown, ChevronRight, Folder } from "react-feather";

interface Props {
  roots: string[];
  selected: string;
  onSelect: (path: string) => void;
  loadChildren: (path: string) => Promise<string[]>;
}

function baseName(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  return trimmed.split(/[/\\]/).pop() || path;
}

function DirectoryNode({
  path,
  depth,
  selected,
  onSelect,
  loadChildren,
}: {
  path: string;
  depth: number;
  selected: string;
  onSelect: (path: string) => void;
  loadChildren: (path: string) => Promise<string[]>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [children, setChildren] = useState<string[] | null>(null);
  const [error, setError] = useState(false);

  const toggle = async () => {
    if (expanded) {
      setExpanded(false);
      return;
    }
    setExpanded(true);
    if (children !== null) return;
    setError(false);
    try {
      setChildren(await loadChildren(path));
    } catch {
      setChildren([]);
      setError(true);
    }
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
          aria-label={`${expanded ? "Collapse" : "Expand"} ${baseName(path)}`}
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
          <span className="truncate">{baseName(path)}</span>
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
            />
          ))}
        </ul>
      )}
    </li>
  );
}

export function DirectoryTree({ roots, selected, onSelect, loadChildren }: Props) {
  return (
    <div className="mb-3 max-h-72 overflow-auto rounded border border-[var(--border-main)] bg-[var(--bg-active)] p-1">
      <ul role="tree" aria-label="Destination directory">
        {roots.map((root) => (
          <DirectoryNode
            key={root}
            path={root}
            depth={0}
            selected={selected}
            onSelect={onSelect}
            loadChildren={loadChildren}
          />
        ))}
      </ul>
    </div>
  );
}
