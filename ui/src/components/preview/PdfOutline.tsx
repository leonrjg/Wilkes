import { X } from "react-feather";
import type { PdfOutlineNode } from "./usePdfOutline";
import type { PdfDestination } from "./pdfDestinations";
import { Tooltip } from "./Tooltip";

interface Props {
  outline: PdfOutlineNode[];
  onNavigateToDestination: (dest: PdfDestination) => void;
  onOpenExternal: (url: string) => void;
  onClose: () => void;
}

function OutlineItems({
  nodes,
  depth,
  onNavigateToDestination,
  onOpenExternal,
}: {
  nodes: PdfOutlineNode[];
  depth: number;
  onNavigateToDestination: (dest: PdfDestination) => void;
  onOpenExternal: (url: string) => void;
}) {
  return (
    <ul className="text-xs text-[var(--text-main)]">
      {nodes.map((node, index) => {
        const navigable = Boolean(node.dest || node.url);
        return (
          <li key={`${depth}-${index}`}>
            <Tooltip content={node.title}>
              <button
                type="button"
                disabled={!navigable}
                onClick={() => {
                  if (node.url) onOpenExternal(node.url);
                  else if (node.dest) onNavigateToDestination(node.dest);
                }}
                style={{ paddingLeft: `${8 + depth * 12}px` }}
                className="block w-full text-left truncate py-1 pr-2 rounded hover:bg-[var(--bg-active)] disabled:opacity-60 disabled:cursor-default"
              >
                {node.title}
              </button>
            </Tooltip>
            {node.items.length > 0 && (
              <OutlineItems
                nodes={node.items}
                depth={depth + 1}
                onNavigateToDestination={onNavigateToDestination}
                onOpenExternal={onOpenExternal}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * Docked sidebar listing the PDF's own outline (table of contents). Entries
 * navigate through the same destination resolver used by in-page links.
 */
export default function PdfOutline({
  outline,
  onNavigateToDestination,
  onOpenExternal,
  onClose,
}: Props) {
  return (
    <aside className="w-64 shrink-0 flex flex-col border-r border-[var(--border-main)] bg-[var(--bg-app)] overflow-hidden">
      <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--border-main)]">
        <span className="text-xs font-semibold text-[var(--text-dim)] uppercase tracking-wide">
          Contents
        </span>
        <Tooltip content="Close table of contents">
          <button
            type="button"
            onClick={onClose}
            className="p-1 rounded hover:bg-[var(--bg-active)] text-[var(--text-dim)] hover:text-[var(--accent-red)]"
          >
            <X size={14} />
          </button>
        </Tooltip>
      </div>
      <div className="flex-1 overflow-auto py-1">
        <OutlineItems
          nodes={outline}
          depth={0}
          onNavigateToDestination={onNavigateToDestination}
          onOpenExternal={onOpenExternal}
        />
      </div>
    </aside>
  );
}
