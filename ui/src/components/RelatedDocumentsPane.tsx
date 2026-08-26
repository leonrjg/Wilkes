import { useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  Globe,
  Percent,
  X,
} from "react-feather";
import { api } from "../services";
import type { FileEntry, RelatedDocument } from "../lib/types";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { fileName, type DocumentDetail } from "./DocumentEntryRow";
import ResultList from "./ResultList";
import { Tooltip } from "./preview";
import {
  useGenerationStream,
  type GenerationStreamPhase,
} from "../hooks/useGenerationStream";

type RelatedStatus = "loading" | "ready" | "empty" | "error" | "unavailable";

interface CachedRelated {
  documents: RelatedDocument[];
  /** Completed explanations by path. Kept in the existing cache rather than a
   *  second one: this key already encodes the index identity, so it is already
   *  invalidated whenever the model or the index changes. */
  explanations: Record<string, string>;
}

const relatedDocumentsCache = new Map<string, CachedRelated>();

interface Props {
  currentPath: string;
  onOpenDocument: (path: string) => void;
  onClose: () => void;
}

export default function RelatedDocumentsPane({ currentPath, onOpenDocument, onClose }: Props) {
  const directory = useSettingsStore((state) => state.directory);
  const indexReady = useSemanticStore((state) => state.readyForCurrentRoot);
  const indexStatus = useSemanticStore((state) => state.indexStatus);
  const [anchorPath, setAnchorPath] = useState(currentPath);
  const [status, setStatus] = useState<RelatedStatus>("loading");
  const [documents, setDocuments] = useState<RelatedDocument[]>([]);
  const [wholeLibrary, setWholeLibrary] = useState(false);
  const [filterText, setFilterText] = useState("");
  const relatedNavigationTargetRef = useRef<string | null>(null);
  const generationReady = useGenerationStore((state) => state.ready);
  const [cacheKey, setCacheKey] = useState<string | null>(null);
  const [expandedPath, setExpandedPath] = useState<string | null>(null);

  useEffect(() => {
    if (currentPath === anchorPath) return;
    if (relatedNavigationTargetRef.current === currentPath) {
      relatedNavigationTargetRef.current = null;
      return;
    }
    relatedNavigationTargetRef.current = null;
    setAnchorPath(currentPath);
  }, [currentPath, anchorPath]);

  useEffect(() => {
    if (!directory || !indexReady || !indexStatus) {
      setStatus("unavailable");
      setDocuments([]);
      return;
    }

    const indexKey = `${indexStatus.model_id}:${indexStatus.built_at ?? "unknown"}`;
    const scope = wholeLibrary ? "all" : "corpus";
    const cacheKey = `${directory}\0${anchorPath}\0${scope}\0${indexKey}`;
    setCacheKey(cacheKey);
    const cached = relatedDocumentsCache.get(cacheKey);
    if (cached) {
      setDocuments(cached.documents);
      setStatus(cached.documents.length > 0 ? "ready" : "empty");
      return;
    }

    let cancelled = false;
    setStatus("loading");
    setDocuments([]);
    api.relatedDocuments({ root: directory, path: anchorPath, scope: { type: scope }, limit: 8 })
      .then((result) => {
        if (cancelled) return;
        const sorted = sortRelatedDocuments(result);
        relatedDocumentsCache.set(cacheKey, { documents: sorted, explanations: {} });
        setDocuments(sorted);
        setStatus(sorted.length > 0 ? "ready" : "empty");
      })
      .catch((error) => {
        if (cancelled) return;
        console.debug("Related documents unavailable:", error);
        setDocuments([]);
        setStatus("error");
      });

    return () => {
      cancelled = true;
    };
  }, [anchorPath, directory, indexReady, indexStatus?.model_id, indexStatus?.built_at, wholeLibrary]);

  // Switching anchor, scope or root abandons any explanation in flight: its
  // subject is no longer on screen.
  useEffect(() => {
    setExpandedPath(null);
  }, [anchorPath, wholeLibrary, directory]);

  // One explanation in flight at a time, requested on expand and never on
  // render: eight at ~2s each would hold the worker for sixteen seconds to
  // produce seven sentences nobody reads.
  const cachedExplanation =
    cacheKey && expandedPath
      ? relatedDocumentsCache.get(cacheKey)?.explanations[expandedPath]
      : undefined;
  const startExplanation = useCallback(
    (requestId: string) =>
      expandedPath
        ? api.explainRelatedDocument(requestId, anchorPath, expandedPath)
        : Promise.resolve(),
    [anchorPath, expandedPath],
  );
  const { phase: streamedPhase } = useGenerationStream({
    enabled:
      generationReady && expandedPath != null && cachedExplanation === undefined,
    requestKey:
      expandedPath == null ? null : `${anchorPath}\u0000${expandedPath}`,
    task: "relation_explanation",
    start: startExplanation,
  });
  const phase: GenerationStreamPhase =
    cachedExplanation === undefined
      ? streamedPhase
      : { kind: "done", text: cachedExplanation };

  useEffect(() => {
    if (
      phase.kind === "done" &&
      cacheKey &&
      expandedPath &&
      relatedDocumentsCache.get(cacheKey)
    ) {
      relatedDocumentsCache.get(cacheKey)!.explanations[expandedPath] = phase.text;
    } else if (phase.kind === "failed") {
      // A partial stream is discarded rather than shown: a sentence cut
      // mid-clause is indistinguishable from a complete one.
      console.debug("Relation explanation unavailable:", phase.error);
    }
  }, [cacheKey, expandedPath, phase]);

  return (
    <aside className="hidden w-64 flex-shrink-0 border-l border-[var(--border-main)] bg-[var(--bg-sidebar)] md:flex md:flex-col">
      <div className="flex items-center gap-1 border-b border-[var(--border-main)] px-3 py-2 text-xs font-medium text-[var(--text-main)]">
        <Tooltip content={anchorPath} className="font-mono break-all">
          <span className="min-w-0 flex-1 truncate">Related to {fileName(anchorPath)}</span>
        </Tooltip>
        {currentPath !== anchorPath && (
          <button
            type="button"
            onClick={() => setAnchorPath(currentPath)}
            className="flex-shrink-0 rounded px-1.5 py-0.5 text-[10px] text-[var(--accent-blue)] hover:bg-[var(--accent-blue-muted)]"
          >
            Use current
          </button>
        )}
        <Tooltip content={wholeLibrary ? "Search related documents in current root" : "Search related documents in whole library"}>
          <button
            type="button"
            onClick={() => setWholeLibrary((value) => !value)}
            aria-label={wholeLibrary ? "Use current root for related documents" : "Use whole library for related documents"}
            aria-pressed={wholeLibrary}
            className={`inline-flex flex-shrink-0 rounded p-0.5 transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] ${
              wholeLibrary ? "bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]" : "text-[var(--text-dim)]"
            }`}
          >
            <Globe size={14} />
          </button>
        </Tooltip>
        <Tooltip content="Close related documents">
          <button
            type="button"
            onClick={onClose}
            aria-label="Close related documents"
            className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
          >
            <X size={14} />
          </button>
        </Tooltip>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto py-1">
        {status === "loading" && <StatusMessage>Loading…</StatusMessage>}
        {status === "unavailable" && <StatusMessage>Semantic index unavailable</StatusMessage>}
        {status === "error" && <StatusMessage error>Related documents unavailable</StatusMessage>}
        {status === "empty" && <StatusMessage>No related documents</StatusMessage>}
        {status === "ready" && (
          <ResultList
            filterText={filterText}
            onFilterTextChange={setFilterText}
            documents={documents}
            preserveDocumentOrder
            documentDetails={relatedDocumentDetails}
            documentAccessory={(entry) =>
              // Per the gating rule: with generation unavailable the row renders
              // exactly as it did before this feature existed — no expander, no
              // spinner, no placeholder.
              generationReady ? (
                <ExplanationRow
                  expanded={expandedPath === entry.path}
                  phase={expandedPath === entry.path ? phase : { kind: "absent" }}
                  onToggle={() =>
                    setExpandedPath((current) => (current === entry.path ? null : entry.path))
                  }
                />
              ) : null
            }
            onFileClick={(path) => {
              relatedNavigationTargetRef.current = path;
              onOpenDocument(path);
            }}
            onMatchClick={() => {}}
          />
        )}
      </div>
    </aside>
  );
}

