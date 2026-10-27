import { useEffect, useMemo, useRef, useState } from "react";
import type { SearchApi } from "../services/api";
import type { HttpCall } from "../lib/types";
import { CopyButton } from "./CopyButton";

interface Props {
  api: SearchApi;
  isActive: boolean;
}

type StatusFilter = "all" | "pending" | "success" | "errors";
const control = "rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2 py-1 text-xs text-[var(--text-main)]";

export default function HttpRequestsPanel({ api, isActive }: Props) {
  const [calls, setCalls] = useState<HttpCall[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<StatusFilter>("all");
  const [clearing, setClearing] = useState(false);
  const revision = useRef(0);

  useEffect(() => {
    if (!isActive) return;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      const requested = revision.current;
      try {
        const result = await api.getHttpCalls();
        if (!disposed && requested === revision.current) {
          setCalls(result);
          setError(null);
        }
      } catch (e) {
        if (!disposed && requested === revision.current) setError(String(e));
      } finally {
        if (!disposed) timer = setTimeout(refresh, 1000);
      }
    };
    void refresh();
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [api, isActive]);

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return calls.filter((call) => {
      const pending = call.duration_ms === null;
      const failed = call.error !== null || (call.status !== null && call.status >= 400);
      if (filter === "pending" && !pending) return false;
      if (filter === "errors" && !failed) return false;
      if (filter === "success" && (pending || failed || call.status === null)) return false;
      return `${call.source} ${call.method} ${call.url} ${call.status ?? ""} ${call.error ?? ""}`.toLowerCase().includes(needle);
    });
  }, [calls, filter, query]);

  const clear = async () => {
    revision.current += 1;
    setClearing(true);
    try {
      await api.clearHttpCalls();
      revision.current += 1;
      setCalls([]);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setClearing(false);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-3 p-1">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-xs font-medium text-[var(--text-main)]">HTTP Requests</h3>
        <div className="flex gap-2">
          <CopyButton className={control} copiedChildren="Copied" copy={() => api.writeClipboard(JSON.stringify(visible, null, 2))}>Copy</CopyButton>
          <button type="button" className={control} disabled={clearing} onClick={() => void clear()}>Clear</button>
        </div>
      </div>
      <div className="flex gap-2">
        <input aria-label="Search HTTP requests" className={`${control} min-w-0 flex-1`} placeholder="Search URL, source, method or status" value={query} onChange={(event) => setQuery(event.target.value)} />
        <select aria-label="Filter HTTP requests" className={control} value={filter} onChange={(event) => setFilter(event.target.value as StatusFilter)}>
          <option value="all">All statuses</option>
          <option value="pending">Pending</option>
          <option value="success">Successful</option>
          <option value="errors">Errors</option>
        </select>
      </div>
      {error && <p role="alert" className="text-xs text-red-400">Could not update HTTP requests: {error}</p>}
      <p className="text-xs text-[var(--text-dim)]">{visible.length} of {calls.length} calls</p>
      <div className="min-h-0 flex-1 overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-input)]">
        {visible.length === 0 ? (
          <p className="p-3 text-xs text-[var(--text-dim)]">{calls.length === 0 ? "No HTTP requests recorded yet." : "No requests match these filters."}</p>
        ) : visible.map((call) => (
          <details key={call.id} className="border-b border-[var(--border-main)] p-2 text-xs last:border-b-0">
            <summary className="cursor-pointer break-all font-mono text-[var(--text-main)]">
              <span className={call.error || (call.status ?? 0) >= 400 ? "text-red-400" : "text-[var(--text-dim)]"}>
                {call.duration_ms === null ? "Pending" : call.status ?? "Failed"}
              </span>{" "}{call.method} {call.url}
              <span className="ml-2 text-[var(--text-dim)]">{call.source} · {new Date(call.started_at).toLocaleTimeString()}{call.duration_ms !== null && ` · ${call.duration_ms} ms`}</span>
            </summary>
            <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 break-all text-[var(--text-muted)]">
              <dt>Started</dt><dd>{new Date(call.started_at).toLocaleString()}</dd>
              <dt>Source</dt><dd>{call.source}</dd>
              <dt>Duration</dt><dd>{call.duration_ms === null ? "Waiting for response headers" : `${call.duration_ms} ms`}</dd>
              {call.error && <><dt>Error</dt><dd className="whitespace-pre-wrap text-red-400">{call.error}</dd></>}
            </dl>
          </details>
        ))}
      </div>
    </div>
  );
}
