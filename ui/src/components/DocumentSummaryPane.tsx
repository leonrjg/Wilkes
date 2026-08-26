import { useCallback } from "react";
import { Check, Copy, RefreshCw, X } from "react-feather";
import { useGenerationStream } from "../hooks/useGenerationStream";
import { api } from "../services";
import { CopyButton } from "./CopyButton";
import { Tooltip } from "./preview";

interface Props {
  path: string;
  onClose: () => void;
}

export default function DocumentSummaryPane({ path, onClose }: Props) {
  const start = useCallback(
    (requestId: string) => api.summarizeDocument(requestId, path),
    [path],
  );
  const { phase, retry } = useGenerationStream({
    enabled: true,
    requestKey: path,
    task: "document_summary",
    start,
  });
  const text =
    phase.kind === "streaming" || phase.kind === "done" ? phase.text : "";

  return (
    <aside className="hidden w-64 flex-shrink-0 border-l border-[var(--border-main)] bg-[var(--bg-sidebar)] md:flex md:flex-col">
      <div className="flex items-center gap-1 border-b border-[var(--border-main)] px-3 py-2 text-xs font-medium text-[var(--text-main)]">
        <span className="min-w-0 flex-1 truncate">Summary</span>
        {phase.kind === "done" && (
          <Tooltip content="Copy summary">
            <CopyButton
              copy={() => api.writeClipboard(phase.text)}
              aria-label="Copy summary"
              copiedChildren={<Check size={14} />}
              className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              <Copy size={14} />
            </CopyButton>
          </Tooltip>
        )}
        {(phase.kind === "done" || phase.kind === "failed") && (
          <Tooltip content="Regenerate summary">
            <button
              type="button"
              onClick={retry}
              aria-label="Regenerate summary"
              className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              <RefreshCw size={14} />
            </button>
          </Tooltip>
        )}
        <Tooltip content="Close summary">
          <button
            type="button"
            onClick={onClose}
            aria-label="Close summary"
            className="inline-flex flex-shrink-0 rounded p-0.5 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
          >
            <X size={14} />
          </button>
        </Tooltip>
      </div>
      <div
        aria-live="polite"
        className="min-h-0 flex-1 overflow-y-auto px-3 py-3"
      >
        {phase.kind === "queued" && (
          <p className="text-xs italic text-[var(--text-dim)]">Thinking…</p>
        )}
        {text && (
          <p className="whitespace-pre-wrap text-xs leading-relaxed text-[var(--text-muted)]">
            {text}
            {phase.kind === "streaming" && (
              <span className="animate-pulse">▍</span>
            )}
          </p>
        )}
        {phase.kind === "failed" && (
          <div className="space-y-2" title={phase.error}>
            <p className="text-xs text-red-500">Summary unavailable</p>
            <button
              type="button"
              onClick={retry}
              className="rounded border border-[var(--border-main)] px-2 py-1 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
            >
              Try again
            </button>
          </div>
        )}
      </div>
    </aside>
  );
}
