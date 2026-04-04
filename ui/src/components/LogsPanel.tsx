import { useState, useEffect, useRef } from "react";
import type { SearchApi } from "../services/api";

interface LogsPanelProps {
  api: SearchApi;
}

export default function LogsPanel({ api }: LogsPanelProps) {
  const [logs, setLogs] = useState<string[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let mounted = true;

    const fetchLogs = async () => {
      try {
        const newLogs = await api.getLogs();
        if (mounted) {
          setLogs(newLogs);
        }
      } catch (e) {
        console.error("Failed to fetch logs:", e);
      }
    };

    fetchLogs();
    const interval = setInterval(fetchLogs, 3000);

    return () => {
      mounted = false;
      clearInterval(interval);
    };
  }, [api]);

  const handleCopy = () => {
    navigator.clipboard.writeText(logs.join("\n"));
  };

  const handleClear = async () => {
    if (confirm("Are you sure you want to clear the logs?")) {
      try {
        await api.clearLogs();
        setLogs([]);
      } catch (e) {
        console.error("Failed to clear logs:", e);
      }
    }
  };

  return (
    <div className="flex flex-col h-full gap-3 p-1">
      <div className="flex items-center justify-between">
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">
          System Logs
        </h3>
        <div className="flex items-center gap-2">
          <button
            onClick={handleCopy}
            className="text-[10px] px-2 py-1 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] rounded border border-[var(--border-main)] transition-colors"
          >
            Copy
          </button>
          <button
            onClick={handleClear}
            className="text-[10px] px-2 py-1 bg-[var(--bg-active)] hover:bg-red-500/10 hover:text-red-500 text-[var(--text-main)] rounded border border-[var(--border-main)] transition-colors"
          >
            Clear
          </button>
        </div>
      </div>

      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto bg-[var(--bg-input)] rounded-lg p-4 font-mono text-[11px] leading-relaxed text-[var(--text-muted)] border border-[var(--border-main)] flex flex-col-reverse"
      >
        <div className="flex flex-col">
          {logs.length === 0 ? (
            <div className="text-[var(--text-dim)] italic">No logs available.</div>
          ) : (
            logs.map((line, i) => (
              <div key={i} className="whitespace-pre-wrap break-all mb-1 last:mb-0">
                {line}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
