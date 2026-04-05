import { useState, useEffect } from "react";
import type { SearchApi, DataPaths } from "../services/api";
import type { IndexStatus } from "../lib/types";

interface Props {
  api: SearchApi;
}

export default function DataPanel({ api }: Props) {
  const [paths, setPaths] = useState<DataPaths | null>(null);
  const [indexStatus, setIndexStatus] = useState<IndexStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);

  const fetchData = async () => {
    try {
      const [p, idx] = await Promise.all([
        api.getDataPaths(),
        api.getIndexStatus().catch(() => null),
      ]);
      setPaths(p);
      setIndexStatus(idx);
    } catch (e: any) {
      setError(e.toString());
    }
  };

  useEffect(() => {
    fetchData();
  }, [api]);

  const onOpen = (path: string) => {
    api.openPath(path).catch((e) => setError(e.toString()));
  };

  const onDeleteIndex = async () => {
    if (!window.confirm("Are you sure you want to delete the semantic index database? This cannot be undone and will require a full reindex.")) {
      return;
    }
    setIsDeleting(true);
    try {
      await api.deleteIndex();
      await fetchData();
    } catch (e: any) {
      setError(e.toString());
    } finally {
      setIsDeleting(false);
    }
  };

  const formatBytes = (bytes: number): string => {
    if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(2)} GB`;
    if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(2)} MB`;
    return `${Math.round(bytes / 1024)} KB`;
  };

  if (error) {
    return (
      <div className="p-4 bg-red-900/20 border border-red-900/50 rounded-lg">
        <p className="text-xs text-red-400 leading-relaxed">{error}</p>
        <button 
          onClick={() => { setError(null); fetchData(); }}
          className="mt-2 text-[10px] text-red-400 underline hover:text-red-300"
        >
          Try again
        </button>
      </div>
    );
  }

  if (!paths) {
    return (
      <div className="flex items-center justify-center h-32">
        <div className="w-5 h-5 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Semantic Index Database</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Stores chunks and vector embeddings for semantic search.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          {indexStatus ? (
            <>
              <div className="grid grid-cols-2 gap-4">
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Status</span>
                  <span className="text-[10px] text-green-500 font-medium">Ready ({indexStatus.indexed_files} files)</span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Size on Disk</span>
                  <span className="text-[10px] text-[var(--text-main)] font-mono">
                    {indexStatus.db_size_bytes ? formatBytes(indexStatus.db_size_bytes) : "Unknown"}
                  </span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Total Chunks</span>
                  <span className="text-[10px] text-[var(--text-main)] font-mono">{indexStatus.total_chunks.toLocaleString()}</span>
                </div>
                <div className="flex flex-col gap-1">
                  <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Model</span>
                  <span className="text-[10px] text-[var(--text-main)] truncate" title={indexStatus.model_id}>{indexStatus.model_id.split('/').pop()}</span>
                </div>
              </div>
              
              <div className="flex flex-col gap-1">
                <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Path</span>
                <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
                  {paths.app_data}/semantic_index.db
                </span>
              </div>

              <div className="flex gap-2 mt-1">
                <button
                  onClick={() => onOpen(paths.app_data)}
                  className="px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
                >
                  Open in File Manager
                </button>
                <button
                  onClick={onDeleteIndex}
                  disabled={isDeleting}
                  className="px-3 py-1.5 bg-red-900/20 hover:bg-red-900/40 text-red-400 text-[10px] font-bold uppercase tracking-wider rounded border border-red-900/50 transition-colors disabled:opacity-50"
                >
                  {isDeleting ? "Deleting..." : "Delete Database"}
                </button>
              </div>
            </>
          ) : (
            <div className="py-4 text-center">
              <p className="text-xs text-[var(--text-dim)] italic">No semantic index built yet.</p>
              <p className="text-[10px] text-[var(--text-dim)] mt-1">Configure and build your index in the Models page.</p>
            </div>
          )}
        </div>
      </section>

      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Application Data</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Local storage for settings, logs, and cache.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Path</span>
            <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
              {paths.app_data}
            </span>
          </div>
          <button
            onClick={() => onOpen(paths.app_data)}
            className="w-fit px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
          >
            Open in File Manager
          </button>
        </div>
      </section>

      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">HuggingFace Cache</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Where SBERT models are downloaded and stored.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">Path</span>
            <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">
              {paths.hf_cache}
            </span>
          </div>
          <button
            onClick={() => onOpen(paths.hf_cache)}
            className="w-fit px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors"
          >
            Open in File Manager
          </button>
        </div>
      </section>
    </div>
  );
}
