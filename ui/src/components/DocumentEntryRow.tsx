import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type React from "react";
import { Folder, Tag as TagIcon } from "react-feather";
import { Tooltip } from "./Tooltip";
import type { FileType, Tag } from "../lib/types";

export type DetailIcon = React.ComponentType<React.SVGProps<SVGSVGElement> & { size?: number }>;

export interface DocumentDetail {
  key: string;
  label: string;
  value: string;
  valueTitle?: string;
  icon: DetailIcon;
  fullWidth?: boolean;
  monospace?: boolean;
  hideWhenMissing?: boolean;
  conflictTooltip?: React.ReactNode;
}

interface DocumentEntry {
  path: string;
  file_type: FileType;
  tags?: Tag[];
}

interface Props {
  entry: DocumentEntry;
  details?: DocumentDetail[];
  /** Compact status rendered immediately after the filename. */
  nameAccessory?: React.ReactNode;
  /** Rendered directly below the row, outside its button so it can hold its
   *  own controls. Absent for every caller that has nothing to attach. */
  accessory?: React.ReactNode;
  selected?: boolean;
  muted?: boolean;
  onClick: () => void;
  onContextMenu?: (event: React.MouseEvent) => void;
  onTagClick?: (tag: Tag) => void;
}

export function fileName(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

export function DocumentEntryRow({
  entry,
  details = [],
  nameAccessory,
  accessory,
  selected = false,
  muted = false,
  onClick,
  onContextMenu,
  onTagClick,
}: Props) {
  const visibleDetails = details.filter(
    (field) =>
      !field.hideWhenMissing ||
      (field.value.trim() !== "" && field.value !== "—"),
  );
  const inlineDetails = visibleDetails.filter((field) => !field.fullWidth);
  const fullWidthDetails = visibleDetails
    .filter((field) => field.fullWidth)
    .filter((field) => field.value.trim() !== "" && field.value !== "—");
  const inlineDetailsRef = useRef<HTMLSpanElement>(null);
  const [inlineDetailsExpanded, setInlineDetailsExpanded] = useState(false);
  const [inlineDetailsOverflow, setInlineDetailsOverflow] = useState(false);
  const inlineDetailsSignature = inlineDetails
    .map((field) => `${field.key}:${field.value}:${field.valueTitle ?? ""}`)
    .join("|");

  useLayoutEffect(() => {
    if (inlineDetailsExpanded) return;
    const element = inlineDetailsRef.current;
    if (!element) {
      setInlineDetailsOverflow(false);
      return;
    }

    const measure = () => {
      setInlineDetailsOverflow(element.scrollWidth > element.clientWidth + 1);
    };

    measure();
    const resizeObserver = new ResizeObserver(measure);
    resizeObserver.observe(element);
    return () => resizeObserver.disconnect();
  }, [inlineDetailsExpanded, inlineDetailsSignature]);

  useEffect(() => {
    setInlineDetailsExpanded(false);
  }, [entry.path, inlineDetailsSignature]);

  const row = (
    <button
      type="button"
      onClick={onClick}
      onContextMenu={onContextMenu}
      className={`w-full flex select-none flex-col gap-1 px-3 py-1.5 text-left hover:bg-[var(--bg-hover)] transition-colors ${
        selected ? "bg-[var(--bg-active)]" : ""
      }`}
    >
      <span className="flex w-full min-w-0 items-center gap-1.5">
        {nameAccessory}
        <span
          className={`min-w-0 truncate text-sm font-medium ${
            muted ? "text-[var(--text-muted)]" : "text-[var(--text-main)]"
          }`}
        >
          {fileName(entry.path)}
        </span>
        <span className="min-w-0 flex-1" aria-hidden="true" />
        {entry.file_type === "Pdf" && (
          <Tooltip content="Type">
            <span
              className="inline-flex flex-shrink-0 items-center gap-1 text-xs font-mono tabular-nums text-[var(--accent-blue)]"
              aria-label="Type"
            >
              <TagIcon size={11} aria-hidden="true" />
              PDF
            </span>
          </Tooltip>
        )}
        <Tooltip content={entry.path} className="font-mono break-all">
          <span
            className="flex h-5 w-5 flex-shrink-0 items-center justify-center text-[var(--text-dim)]"
            aria-label={`Path: ${entry.path}`}
          >
            <Folder size={12} aria-hidden="true" />
          </span>
        </Tooltip>
      </span>
      {!!entry.tags?.length && (
        <span className="flex flex-wrap gap-1">
          {entry.tags.map((tag) => (
            <span
              key={tag.id}
              role={onTagClick ? "button" : undefined}
              tabIndex={onTagClick ? 0 : undefined}
              onClick={onTagClick ? (event) => { event.stopPropagation(); onTagClick(tag); } : undefined}
              onKeyDown={onTagClick ? (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  event.stopPropagation();
                  onTagClick(tag);
                }
              } : undefined}
              className={`rounded-full bg-[var(--accent-blue-muted)] px-1.5 py-0.5 text-[10px] text-[var(--accent-blue)] ${onTagClick ? "hover:ring-1 hover:ring-[var(--accent-blue)]" : ""}`}
            >{tag.name}</span>
          ))}
        </span>
      )}
      {inlineDetails.length > 0 && (
        <span className="flex w-full min-w-0 items-start gap-1.5 pl-0.5">
          <span
            ref={inlineDetailsRef}
            className={`flex min-w-0 flex-1 items-center gap-x-2 gap-y-0.5 ${
              inlineDetailsExpanded ? "flex-wrap" : "overflow-hidden whitespace-nowrap"
            }`}
          >
            {inlineDetails.map((field) => (
              <span key={field.key} className="inline-flex min-w-0 flex-shrink-0 items-center gap-1 text-xs">
                <Tooltip content={field.label}>
                  <span
                    className="flex h-3.5 w-3.5 flex-shrink-0 items-center justify-center text-[var(--text-dim)]"
                    aria-label={field.label}
                  >
                    <field.icon size={11} aria-hidden="true" />
                  </span>
                </Tooltip>
                <Tooltip content={field.conflictTooltip ?? field.valueTitle}>
                  <span
                    className={`min-w-0 truncate text-[var(--text-muted)] ${
                      field.monospace ? "font-mono tabular-nums" : ""
                    } ${field.key === "file-type" ? "text-[var(--accent-blue)]" : ""} ${
                      field.conflictTooltip ? "underline decoration-wavy decoration-[var(--accent-blue)] underline-offset-2" : ""
                    }`}
                  >
                    {field.value}
                  </span>
                </Tooltip>
              </span>
            ))}
          </span>
          {inlineDetailsOverflow && !inlineDetailsExpanded && (
            <span
              role="button"
              tabIndex={0}
              aria-label="Show hidden file details"
              onClick={(event) => {
                event.stopPropagation();
                setInlineDetailsExpanded(true);
              }}
              onKeyDown={(event) => {
                if (event.key !== "Enter" && event.key !== " ") return;
                event.preventDefault();
                event.stopPropagation();
                setInlineDetailsExpanded(true);
              }}
              className="flex h-4 flex-shrink-0 items-center rounded border border-[var(--border-main)] px-1 text-[10px] leading-none text-[var(--text-dim)] hover:border-[var(--border-strong)] hover:text-[var(--text-muted)]"
            >
              ...
            </span>
          )}
        </span>
      )}
      {fullWidthDetails.map((field) => (
        <span key={field.key} className="flex w-full min-w-0 items-center gap-1.5 pl-0.5 text-xs">
          <Tooltip content={field.label}>
            <span
              className="flex h-3.5 w-3.5 flex-shrink-0 items-center justify-center text-[var(--text-dim)]"
              aria-label={field.label}
            >
              <field.icon size={11} aria-hidden="true" />
            </span>
          </Tooltip>
          <Tooltip content={field.conflictTooltip ?? field.valueTitle ?? field.value}>
            <span
              className={`min-w-0 flex-1 truncate text-[var(--text-muted)] ${
                field.conflictTooltip ? "underline decoration-wavy decoration-[var(--accent-blue)] underline-offset-2" : ""
              }`}
            >
              {field.value}
            </span>
          </Tooltip>
        </span>
      ))}
    </button>
  );

  if (!accessory) return row;
  return (
    <div className={selected ? "bg-[var(--bg-active)]" : ""}>
      {row}
      {accessory}
    </div>
  );
}
