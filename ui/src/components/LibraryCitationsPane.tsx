import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, FileText, Loader, Sidebar, X } from "react-feather";
import { Tooltip } from "@leonrjg/wilkes-reader";
import type {
  CitationGraph,
  CitationGraphDocument,
  ExternalCitedWork,
  FileEntry,
  FileMatches,
} from "../lib/types";
import {
  documentLabel,
  externalLabel,
  layoutCitationGraph,
  type GraphNode,
} from "../lib/utils/citationGraphLayout";
import { api } from "../services";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import {
  citationQueryKey,
  MIN_EXTERNAL_CITATION_OPTIONS,
  useCitationsStore,
  type CitationScope,
  type CitationView,
} from "../stores/useCitationsStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import { DocumentEntryRow } from "./DocumentEntryRow";
import { useToasts } from "./Toast";

/** The citation graph of the current folder or the whole library: what is
 *  cited most within it, what it cites most that it does not hold, and how
 *  much of it the graph covers. */
export default function LibraryCitationsPane() {
  const { addToast } = useToasts();
  const directory = useSettingsStore((state) => state.directory);
  const dock = useSettingsStore((state) => state.bookmarksDock);
  const setDock = useSettingsStore((state) => state.setBookmarksDock);
  const scope = useCitationsStore((state) => state.scope);
  const view = useCitationsStore((state) => state.view);
  const minExternal = useCitationsStore((state) => state.minExternalCitations);
  const loading = useCitationsStore((state) => state.loading);
  const error = useCitationsStore((state) => state.error);
  const graph = useCitationsStore((state) => state.graph);
  const loadedKey = useCitationsStore((state) => state.loadedKey);
  const fetching = useCitationsStore((state) => state.fetching);
  const setScope = useCitationsStore((state) => state.setScope);
  const setView = useCitationsStore((state) => state.setView);
  const setMinExternal = useCitationsStore((state) => state.setMinExternalCitations);
  const load = useCitationsStore((state) => state.load);
  const fetchMissing = useCitationsStore((state) => state.fetchMissing);
  const closePane = useCitationsStore((state) => state.closePane);
  const showResultSet = useSearchStore((state) => state.showResultSet);
  const openFile = useViewerStore((state) => state.openFile);
  const searchInPane = useCatalogueStore((state) => state.searchInPane);
  const [refreshVersion, setRefreshVersion] = useState(0);

  useEffect(() => {
    if (!directory) return;
    load(directory).catch((error) => {
      console.error("Failed to build citation graph:", error);
    });
  }, [directory, scope, minExternal, refreshVersion, load]);

  // Metadata and references fill in the background, after a listing or a
  // fetch. Rebuild once a burst of updates settles rather than per update.
  useEffect(() => {
    let mounted = true;
    let timeout: number | undefined;
    let unlisten: (() => void) | undefined;
    api
      .onFileMetadataUpdated(() => {
        window.clearTimeout(timeout);
        timeout = window.setTimeout(() => {
          if (mounted) setRefreshVersion((version) => version + 1);
        }, 1500);
      })
      .then((nextUnlisten) => {
        if (mounted) unlisten = nextUnlisten;
        else nextUnlisten();
      });
    return () => {
      mounted = false;
      window.clearTimeout(timeout);
      unlisten?.();
    };
  }, []);

  const current =
    directory && loadedKey === citationQueryKey(scope, directory, minExternal) ? graph : null;
  const here = scope === "root" ? "this folder" : "your library";

  const showFiles = async (entries: FileEntry[], subject: string) => {
    const results: FileMatches[] = entries.map((entry) => ({
      path: entry.path,
      file_type: entry.file_type,
      title: entry.title,
      matches: [],
    }));
    await showResultSet(results, { kind: "citations", subject });
  };

  const onFetchMissing = async () => {
    if (!directory) return;
    try {
      const queued = await fetchMissing(directory);
      addToast(
        queued === 0
          ? "Every document's references have been asked for"
          : `Fetching references for ${queued} ${queued === 1 ? "document" : "documents"}`,
        { type: "info" },
      );
    } catch (error) {
      console.error("Failed to fetch missing references:", error);
      addToast(error instanceof Error ? error.message : String(error), { type: "error" });
    }
  };

  return (
    <aside
      aria-label="Library citations"
      className="flex h-full flex-col border-l border-[var(--border-main)] bg-[var(--bg-sidebar)]"
    >
      <header className="flex flex-col gap-2 border-b border-[var(--border-main)] p-2">
        <div className="flex items-center justify-between gap-2">
          <div className="min-w-0">
            <h2 className="text-xs font-semibold text-[var(--text-main)]">Citations</h2>
            <p className="truncate text-[10px] text-[var(--text-dim)]">
              {current
                ? `${current.coverage.with_doi.toLocaleString()} documents, ${current.edges.length.toLocaleString()} links between them`
                : "Who cites whom, by DOI"}
            </p>
          </div>
          <div className="flex items-center gap-1">
            <Tooltip content={dock === "Left" ? "Dock right" : "Dock left"}>
              <button
                type="button"
                aria-label={dock === "Left" ? "Dock citations right" : "Dock citations left"}
                onClick={() => setDock(dock === "Left" ? "Right" : "Left")}
                className="flex h-7 w-7 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <Sidebar size={13} />
              </button>
            </Tooltip>
            <Tooltip content="Close citations">
              <button
                type="button"
                aria-label="Close citations"
                onClick={closePane}
                className="flex h-7 w-7 items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
              >
                <X size={14} />
              </button>
            </Tooltip>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Segmented<CitationScope>
            label="Citation scope"
            value={scope}
            onChange={setScope}
            options={[
              ["root", "This folder"],
              ["library", "Library"],
            ]}
          />
          <Segmented<CitationView>
            label="Citation view"
            value={view}
            onChange={setView}
            options={[
              ["list", "List"],
              ["graph", "Graph"],
            ]}
          />
        </div>
        {current && (
          <CoverageNotice
            graph={current}
            fetching={fetching}
            onFetchMissing={() => void onFetchMissing()}
          />
        )}
      </header>

      <div className="min-h-0 flex-1 overflow-auto custom-scrollbar">
        {!directory && (
          <p className="p-3 text-xs text-[var(--text-dim)]">Choose a folder to see its citations.</p>
        )}
        {directory && loading && !current && (
          <div className="flex items-center gap-2 p-3 text-xs text-[var(--text-dim)]">
            <Loader size={13} className="animate-spin" />
            Reading citations…
          </div>
        )}
        {directory && !loading && error && (
          <p className="p-3 text-xs text-[var(--accent-red)]" role="alert">
            {error}
          </p>
        )}
        {current && current.documents.length === 0 && (
          <p className="p-3 text-xs text-[var(--text-dim)]">
            No document in {here} has a DOI yet, so none can be joined by citation.
          </p>
        )}
        {current && current.documents.length > 0 && view === "list" && (
          <CitationLists
            graph={current}
            here={here}
            minExternal={minExternal}
            onMinExternalChange={setMinExternal}
            onOpen={(path) => openFile(path)}
            onFind={(work) => void searchInPane(work.reference.title ?? work.reference.doi)}
            onShowFiles={(entries, subject) => void showFiles(entries, subject)}
          />
        )}
        {current && current.documents.length > 0 && view === "graph" && (
          <CitationGraphView graph={current} onOpen={(path) => openFile(path)} />
        )}
      </div>
    </aside>
  );
}

