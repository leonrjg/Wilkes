import { Fragment, useCallback } from "react";
import { Check, Copy, RefreshCw, X } from "react-feather";
import { useGenerationStream } from "../hooks/useGenerationStream";
import type {
  SearchResultsSummaryFile,
  SearchResultsSummaryInput,
} from "../lib/types";
import { api } from "../services";
import { useViewerStore } from "../stores/useViewerStore";
import { CopyButton } from "./CopyButton";
import { Tooltip } from "./Tooltip";

interface Props {
  input: SearchResultsSummaryInput;
  requestKey: string;
  onClose: () => void;
}

/**
 * Render the answer, turning each `[k]` the grammar emitted into a link that
 * opens the k-th source. `k` maps to `files[k - 1]` because the backend numbers
 * sources by the exact order it received them.
 */
function renderAnswer(
  text: string,
  files: SearchResultsSummaryFile[],
  openFile: (path: string) => void,
) {
  const nodes: React.ReactNode[] = [];
  const citation = /\[(\d+)\]/g;
  let cursor = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  while ((match = citation.exec(text)) !== null) {
    if (match.index > cursor) nodes.push(text.slice(cursor, match.index));
    const file = files[Number(match[1]) - 1];
    if (file) {
      nodes.push(
        <button
          key={`cite-${key++}`}
          type="button"
          onClick={() => openFile(file.path)}
          title={file.title}
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
}: Props) {
  const openFile = useViewerStore((state) => state.openFile);
  const start = useCallback(
    (requestId: string) => api.summarizeSearchResults(requestId, input),
    [input],
  );
  const { phase, retry } = useGenerationStream({
    enabled: true,
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
        {phase.kind === "queued" && (
          <p className="text-xs italic text-[var(--text-dim)]">Summarizing…</p>
        )}
        {text && (
          <p className="whitespace-pre-wrap text-xs leading-relaxed text-[var(--text-muted)]">
            {renderAnswer(text, input.files, openFile)}
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
        <p className="mt-2 text-[10px] leading-relaxed text-[var(--text-dim)]">
          {input.files.map((file, index) => (
            <Fragment key={file.path}>
              {index > 0 && " · "}
              <button
                type="button"
                onClick={() => openFile(file.path)}
                title={file.title}
                className="hover:text-[var(--text-main)] hover:underline"
              >
                [{index + 1}] {file.title}
              </button>
            </Fragment>
          ))}
        </p>
      </div>
    </section>
  );
}
