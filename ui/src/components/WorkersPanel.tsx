import { useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type { WorkerStatus, Settings } from "../lib/types";

interface WorkersPanelProps {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

export default function WorkersPanel({ api, settings, onUpdateSettings }: WorkersPanelProps) {
  const [status, setStatus] = useState<WorkerStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [timeoutSecs, setTimeoutSecs] = useState<string>(settings.semantic.worker_timeout_secs.toString());

  const fetchStatus = async () => {
    try {
      const s = await api.getWorkerStatus();
      setStatus(s);
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
          
          {status ? (
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <div className={`w-2 h-2 rounded-full ${status.active ? "bg-green-500 shadow-[0_0_8px_rgba(34,197,94,0.5)]" : "bg-[var(--text-dim)]"}`} />
                  <span className="text-sm font-medium text-[var(--text-main)]">
                    {status.active ? "Active" : "Idle"}
                  </span>
                </div>
                {status.active && (
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
                </div>
              )}
            </div>
          ) : (
            <div className="text-sm text-[var(--text-muted)]">Loading status...</div>
          )}
        </div>
      </section>

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
                onClick={handleApplyTimeout}
                className="px-3 py-1.5 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-xs font-medium rounded transition-colors border border-[var(--border-main)]"
              >
                Apply
              </button>
            </div>
            <p className="text-[10px] text-[var(--text-dim)] italic">
              Worker processes will be shut down after this duration of inactivity to free system resources.
            </p>
          </div>
        </div>
      </section>
    </div>
  );
}
