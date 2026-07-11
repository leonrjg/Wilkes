import { useEffect, useRef, useState } from "react";
import { Percent, X } from "react-feather";
import { api } from "../services";
import type { FileEntry, RelatedDocument } from "../lib/types";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { fileName, type DocumentDetail } from "./DocumentEntryRow";
import ResultList from "./ResultList";
import { Tooltip } from "./Tooltip";

type RelatedStatus = "loading" | "ready" | "empty" | "error" | "unavailable";
const relatedDocumentsCache = new Map<string, RelatedDocument[]>();

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
  const relatedNavigationTargetRef = useRef<string | null>(null);

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
    const cacheKey = `${directory}\0${anchorPath}\0${indexKey}`;
    const cached = relatedDocumentsCache.get(cacheKey);
    if (cached) {
      setDocuments(cached);
      setStatus(cached.length > 0 ? "ready" : "empty");
      return;
    }

    let cancelled = false;
    setStatus("loading");
    setDocuments([]);
    api.relatedDocuments({ root: directory, path: anchorPath, limit: 8 })
      .then((result) => {
        if (cancelled) return;
        const sorted = sortRelatedDocuments(result);
        relatedDocumentsCache.set(cacheKey, sorted);
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
  }, [anchorPath, directory, indexReady, indexStatus?.model_id, indexStatus?.built_at]);

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
            documents={documents}
            preserveDocumentOrder
            documentDetails={relatedDocumentDetails}
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
