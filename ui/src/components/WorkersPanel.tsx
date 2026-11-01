import { useCallback, useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type { PythonEnvInventory, WorkerStatus, Settings } from "../lib/types";
import { formatModelBytes } from "./ModelCatalog";

/** The worker roles the backend reports, in the order they are listed. */
const ROLE_LABELS: Record<string, string> = {
  embed: "Embedding worker",
  generate: "Generation worker",
  recognize: "Recognition worker",
};

function formatMicros(micros: number | null | undefined): string {
  if (micros == null) return "—";
  return `${(micros / 1000).toFixed(1)} ms`;
}

interface WorkersPanelProps {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

function formatLastUsed(lastUsed: string | null): string {
  return lastUsed ? `last used ${new Date(lastUsed).toLocaleDateString()}` : "never used";
}

/**
 * The Python environments models run in. Nothing here is ever removed without
 * being asked: a finished environment stays until the user takes it away, and
 * one taken away while a model needs it is rebuilt from its lock on next use.
 */
function PythonEnvironments({ api }: { api: WorkersPanelProps["api"] }) {
  const [inventory, setInventory] = useState<PythonEnvInventory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setInventory(await api.getPythonEnvs());
      setError(null);
    } catch (e: any) {
      setError(e.toString());
    }
  }, [api]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const remove = async (ids: string[]) => {
    setRemoving(true);
    try {
      await api.removePythonEnvs(ids);
      await refresh();
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setRemoving(false);
    }
  };

  const unused = (inventory?.envs ?? []).filter((env) => !env.is_base && env.models.length === 0);
  const buttonClass =
    "px-2 py-1 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-[10px] font-medium rounded transition-colors border border-[var(--border-main)] disabled:opacity-50";

  return (
    <section>
      <div className="flex items-center justify-between mb-2.5">
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">Python Environments</h3>
        {inventory && inventory.envs.length > 0 && (
          <span className="text-[10px] text-[var(--text-dim)]">
            {formatModelBytes(inventory.total_bytes)} in total
          </span>
        )}
      </div>
      <div className="bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg p-4 space-y-3">
        {error && (
          <div className="p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 font-mono break-all whitespace-pre-wrap">
            {error}
          </div>
        )}
        {inventory?.envs.length === 0 && (
          <p className="text-xs text-[var(--text-muted)]">
            None yet. One is set up the first time a Python model is used.
          </p>
        )}
        {inventory?.envs.map((env) => (
          <div key={env.id} className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <p className="text-xs text-[var(--text-main)]">
                Transformers {env.transformers} on Python {env.python}
                {env.is_base && <span className="ml-1.5 text-[10px] text-[var(--text-dim)]">base</span>}
              </p>
              <p className="text-[10px] text-[var(--text-dim)] break-words">
                {formatModelBytes(env.size_bytes)} · {formatLastUsed(env.last_used_at)}
                {env.packages.length > 0 && ` · with ${env.packages.join(", ")}`}
              </p>
              <p className="text-[10px] text-[var(--text-muted)] break-words">
                {env.is_base
                  ? ["Built-in models", ...env.models].join(", ")
                  : env.models.length > 0
                    ? env.models.join(", ")
                    : "Not used by any model"}
              </p>
            </div>
            <button
              type="button"
              disabled={removing}
              aria-label={`${env.is_base ? "Rebuild" : "Remove"} Transformers ${env.transformers} on Python ${env.python}`}
              title={
                env.is_base || env.models.length > 0
                  ? "Deleted now and built again the next time a model needs it."
                  : undefined
              }
              onClick={() => void remove([env.id])}
              className={buttonClass}
            >
              {env.is_base ? "Rebuild" : "Remove"}
            </button>
          </div>
        ))}
        {unused.length > 1 && (
          <button
            type="button"
            disabled={removing}
            onClick={() => void remove(unused.map((env) => env.id))}
            className={buttonClass}
          >
            Remove all unused
          </button>
        )}
      </div>
    </section>
  );
}

