import { Fragment, useCallback } from "react";
import { Check, Copy, MessageCircle, RefreshCw, X } from "react-feather";
import { useGenerationStream } from "../hooks/useGenerationStream";
import type {
  SearchResultsSummaryInput,
  SearchResultsSummarySource,
} from "../lib/types";
import { api } from "../services";
import { useViewerStore } from "../stores/useViewerStore";
import { CopyButton } from "./CopyButton";
import { Tooltip } from "./Tooltip";

interface Props {
  input: SearchResultsSummaryInput;
  requestKey: string;
  onClose: () => void;
  onExplore?: () => void;
}

/**
 * Render the synthesized answer, turning each constrained `[k]` citation into
 * a link to `sources[k - 1]`.
 */
function renderAnswer(
  text: string,
  sources: SearchResultsSummarySource[],
  openFile: (path: string) => void,
) {
  const nodes: React.ReactNode[] = [];
  const citation = /\[(\d+)\]/g;
  let cursor = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  while ((match = citation.exec(text)) !== null) {
    if (match.index > cursor) nodes.push(text.slice(cursor, match.index));
    const source = sources[Number(match[1]) - 1];
    if (source) {
      nodes.push(
        <button
          key={`cite-${key++}`}
          type="button"
          onClick={() => openFile(source.path)}
          title={source.title}
          className="mx-0.5 rounded bg-[var(--bg-active)] px-1 font-medium text-[var(--text-main)] hover:bg-[var(--bg-hover)]"
        >
          {match[0]}
        </button>,
      );
    } else {
      nodes.push(match[0]);
    }
    cursor = match.index + match[0].length;
  }
  if (cursor < text.length) nodes.push(text.slice(cursor));
  return nodes;
}

export default function SearchResultsSummary({
  input,
  requestKey,
  onClose,
  onExplore,
}: Props) {
  const openFile = useViewerStore((state) => state.openFile);
  const start = useCallback(
    (requestId: string) => api.summarizeSearchResults(requestId, input),
    [input],
  );
  const hasPassages = input.passages.length > 0;
  const { phase, retry } = useGenerationStream({
    enabled: hasPassages,
    requestKey,
    task: "search_results_summary",
    start,
  });
  const text =
    phase.kind === "streaming" || phase.kind === "done" ? phase.text : "";

  return (
    <section className="flex max-h-64 flex-shrink-0 flex-col border-b border-[var(--border-main)] bg-[var(--bg-sidebar)]">
      <div className="flex items-center gap-1 px-3 py-2 text-xs font-medium text-[var(--text-main)]">
        <span className="min-w-0 flex-1 truncate">Results summary</span>
        {phase.kind === "done" && (
          <Tooltip content="Copy results summary">
            <CopyButton
              copy={() => api.writeClipboard(phase.text)}
              aria-label="Copy results summary"
              copiedChildren={<Check size={14} />}
              className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              <Copy size={14} />
            </CopyButton>
          </Tooltip>
        )}
        {(phase.kind === "done" || phase.kind === "failed") && (
          <Tooltip content="Regenerate results summary">
            <button
              type="button"
              onClick={retry}
              aria-label="Regenerate results summary"
              className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              <RefreshCw size={14} />
            </button>
          </Tooltip>
        )}
        <Tooltip content="Close results summary">
          <button
            type="button"
            onClick={onClose}
            aria-label="Close results summary"
            className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
          >
            <X size={14} />
          </button>
        </Tooltip>
      </div>
      <div aria-live="polite" className="min-h-0 overflow-y-auto px-3 pb-3">
        {!hasPassages && (
          <p className="text-xs leading-relaxed text-[var(--text-muted)]">
            No substantive passage in these results directly addresses the query.
          </p>
        )}
        {hasPassages && phase.kind === "queued" && (
          <p className="text-xs italic text-[var(--text-dim)]">Summarizing…</p>
        )}
        {text && (
          <p className="whitespace-pre-wrap text-xs leading-relaxed text-[var(--text-muted)]">
            {renderAnswer(text, input.sources, openFile)}
            {phase.kind === "streaming" && <span className="animate-pulse">▍</span>}
          </p>
        )}
        {phase.kind === "failed" && (
          <div className="space-y-2" title={phase.error}>
            <p className="text-xs text-red-500">Results summary unavailable</p>
            <button
              type="button"
              onClick={retry}
              className="rounded border border-[var(--border-main)] px-2 py-1 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              Try again
            </button>
          </div>
        )}
        {input.sources.length > 0 && (
          <p className="mt-2 text-[10px] leading-relaxed text-[var(--text-dim)]">
            {input.sources.map((source, index) => (
              <Fragment key={source.path}>
                {index > 0 && " · "}
                <button
                  type="button"
                  onClick={() => openFile(source.path)}
                  title={source.title}
                  className="hover:text-[var(--text-main)] hover:underline"
                >
                  [{index + 1}] {source.title}
                </button>
              </Fragment>
            ))}
          </p>
        )}
        {onExplore && (
          <button
            type="button"
            onClick={onExplore}
            className="mt-2 inline-flex items-center gap-1 rounded border border-[var(--border-main)] px-2 py-1 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
          >
            <MessageCircle size={12} aria-hidden="true" />
            Explore results in agent chat
          </button>
        )}
      </div>
    </section>
  );
}
