import { useState, useEffect } from "react";
import type { SearchApi, DataPaths } from "../services/api";

interface Props {
  api: SearchApi;
}

export default function DataPanel({ api }: Props) {
  const [paths, setPaths] = useState<DataPaths | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.getDataPaths()
      .then(setPaths)
      .catch((e) => setError(e.toString()));
  }, [api]);

  const onOpen = (path: string) => {
    api.openPath(path).catch((e) => setError(e.toString()));
  };

  if (error) {
    return (
      <div className="p-4 bg-red-900/20 border border-red-900/50 rounded-lg">
        <p className="text-xs text-red-400 leading-relaxed">{error}</p>
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
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Application Data</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Local storage for settings, logs, and semantic indices.
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