export default function WorkersPanel({ api, settings, onUpdateSettings }: WorkersPanelProps) {
  const [statuses, setStatuses] = useState<WorkerStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [timeoutSecs, setTimeoutSecs] = useState<string>(settings.semantic.worker_timeout_secs.toString());
  const [batchSize, setBatchSize] = useState<string>(settings.semantic.embed_batch_size.toString());

  const fetchStatus = async () => {
    try {
      // One row per role: two processes can die independently, so a single
      // status would misreport a dead generation worker as healthy.
      setStatuses(await api.getWorkerStatuses());
      setError(null);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  useEffect(() => {
    fetchStatus();
    const interval = setInterval(fetchStatus, 3000);
    return () => clearInterval(interval);
  }, [api]);

  const handleKill = async () => {
    try {
      await api.killWorker();
      await fetchStatus();
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const handleApplyTimeout = async () => {
    const secs = parseInt(timeoutSecs, 10);
    if (isNaN(secs) || secs < 0) {
      setError("Timeout must be a positive integer.");
      return;
    }
    
    try {
      await api.setWorkerTimeout(secs);
      // We also save this to settings so it persists across restarts
      await onUpdateSettings({
        semantic: {
          ...settings.semantic,
          worker_timeout_secs: secs
        }
      });
      await fetchStatus();
      setError(null);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  const handleApplyBatchSize = async () => {
    const size = parseInt(batchSize, 10);
    // Rejected rather than clamped: this is the control that decides whether a
    // build fits in memory, and silently substituting a number would leave the
    // user reading a value the worker is not using.
    if (isNaN(size) || size < 1) {
      setError("Embedding batch size must be a positive integer.");
      return;
    }

    try {
      await onUpdateSettings({
        semantic: {
          ...settings.semantic,
          embed_batch_size: size,
        },
      });
      setError(null);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  return (
    <div className="space-y-6">
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">Worker Status</h3>
        <div className="bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg p-4">
          {error && (
            <div className="mb-4 p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 font-mono break-all whitespace-pre-wrap">
              {error}
            </div>
          )}
          
          {statuses ? (
            <div className="space-y-6">
              {statuses.map((status, index) => (
            <div key={status.role ?? index} className="space-y-4">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <div className={`w-2 h-2 rounded-full ${status.active ? "bg-green-500 shadow-[0_0_8px_rgba(34,197,94,0.5)]" : "bg-[var(--text-dim)]"}`} />
                  <span className="text-sm font-medium text-[var(--text-main)]">
                    {status.role ? ROLE_LABELS[status.role] ?? status.role : "Worker"}
                  </span>
                  <span className="text-xs text-[var(--text-muted)]">
                    {status.active ? "Active" : "Idle"}
                  </span>
                </div>
                {/* Only the embedding worker has a kill endpoint; the button
                    on any other row would kill the embedder instead. */}
                {status.active && status.role === "embed" && (
                  <button
                    onClick={handleKill}
                    className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-red-500/20 text-[var(--text-main)] hover:text-red-400 text-xs font-medium rounded transition-colors border border-[var(--border-main)] hover:border-red-500/30"
                  >
                    Kill Worker
                  </button>
                )}
              </div>
              
              {status.active && (
                <div className="grid grid-cols-2 gap-4 text-xs">
                  <div>
                    <span className="text-[var(--text-muted)] block mb-1">Engine</span>
                    <span className="text-[var(--text-main)]">{status.engine || "Unknown"}</span>
                  </div>
                  <div>
                    <span className="text-[var(--text-muted)] block mb-1">Model</span>
                    <span className="text-[var(--text-main)] font-mono text-[10px]">{status.model || "Unknown"}</span>
                  </div>
                  <div>
                    <span className="text-[var(--text-muted)] block mb-1">Device</span>
                    <span className="text-[var(--text-main)]">
                      {status.device || (status.generation
                        ? `Initializing (${status.generation.requested_device})`
                        : "Unknown")}
                    </span>
                  </div>
                  <div>
                    <span className="text-[var(--text-muted)] block mb-1">Request</span>
                    <span className="text-[var(--text-main)] font-mono text-[10px]">{status.request_mode || "Unknown"}</span>
                  </div>
                  <div>
                    <span className="text-[var(--text-muted)] block mb-1">PID</span>
                    <span className="text-[var(--text-main)] font-mono text-[10px]">{status.pid ?? "Unknown"}</span>
                  </div>
                  {status.generation?.model_load_micros != null && (
                    <div>
                      <span className="text-[var(--text-muted)] block mb-1">Model load</span>
                      <span className="text-[var(--text-main)] font-mono text-[10px]">
                        {formatMicros(status.generation.model_load_micros)}
                      </span>
                    </div>
                  )}
                  {status.generation?.timings && (
                    <>
                      <div>
                        <span className="text-[var(--text-muted)] block mb-1">Prompt</span>
                        <span className="text-[var(--text-main)] font-mono text-[10px]">
                          {formatMicros(status.generation.timings.prompt_micros)}
                        </span>
                      </div>
                      <div>
                        <span className="text-[var(--text-muted)] block mb-1">Decode</span>
                        <span className="text-[var(--text-main)] font-mono text-[10px]">
                          {formatMicros(status.generation.timings.decode_micros)}
                        </span>
                      </div>
                      <div>
                        <span className="text-[var(--text-muted)] block mb-1">Constraint / GPU sync</span>
                        <span className="text-[var(--text-main)] font-mono text-[10px]">
                          {formatMicros(status.generation.timings.constraint_micros)}
                        </span>
                      </div>
                    </>
                  )}
                  {status.generation?.fallback_reason && (
                    <div className="col-span-2 rounded border border-amber-700/40 bg-amber-900/20 p-2 text-[10px] text-amber-300">
                      Requested {status.generation.requested_device}; using {status.device}:{" "}
                      {status.generation.fallback_reason}
                    </div>
                  )}
                </div>
              )}
            </div>
              ))}
            </div>
          ) : (
            <div className="text-sm text-[var(--text-muted)]">Loading status...</div>
          )}
        </div>
      </section>

      <PythonEnvironments api={api} />

      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">Configuration</h3>
        <div className="space-y-4">
          <div className="flex flex-col gap-1.5">
            <label className="text-xs text-[var(--text-muted)]">Idle Timeout (seconds)</label>
            <div className="flex gap-2">
              <input
                type="number"
                min="0"
                value={timeoutSecs}
                onChange={(e) => setTimeoutSecs(e.target.value)}
                className="w-32 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                placeholder="300"
              />
              <button
                aria-label="Apply idle timeout"
                onClick={handleApplyTimeout}
                className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-xs font-medium rounded transition-colors border border-[var(--border-main)]"
              >
                Apply
              </button>
            </div>
            <p className="text-[10px] text-[var(--text-dim)] italic">
              Release worker memory after this much idle time.
            </p>
          </div>

          <div className="flex flex-col gap-1.5">
            <label className="text-xs text-[var(--text-muted)]">Embedding Batch Size (texts)</label>
            <div className="flex gap-2">
              <input
                type="number"
                min="1"
                value={batchSize}
                onChange={(e) => setBatchSize(e.target.value)}
                className="w-32 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                placeholder="16"
              />
              <button
                aria-label="Apply embedding batch size"
                onClick={handleApplyBatchSize}
                className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-xs font-medium rounded transition-colors border border-[var(--border-main)]"
              >
                Apply
              </button>
            </div>
            <p className="text-[10px] text-[var(--text-dim)] italic">
              Larger batches can be faster but use more memory.
            </p>
          </div>
        </div>
      </section>
    </div>
  );
}