function Segmented<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: [T, string][];
  onChange: (value: T) => void;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className="inline-flex overflow-hidden rounded border border-[var(--border-main)] bg-[var(--bg-active)]"
    >
      {options.map(([option, text], index) => (
        <button
          key={option}
          type="button"
          aria-pressed={value === option}
          onClick={() => onChange(option)}
          className={`px-2 py-1 text-[11px] ${index > 0 ? "border-l border-[var(--border-main)]" : ""} ${
            value === option
              ? "bg-[var(--bg-header)] text-[var(--text-main)]"
              : "text-[var(--text-muted)]"
          }`}
        >
          {text}
        </button>
      ))}
    </div>
  );
}

/** How much of the scope the graph covers, said whenever it is not all of
 *  it: a partial graph must not read as a complete one. */
function CoverageNotice({
  graph,
  fetching,
  onFetchMissing,
}: {
  graph: CitationGraph;
  fetching: boolean;
  onFetchMissing: () => void;
}) {
  const { coverage } = graph;
  const missing = coverage.with_doi - coverage.references_fetched;
  const parts: string[] = [
    `${coverage.with_doi} of ${coverage.files} files have a DOI`,
    `${coverage.references_fetched} of those have references`,
  ];
  if (coverage.pending_metadata > 0) {
    parts.push(`${coverage.pending_metadata} still being read`);
  }
  if (coverage.metadata_unavailable > 0) {
    parts.push(`${coverage.metadata_unavailable} cannot be read`);
  }
  const complete =
    missing === 0 && coverage.pending_metadata === 0 && coverage.metadata_unavailable === 0;
  return (
    <div
      role="status"
      className={`rounded px-2 py-1.5 text-[11px] leading-snug ${
        complete
          ? "text-[var(--text-dim)]"
          : "bg-[var(--bg-active)] text-[var(--text-muted)]"
      }`}
    >
      {parts.join(", ")}.
      {missing > 0 && coverage.references_provider && (
        <button
          type="button"
          disabled={fetching}
          onClick={onFetchMissing}
          className="ml-1 text-[var(--accent-blue)] underline disabled:opacity-50"
        >
          {fetching ? "Fetching…" : `Fetch ${missing} missing`}
        </button>
      )}
      {missing > 0 && !coverage.references_provider && (
        <span className="ml-1">
          No enabled integration fetches references; turn on OpenAlex in Settings › Integrations.
        </span>
      )}
    </div>
  );
}