function ExplanationRow({
  expanded,
  phase,
  onToggle,
}: {
  expanded: boolean;
  phase: GenerationStreamPhase;
  onToggle: () => void;
}) {
  const text = phase.kind === "streaming" || phase.kind === "done" ? phase.text : "";
  return (
    <div className="px-3 pb-1.5">
      <button
        type="button"
        aria-expanded={expanded}
        aria-label={expanded ? "Hide why these are related" : "Explain why these are related"}
        onClick={onToggle}
        className="flex items-center gap-1 text-[10px] text-[var(--text-dim)] hover:text-[var(--text-muted)]"
      >
        {expanded ? <ChevronDown size={10} /> : <ChevronRight size={10} />}
        Why?
      </button>
      {expanded && phase.kind === "queued" && (
        <p className="pl-3 pt-0.5 text-[11px] italic text-[var(--text-dim)]">Thinking…</p>
      )}
      {expanded && text && (
        <p className="pl-3 pt-0.5 text-[11px] leading-snug text-[var(--text-muted)]">
          {text}
          {phase.kind === "streaming" && <span className="animate-pulse">▍</span>}
        </p>
      )}
    </div>
  );
}

function StatusMessage({ children, error = false }: { children: string; error?: boolean }) {
  return <div className={`px-3 py-3 text-xs ${error ? "text-red-500" : "text-[var(--text-dim)]"}`}>{children}</div>;
}

function relatedDocumentDetails(entry: FileEntry): DocumentDetail[] {
  const document = entry as RelatedDocument;
  return [{
    key: "score",
    label: "Score",
    value: `${Math.round(document.score * 100)}%`,
    valueTitle: `${document.score.toFixed(3)} cosine similarity`,
    icon: Percent,
    monospace: true,
  }];
}

function sortRelatedDocuments(documents: RelatedDocument[]): RelatedDocument[] {
  return [...documents].sort(
    (a, b) =>
      b.score - a.score ||
      fileName(a.path).localeCompare(fileName(b.path), undefined, {
        numeric: true,
        sensitivity: "base",
      }) ||
      a.path.localeCompare(b.path),
  );
}
