import { useCallback, useEffect, useState } from "react";
import { ArrowDownLeft, ArrowUpRight, ChevronDown, ChevronRight, X } from "react-feather";
import type { CitationLinks, CitationReference, FileEntry } from "../lib/types";
import { api } from "../services";
import { useSettingsStore } from "../stores/useSettingsStore";
import { DocumentEntryRow, fileName } from "./DocumentEntryRow";
import { Tooltip } from "./preview";

type CitationStatus = "loading" | "ready" | "empty" | "error";

interface Props {
  currentPath: string;
  doi: string;
  onOpenDocument: (path: string) => void;
  onClose: () => void;
}

export default function CitationGraphPane({
  currentPath,
  doi,
  onOpenDocument,
  onClose,
}: Props) {
  const directory = useSettingsStore((state) => state.directory);
  const [status, setStatus] = useState<CitationStatus>("loading");
  const [links, setLinks] = useState<CitationLinks>({
    references: [],
    cited_by: [],
    all_references: [],
  });
  const [refreshVersion, setRefreshVersion] = useState(0);

  const load = useCallback(() => {
    if (!directory) {
      setLinks({ references: [], cited_by: [], all_references: [] });
      setStatus("error");
      return () => {};
    }

    let cancelled = false;
    setStatus("loading");
    api.citationLinks({ root: directory, path: currentPath })
      .then((result) => {
        if (cancelled) return;
        setLinks(result);
        setStatus(
          result.references.length > 0 ||
            result.cited_by.length > 0 ||
            result.all_references.length > 0
            ? "ready"
            : "empty",
        );
      })
      .catch((error) => {
        if (cancelled) return;
        console.debug("Citation links unavailable:", error);
        setLinks({ references: [], cited_by: [], all_references: [] });
        setStatus("error");
      });

    return () => {
      cancelled = true;
    };
  }, [currentPath, directory]);

  useEffect(load, [load, refreshVersion]);

  // OpenAlex fills citation edges asynchronously. Refetch an open pane when
  // metadata for its anchor changes instead of retaining an early empty result.
  useEffect(() => {
    let mounted = true;
    let unlisten: (() => void) | undefined;
    api.onFileMetadataUpdated((updates) => {
      if (mounted && updates.some((update) => update.path === currentPath)) {
        setRefreshVersion((version) => version + 1);
      }
    }).then((nextUnlisten) => {
      if (mounted) unlisten = nextUnlisten;
      else nextUnlisten();
    });
    return () => {
      mounted = false;
      unlisten?.();
    };
  }, [currentPath]);

  return (
    <aside
      aria-label="Citation graph"
      className="hidden w-64 flex-shrink-0 border-l border-[var(--border-main)] bg-[var(--bg-sidebar)] md:flex md:flex-col"
    >
      <div className="flex items-center gap-1 border-b border-[var(--border-main)] px-3 py-2 text-xs font-medium text-[var(--text-main)]">
        <Tooltip content={`${currentPath}\nDOI: ${doi}`} className="font-mono break-all">
          <span className="min-w-0 flex-1 truncate">Citations for {fileName(currentPath)}</span>
        </Tooltip>
        <Tooltip content="Close citation graph">
          <button
            type="button"
            onClick={onClose}
            aria-label="Close citation graph"
            className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
          >
            <X size={14} />
          </button>
        </Tooltip>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {status === "loading" && <StatusMessage>Loading citation graph…</StatusMessage>}
        {status === "error" && <StatusMessage error>Citation graph unavailable</StatusMessage>}
        {status === "empty" && <StatusMessage>No citation references found</StatusMessage>}
        {status === "ready" && (
          <>
            <CitationSection
              title="References in your library"
              icon={ArrowUpRight}
              entries={links.references}
              onOpen={onOpenDocument}
            />
            <CitationSection
              title="Cited by in your library"
              icon={ArrowDownLeft}
              entries={links.cited_by}
              onOpen={onOpenDocument}
            />
            <ReferenceSection entries={links.all_references} />
          </>
        )}
      </div>
    </aside>
  );
}

function ReferenceSection({ entries }: { entries: CitationReference[] }) {
  const [collapsed, setCollapsed] = useState(false);
  if (entries.length === 0) return null;
  return (
    <section className="mb-1 border-b border-[var(--border-main)] pb-1">
      <button
        type="button"
        aria-expanded={!collapsed}
        aria-label={`References, ${entries.length}`}
        onClick={() => setCollapsed((value) => !value)}
        className="flex w-full items-center gap-1 px-3 py-1.5 text-[11px] font-medium text-[var(--text-muted)] hover:text-[var(--text-main)]"
      >
        {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
        <span className="min-w-0 flex-1 truncate text-left">References</span>
        <span className="flex-shrink-0 rounded-full bg-[var(--bg-active)] px-1.5 py-0.5 text-[10px] leading-none text-[var(--text-dim)]">
          {entries.length}
        </span>
      </button>
      {!collapsed &&
        entries.map((entry) => (
          <Tooltip key={entry.doi} content={entry.doi} size="wide">
            <div
              className={`px-3 py-1.5 text-[11px] leading-snug text-[var(--text-main)] ${
                entry.citation_line ? "break-words" : "break-all font-mono"
              }`}
            >
              {entry.citation_line ?? entry.doi}
            </div>
          </Tooltip>
        ))}
    </section>
  );
}

function StatusMessage({ children, error = false }: { children: string; error?: boolean }) {
  return (
    <div className={`px-3 py-3 text-xs ${error ? "text-red-500" : "text-[var(--text-dim)]"}`}>
      {children}
    </div>
  );
}

function CitationSection({
  title,
  icon: Icon,
  entries,
  onOpen,
}: {
  title: string;
  icon: React.ComponentType<{ size?: number }>;
  entries: FileEntry[];
  onOpen: (path: string) => void;
}) {
  const [collapsed, setCollapsed] = useState(false);
  if (entries.length === 0) return null;
  return (
    <section className="mb-1 border-b border-[var(--border-main)] pb-1">
      <button
        type="button"
        aria-expanded={!collapsed}
        onClick={() => setCollapsed((value) => !value)}
        className="flex w-full items-center gap-1 px-3 py-1.5 text-[11px] font-medium text-[var(--text-muted)] hover:text-[var(--text-main)]"
      >
        {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
        <Icon size={12} />
        <span className="min-w-0 flex-1 truncate text-left">{title}</span>
        <span className="flex-shrink-0 text-[var(--text-dim)]">{entries.length}</span>
      </button>
      {!collapsed &&
        entries.map((entry) => (
          <DocumentEntryRow key={entry.path} entry={entry} onClick={() => onOpen(entry.path)} />
        ))}
    </section>
  );
}