function CitationLists({
  graph,
  here,
  minExternal,
  onMinExternalChange,
  onOpen,
  onFind,
  onShowFiles,
}: {
  graph: CitationGraph;
  here: string;
  minExternal: number;
  onMinExternalChange: (min: number) => void;
  onOpen: (path: string) => void;
  onFind: (work: ExternalCitedWork) => void;
  onShowFiles: (entries: FileEntry[], subject: string) => void;
}) {
  const mostCited = useMemo(
    () =>
      graph.documents
        .map((document, index) => ({ document, index }))
        .filter(({ document }) => document.cited_by.length > 0)
        .sort(
          (a, b) =>
            b.document.cited_by.length - a.document.cited_by.length ||
            documentLabel(a.document).localeCompare(documentLabel(b.document)),
        ),
    [graph],
  );
  const unconnected = useMemo(() => {
    const linked = new Set<number>();
    for (const edge of graph.edges) {
      linked.add(edge.source);
      linked.add(edge.target);
    }
    return graph.documents.filter(
      (document, index) => document.references_fetched && !linked.has(index),
    );
  }, [graph]);

  return (
    <>
      <ListSection title={`Most cited in ${here}`} count={mostCited.length}>
        {mostCited.length === 0 ? (
          <p className="px-3 py-2 text-[11px] text-[var(--text-dim)]">
            No document here cites another one here yet.
          </p>
        ) : (
          mostCited.map(({ document }) => (
            <CitedDocumentRow
              key={document.doi}
              document={document}
              citing={document.cited_by.map((source) => graph.documents[source])}
              onOpen={onOpen}
            />
          ))
        )}
      </ListSection>

      <ListSection
        title={`Cited here, not in ${here}`}
        count={graph.external.length}
        trailing={
          <label className="flex items-center gap-1 text-[10px] text-[var(--text-dim)]">
            cited by at least
            <select
              aria-label="Minimum citations for outside works"
              value={minExternal}
              onChange={(event) => onMinExternalChange(Number(event.target.value))}
              className="rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1 text-[10px] text-[var(--text-main)]"
            >
              {MIN_EXTERNAL_CITATION_OPTIONS.map((option) => (
                <option key={option} value={option}>
                  {option}
                </option>
              ))}
            </select>
          </label>
        }
      >
        {graph.external.length === 0 ? (
          <p className="px-3 py-2 text-[11px] text-[var(--text-dim)]">
            {graph.external_total === 0
              ? "No outside work is cited here yet."
              : `None of the ${graph.external_total} outside works is cited ${minExternal} times or more.`}
          </p>
        ) : (
          graph.external.map((work) => (
            <ExternalWorkRow
              key={work.reference.doi}
              work={work}
              onOpen={onOpen}
              onFind={onFind}
            />
          ))
        )}
      </ListSection>

      {(graph.without_doi.length > 0 || unconnected.length > 0) && (
        <div className="flex flex-col gap-1 px-3 py-2 text-[11px] text-[var(--text-dim)]">
          {graph.without_doi.length > 0 && (
            <button
              type="button"
              className="text-left hover:text-[var(--text-main)]"
              onClick={() => onShowFiles(graph.without_doi, "Documents without a DOI")}
            >
              {graph.without_doi.length} without a DOI, left out ·{" "}
              <span className="underline">Show</span>
            </button>
          )}
          {unconnected.length > 0 && (
            <button
              type="button"
              className="text-left hover:text-[var(--text-main)]"
              onClick={() =>
                onShowFiles(
                  unconnected.flatMap((document) => document.entries),
                  `Documents with no citation links in ${here}`,
                )
              }
            >
              {unconnected.length} with no links to others here ·{" "}
              <span className="underline">Show</span>
            </button>
          )}
        </div>
      )}
    </>
  );
}

