import { useCallback, useEffect, useRef, useState } from "react";
import type { SearchApi } from "../services/api";
import type {
  DocumentOutcome,
  DocumentStage,
  IndexActivity,
  JobDocument,
  JobState,
  JobSummary,
  Settings,
} from "../lib/types";
import { useSettingsStore } from "../stores/useSettingsStore";
import WorkersPanel from "./WorkersPanel";

/**
 * What a long indexing job is doing, document by document.
 *
 * The single source is the backend's job journal: every event on the progress
 * stream is treated as a signal to re-read it, never as a fact to accumulate
 * locally. That is what makes this view identical whether it was open for the
 * whole build or opened for the first time an hour after the app was restarted.
 */

/** How long a job's own stream may go unread before the view refetches. */
const LIVE_REFRESH_MS = 700;
/** How often a running job is checked even if no event arrives. */
const POLL_MS = 4000;

const STAGE_LABELS: Record<DocumentStage, string> = {
  queued: "Queued",
  checking: "Checking",
  reading_figures: "Reading figures",
  extracting: "Extracting",
  embedding: "Embedding",
};

const OUTCOME_LABELS: Record<DocumentOutcome, string> = {
  pending: "Waiting",
  reused: "Unchanged",
  indexed: "Indexed",
  empty: "No text",
  failed: "Failed",
};

const OUTCOME_STYLES: Record<DocumentOutcome, string> = {
  pending: "text-[var(--text-muted)]",
  reused: "text-[var(--text-muted)]",
  indexed: "text-green-400",
  empty: "text-[var(--text-dim)]",
  failed: "text-red-400",
};

const STATE_LABELS: Record<JobState, string> = {
  running: "Running",
  completed: "Completed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
  failed: "Failed",
};

const STATE_STYLES: Record<JobState, string> = {
  running: "bg-blue-500/15 text-blue-300 border-blue-500/30",
  completed: "bg-green-500/15 text-green-300 border-green-500/30",
  cancelled: "bg-amber-500/15 text-amber-300 border-amber-500/30",
  interrupted: "bg-amber-500/15 text-amber-300 border-amber-500/30",
  failed: "bg-red-500/15 text-red-300 border-red-500/30",
};