function ListSection({
  title,
  count,
  trailing,
  children,
}: {
  title: string;
  count: number;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(false);
  return (
    <section className="border-b border-[var(--border-main)] pb-1">
      <div className="flex items-center gap-1 px-3 py-1.5">
        <button
          type="button"
          aria-expanded={!collapsed}
          aria-label={`${title}, ${count}`}
          onClick={() => setCollapsed((value) => !value)}
          className="flex min-w-0 flex-1 items-center gap-1 text-[11px] font-medium text-[var(--text-muted)] hover:text-[var(--text-main)]"
        >
          {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
          <span className="min-w-0 truncate text-left">{title}</span>
          <span className="flex-shrink-0 text-[var(--text-dim)]">{count}</span>
        </button>
        {trailing}
      </div>
      {!collapsed && children}
    </section>
  );
}

function CitedDocumentRow({
  document,
  citing,
  onOpen,
}: {
  document: CitationGraphDocument;
  citing: CitationGraphDocument[];
  onOpen: (path: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  return (
    <DocumentEntryRow
      entry={document.entries[0]}
      details={[
        {
          key: "title",
          label: "Title",
          value: document.entries[0].title ?? "",
          icon: FileText,
          fullWidth: true,
          monospace: false,
          hideWhenMissing: true,
        },
      ]}
      onClick={() => onOpen(document.entries[0].path)}
      accessory={
        <div className="px-3 pb-1">
          <button
            type="button"
            aria-expanded={expanded}
            onClick={() => setExpanded((value) => !value)}
            className="flex items-center gap-1 text-[10px] text-[var(--text-dim)] hover:text-[var(--text-main)]"
          >
            {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
            Cited by {citing.length} here
          </button>
          {expanded && (
            <ul className="ml-3 mt-0.5 flex flex-col gap-0.5">
              {citing.map((source) => (
                <li key={source.doi}>
                  <button
                    type="button"
                    onClick={() => onOpen(source.entries[0].path)}
                    className="text-left text-[11px] text-[var(--text-muted)] hover:text-[var(--text-main)]"
                  >
                    {documentLabel(source)}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      }
    />
  );
}

function ExternalWorkRow({
  work,
  onOpen,
  onFind,
}: {
  work: ExternalCitedWork;
  onOpen: (path: string) => void;
  onFind: (work: ExternalCitedWork) => void;
}) {
  const { reference } = work;
  const byline = [reference.authors, reference.year?.toString(), reference.venue]
    .filter((part): part is string => Boolean(part))
    .join(" · ");
  return (
    <div className="px-3 py-1.5 text-[11px] leading-snug">
      <div className="flex items-start gap-2">
        <Tooltip content={reference.doi} size="wide">
          <div
            className={`min-w-0 flex-1 text-[var(--text-main)] ${
              reference.title ? "break-words" : "break-all font-mono"
            }`}
          >
            {externalLabel(work)}
            {work.library_entry && (
              <span className="ml-1 rounded bg-[var(--bg-active)] px-1 text-[10px] text-[var(--accent-blue)]">
                in library
              </span>
            )}
          </div>
        </Tooltip>
        <span className="flex-shrink-0 text-[10px] text-[var(--text-dim)]">
          cited by {work.cited_by.length}
        </span>
      </div>
      <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[10px] text-[var(--text-dim)]">
        {byline && <span className="break-words">{byline}</span>}
        {work.library_entry ? (
          <button
            type="button"
            onClick={() => onOpen(work.library_entry!.path)}
            className="text-[var(--accent-blue)] hover:underline"
          >
            Open
          </button>
        ) : (
          <Tooltip content="Search the literature pane for this work">
            <button
              type="button"
              onClick={() => onFind(work)}
              className="text-[var(--accent-blue)] hover:underline"
            >
              Find
            </button>
          </Tooltip>
        )}
      </div>
    </div>
  );
}

/** The year-axis drawing of the graph. Older work is to the left, so every
 *  citation runs leftward or within a year's column. */
function CitationGraphView({
  graph,
  onOpen,
}: {
  graph: CitationGraph;
  onOpen: (path: string) => void;
}) {
  const layout = useMemo(() => layoutCitationGraph(graph), [graph]);
  const [hovered, setHovered] = useState<string | null>(null);
  const byKey = useMemo(
    () => new Map(layout.nodes.map((node) => [node.key, node])),
    [layout],
  );
  const neighbours = useMemo(() => {
    if (!hovered) return null;
    const keys = new Set([hovered]);
    for (const edge of layout.edges) {
      if (edge.from === hovered) keys.add(edge.to);
      if (edge.to === hovered) keys.add(edge.from);
    }
    return keys;
  }, [hovered, layout]);

  const activate = (node: GraphNode) => {
    if (node.path) onOpen(node.path);
  };

  return (
    <div className="flex flex-col gap-1 p-2">
      <div className="overflow-x-auto custom-scrollbar">
        <svg
          role="img"
          aria-label={`Citation graph by year: ${layout.nodes.length} works, ${layout.edges.length} citations`}
          width={layout.width}
          height={layout.height}
          className="block"
        >
          <defs>
            <marker
              id="citation-arrow"
              viewBox="0 0 6 6"
              refX="5"
              refY="3"
              markerWidth="5"
              markerHeight="5"
              orient="auto-start-reverse"
            >
              <path d="M0,0 L6,3 L0,6 z" style={{ fill: "var(--text-dim)" }} />
            </marker>
          </defs>
          {layout.edges.map((edge) => {
            const from = byKey.get(edge.from)!;
            const to = byKey.get(edge.to)!;
            const active = hovered !== null && (edge.from === hovered || edge.to === hovered);
            return (
              <path
                key={`${edge.from}-${edge.to}`}
                d={edgePath(from, to)}
                fill="none"
                markerEnd="url(#citation-arrow)"
                style={{
                  stroke: active ? "var(--accent-blue)" : "var(--border-strong)",
                  strokeWidth: active ? 1.4 : 0.8,
                  opacity: neighbours && !active ? 0.15 : 0.8,
                }}
              />
            );
          })}
          {layout.nodes.map((node) => {
            const dimmed = neighbours !== null && !neighbours.has(node.key);
            const external = node.kind === "external";
            return (
              <g
                key={node.key}
                role={node.path ? "button" : undefined}
                tabIndex={node.path ? 0 : undefined}
                aria-label={node.description.replace(/\n/g, ", ")}
                onMouseEnter={() => setHovered(node.key)}
                onMouseLeave={() => setHovered(null)}
                onFocus={() => setHovered(node.key)}
                onBlur={() => setHovered(null)}
                onClick={() => activate(node)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") activate(node);
                }}
                style={{ cursor: node.path ? "pointer" : "default", opacity: dimmed ? 0.25 : 1 }}
              >
                <title>{node.description}</title>
                <circle
                  cx={node.x}
                  cy={node.y}
                  r={node.r}
                  strokeDasharray={external ? "2 2" : undefined}
                  style={{
                    fill: external ? "var(--bg-sidebar)" : "var(--accent-blue)",
                    fillOpacity: external ? 1 : 0.75,
                    stroke: external ? "var(--text-dim)" : "var(--accent-blue)",
                    strokeWidth: 1,
                  }}
                />
                {node.labelled && (
                  <text
                    x={node.x + node.r + 3}
                    y={node.y + 3}
                    style={{ fill: "var(--text-muted)", fontSize: 10 }}
                  >
                    {truncate(node.label, 22)}
                  </text>
                )}
              </g>
            );
          })}
          <line
            x1={0}
            x2={layout.width}
            y1={layout.axisY}
            y2={layout.axisY}
            style={{ stroke: "var(--border-main)" }}
          />
          {layout.columns.map((column) => (
            <text
              key={column.label}
              x={column.x}
              y={layout.axisY + 14}
              textAnchor="middle"
              style={{ fill: "var(--text-dim)", fontSize: 10 }}
            >
              {column.label}
            </text>
          ))}
        </svg>
      </div>
      <p className="px-1 text-[10px] leading-snug text-[var(--text-dim)]">
        Filled: in this scope, sized by citations here. Dashed: cited from here, outside it.
        {layout.hiddenDocuments > 0 &&
          ` Showing the ${layout.nodes.filter((n) => n.kind === "document").length} best-connected of ${graph.documents.length} documents.`}
        {layout.hiddenExternal > 0 &&
          ` ${layout.hiddenExternal} more outside works are in the list.`}
      </p>
    </div>
  );
}

/** A curve from the citing work to the cited one, ending at its edge. Works in
 *  one column are joined by an arc bowed to the right. */
function edgePath(from: GraphNode, to: GraphNode) {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.hypot(dx, dy) || 1;
  if (Math.abs(dx) < 1) {
    const bow = 18 + Math.min(40, Math.abs(dy) / 4);
    const endY = to.y - Math.sign(dy) * to.r;
    return `M${from.x},${from.y} C${from.x + bow},${from.y} ${to.x + bow},${to.y} ${to.x + to.r * 0.7},${endY}`;
  }
  const endX = to.x - (dx / length) * to.r;
  const endY = to.y - (dy / length) * to.r;
  const midX = (from.x + to.x) / 2;
  return `M${from.x},${from.y} C${midX},${from.y} ${midX},${endY} ${endX},${endY}`;
}

function truncate(text: string, length: number) {
  return [...text].length > length ? `${[...text].slice(0, length - 1).join("")}…` : text;
}