function basename(path: string): string {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

function formatWhen(ms: number | null): string {
  if (ms == null) return "";
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatDuration(job: JobSummary): string {
  const end = job.ended_at_ms ?? Date.now();
  const seconds = Math.max(0, Math.round((end - job.started_at_ms) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/**
 * What the job is, in one sentence, for a reader who was not watching.
 *
 * A stopped job says what it saved before it stopped, because "cancelled" on
 * its own reads as "nothing happened" and that is exactly what is no longer
 * true of a cancelled build.
 */
export function describeJob(job: JobSummary): string {
  const saved = job.counts.reused + job.counts.indexed;
  const settled = saved + job.counts.empty + job.counts.failed;
  switch (job.state) {
    case "running":
      return `Reading ${job.total_documents} documents — ${settled} done, ${saved} saved so far.`;
    case "completed":
      return `Read all ${job.total_documents} documents. ${saved} saved${
        job.counts.failed > 0 ? `, ${job.counts.failed} failed` : ""
      }.`;
    case "cancelled":
      return `Stopped after ${settled} of ${job.total_documents} documents. ${saved} were saved and do not need reading again.`;
    case "interrupted":
      return `Interrupted after ${settled} of ${job.total_documents} documents. ${saved} were saved and do not need reading again.`;
    case "failed":
      return `Failed after ${settled} of ${job.total_documents} documents. ${saved} were saved and do not need reading again.`;
  }
}

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
  isActive: boolean;
}

export default function IndexActivityPanel({
  api,
  settings,
  onUpdateSettings,
  isActive,
}: Props) {
  const directory = useSettingsStore((s) => s.directory);
  const root = directory ?? settings.last_directory;
  const [activity, setActivity] = useState<IndexActivity | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<null | "continue" | "retry">(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showDiagnostics, setShowDiagnostics] = useState(false);
  const lastRefresh = useRef(0);

  const refresh = useCallback(async () => {
    if (!root) {
      setActivity(null);
      return;
    }
    lastRefresh.current = Date.now();
    try {
      setActivity(await api.indexActivity(root));
      setError(null);
    } catch (e: any) {
      setError(e?.toString?.() ?? "Could not read the indexing activity");
    }
  }, [api, root]);

  useEffect(() => {
    if (!isActive) return;
    void refresh();
  }, [isActive, refresh]);

  // The progress stream says *that* something changed; the journal says what.
  // Reading it back rather than accumulating events locally is what lets this
  // view be correct when it is opened after the fact.
  useEffect(() => {
    if (!isActive) return;
    let unlisten: (() => void) | undefined;
    let stopped = false;
    api
      .onEmbedProgress((p) => {
        if (stopped || !("Build" in p)) return;
        if (Date.now() - lastRefresh.current < LIVE_REFRESH_MS) return;
        void refresh();
      })
      .then((u) => {
        if (stopped) u();
        else unlisten = u;
      })
      .catch(() => {});
    return () => {
      stopped = true;
      if (unlisten) unlisten();
    };
  }, [api, isActive, refresh]);

  useEffect(() => {
    if (!isActive || activity?.job?.state !== "running") return;
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [isActive, activity?.job?.state, refresh]);

  const act = async (which: "continue" | "retry") => {
    if (!root) return;
    setBusy(which);
    setNotice(null);
    try {
      if (which === "continue") {
        await api.continueIndexJob(root, settings.semantic.selected);
        setNotice("Continuing with the documents that were not read.");
      } else {
        await api.retryFailedDocuments(root, settings.semantic.selected);
        setNotice("Re-attempting the documents that failed.");
      }
      await refresh();
    } catch (e: any) {
      setError(e?.toString?.() ?? "The action could not be started");
    } finally {
      setBusy(null);
    }
  };

  const job = activity?.job ?? null;
  const failed = job?.counts.failed ?? 0;
  const remaining = job?.counts.pending ?? 0;
  const settled = job
    ? job.counts.reused + job.counts.indexed + job.counts.empty + job.counts.failed
    : 0;
  const percent =
    job && job.total_documents > 0
      ? Math.min(100, Math.round((settled / job.total_documents) * 100))
      : 0;
  // Offered only when the job has actually stopped: continuing a running build
  // would start a second one over the same documents.
  const canAct = job != null && job.state !== "running";

  return (
    <div className="space-y-6">
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Indexing Activity
        </h3>
        <div className="bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg p-4 space-y-4">
          {error && (
            <div
              role="alert"
              className="p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 font-mono break-all whitespace-pre-wrap"
            >
              {error}
            </div>
          )}

          {!root && (
            <p className="text-sm text-[var(--text-muted)]">
              Choose a directory to see what indexing it has had.
            </p>
          )}

          {root && !job && !error && (
            <p className="text-sm text-[var(--text-muted)]">
              No indexing job has been recorded for this directory yet.
            </p>
          )}

          {job && (
            <>
              <div className="flex items-center justify-between gap-3 flex-wrap">
                <div className="flex items-center gap-2">
                  <span
                    className={`px-2 py-0.5 rounded border text-[10px] font-medium ${STATE_STYLES[job.state]}`}
                  >
                    {STATE_LABELS[job.state]}
                  </span>
                  <span
                    className="text-xs text-[var(--text-muted)] font-mono truncate max-w-[24rem]"
                    title={job.root}
                  >
                    {job.root}
                  </span>
                </div>
                <span className="text-[10px] text-[var(--text-dim)]">
                  {formatWhen(job.started_at_ms)} · {formatDuration(job)}
                </span>
              </div>

              <p className="text-sm text-[var(--text-main)]">{describeJob(job)}</p>

              <div>
                <div className="h-1.5 w-full bg-[var(--bg-active)] rounded overflow-hidden">
                  <div
                    className="h-full bg-[var(--accent-blue)] transition-all"
                    style={{ width: `${percent}%` }}
                  />
                </div>
                <div className="mt-2 flex gap-4 text-[10px] text-[var(--text-muted)] flex-wrap">
                  <span>
                    <span className="text-green-400">{job.counts.indexed}</span> indexed
                  </span>
                  <span>
                    <span className="text-[var(--text-main)]">{job.counts.reused}</span> unchanged
                  </span>
                  <span>
                    <span className="text-[var(--text-main)]">{job.counts.empty}</span> no text
                  </span>
                  <span>
                    <span className={failed > 0 ? "text-red-400" : ""}>{failed}</span> failed
                  </span>
                  <span>
                    <span className="text-[var(--text-main)]">{remaining}</span> not read
                  </span>
                </div>
              </div>

              {job.detail && (
                <div className="p-2 rounded border border-red-900/50 bg-red-900/20 text-[10px] text-red-300 font-mono whitespace-pre-wrap break-all">
                  {job.detail}
                </div>
              )}

              {notice && (
                <p className="text-[10px] text-[var(--text-muted)] italic">{notice}</p>
              )}

              {canAct && (remaining > 0 || failed > 0) && (
                <div className="flex gap-2 flex-wrap">
                  {remaining > 0 && (
                    <button
                      onClick={() => act("continue")}
                      disabled={busy !== null}
                      className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-xs font-medium rounded transition-colors border border-[var(--border-main)] disabled:opacity-50"
                    >
                      {busy === "continue"
                        ? "Starting..."
                        : `Continue with ${remaining} unread document${remaining === 1 ? "" : "s"}`}
                    </button>
                  )}
                  {failed > 0 && (
                    <button
                      onClick={() => act("retry")}
                      disabled={busy !== null}
                      className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-red-500/20 text-[var(--text-main)] hover:text-red-400 text-xs font-medium rounded transition-colors border border-[var(--border-main)] hover:border-red-500/30 disabled:opacity-50"
                    >
                      {busy === "retry"
                        ? "Starting..."
                        : `Retry ${failed} failed document${failed === 1 ? "" : "s"}`}
                    </button>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      </section>

      {activity && activity.documents.length > 0 && (
        <section>
          <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
            Documents
          </h3>
          <div className="bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg divide-y divide-[var(--border-main)] max-h-80 overflow-y-auto">
            {activity.documents.map((doc) => (
              <DocumentRow key={doc.path} doc={doc} />
            ))}
          </div>
          {job && job.total_documents > activity.documents.length && (
            <p className="mt-2 text-[10px] text-[var(--text-dim)] italic">
              Showing {activity.documents.length} of {job.total_documents} documents; those needing
              attention are listed first.
            </p>
          )}
        </section>
      )}

      {activity && activity.history.length > 1 && (
        <section>
          <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
            Earlier Jobs
          </h3>
          <div className="bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg divide-y divide-[var(--border-main)]">
            {activity.history.slice(1).map((past) => (
              <div key={past.id} className="flex items-center justify-between gap-3 px-3 py-2">
                <span
                  className={`px-2 py-0.5 rounded border text-[10px] font-medium shrink-0 ${STATE_STYLES[past.state]}`}
                >
                  {STATE_LABELS[past.state]}
                </span>
                <span className="text-[10px] text-[var(--text-muted)] flex-1 truncate">
                  {past.counts.reused + past.counts.indexed} of {past.total_documents} saved
                  {past.counts.failed > 0 ? `, ${past.counts.failed} failed` : ""}
                </span>
                <span className="text-[10px] text-[var(--text-dim)] shrink-0">
                  {formatWhen(past.started_at_ms)}
                </span>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Beneath the job, not beside it: the per-process detail matters once a
          document has stopped moving and the question becomes which model is
          holding it up. */}
      <section>
        <button
          onClick={() => setShowDiagnostics((open) => !open)}
          aria-expanded={showDiagnostics}
          className="flex items-center gap-1.5 text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider hover:text-[var(--text-main)] transition-colors"
        >
          <span className={showDiagnostics ? "rotate-90 transition-transform" : "transition-transform"}>
            ▸
          </span>
          Worker Diagnostics
        </button>
        {showDiagnostics && (
          <div className="mt-3">
            <WorkersPanel api={api} settings={settings} onUpdateSettings={onUpdateSettings} />
          </div>
        )}
      </section>
    </div>
  );
}

function DocumentRow({ doc }: { doc: JobDocument }) {
  const label =
    doc.outcome === "pending" ? STAGE_LABELS[doc.stage] : OUTCOME_LABELS[doc.outcome];
  return (
    <div className="px-3 py-2">
      <div className="flex items-center justify-between gap-3">
        <span
          className="text-xs text-[var(--text-main)] truncate font-mono"
          title={doc.path}
        >
          {basename(doc.path)}
        </span>
        <span className={`text-[10px] shrink-0 ${OUTCOME_STYLES[doc.outcome]}`}>
          {label}
          {doc.outcome === "indexed" && doc.chunks != null ? ` · ${doc.chunks} passages` : ""}
        </span>
      </div>
      {doc.error && (
        <p className="mt-1 text-[10px] text-red-400/90 font-mono break-all whitespace-pre-wrap">
          {doc.error}
        </p>
      )}
    </div>
  );
}
