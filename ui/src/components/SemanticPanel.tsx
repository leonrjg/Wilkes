import { useState, useEffect, useCallback, useMemo } from "react";
import type {
  EmbedderModel,
  EmbedProgress,
  EmbedDone,
  EmbedError,
  ModelDescriptor,
  SemanticSettings,
  IndexStatus,
  EmbeddingEngine,
} from "../lib/types";
import type { SearchApi } from "../services/api";

type Phase = "not_downloaded" | "downloading" | "ready" | "building" | "indexed" | "engine_mismatch";

interface ProgressState {
  current: number;
  total: number;
}

interface Props {
  api: SearchApi;
  directory: string;
  refreshSemanticReady: () => Promise<void>;
}

export default function SemanticPanel({ api, directory, refreshSemanticReady }: Props) {
  const [settings, setSettings] = useState<SemanticSettings | null>(null);
  const [phase, setPhase] = useState<Phase>("not_downloaded");
  const [progress, setProgress] = useState<ProgressState | null>(null);
  const [indexStatus, setIndexStatus] = useState<IndexStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isCancelling, setIsCancelling] = useState(false);
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [modelFilter, setModelFilter] = useState("");
  const [sizeFetchingFor, setSizeFetchingFor] = useState<string | null>(null);

  const refreshState = useCallback(async () => {
    try {
      const s = await api.getSettings();
      const sem = s.semantic;
      setSettings(sem);

      const descriptors = await api.listModels(sem.engine);
      setModels(descriptors);

      const selected = descriptors.find((m) => m.model_id === sem.model);
      
      // Check if there is an existing index
      try {
        const status = await api.getIndexStatus();
        setIndexStatus(status);

        // Check for engine/model mismatch with existing index
        if (status.engine !== sem.engine || status.model_id !== sem.model) {
          setPhase("engine_mismatch");
          return;
        }
        
        setPhase("indexed");
      } catch (e) {
        // No index or error reading it
        setIndexStatus(null);
        if (!selected?.is_cached) {
          setPhase("not_downloaded");
        } else {
          setPhase("ready");
        }
      }
    } catch (e) {
      console.error("refreshState failed:", e);
    }
  }, [api]);

  useEffect(() => {
    refreshState();
  }, [refreshState]);

  // Subscribe to embed events.
  useEffect(() => {
    let mounted = true;
    const unlisteners: Array<() => void> = [];

    api
      .onEmbedProgress((p: EmbedProgress) => {
        if ("Download" in p) {
          setPhase("downloading");
          setProgress({ current: p.Download.bytes_received, total: p.Download.total_bytes });
        } else if ("Build" in p) {
          setPhase("building");
          setProgress({ current: p.Build.files_processed, total: p.Build.total_files });
        }
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedProgress subscription failed:", e));

    api
      .onEmbedDone((done: EmbedDone) => {
        setProgress(null);
        setError(null);
        setIsCancelling(false);
        if (done.operation === "Download") {
          setPhase("ready");
          refreshState();
        } else if (done.operation === "Build") {
          setPhase("indexed");
          api.getIndexStatus().then(setIndexStatus).catch(console.error);
          refreshSemanticReady();
        }
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedDone subscription failed:", e));

    api
      .onEmbedError((err: EmbedError) => {
        setIsCancelling(false);
        if (err.message) console.error(`Embed error (${err.operation}):`, err.message);
        setError(err.message || null);
        setProgress(null);
        setPhase((prev) => {
          if (prev === "downloading") return "not_downloaded";
          if (prev === "building") return "ready";
          return prev;
        });
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedError subscription failed:", e));

    return () => {
      mounted = false;
      unlisteners.forEach((u) => u());
    };
  }, [api, refreshState, refreshSemanticReady]);

  const handleEngineChange = async (engine: EmbeddingEngine) => {
    if (!settings) return;
    const next = { ...settings, engine, enabled: false, index_path: null };
    setSettings(next);
    setModels([]);
    setModelFilter("");
    await api.updateSettings({ semantic: next });
    await refreshState();
  };

  const handleModelChange = useCallback(
    async (modelId: EmbedderModel) => {
      if (!settings) return;
      const next = { ...settings, model: modelId };
      setSettings(next);
      const descriptor = models.find((m) => m.model_id === modelId);
      
      // If switching models, we disable the index until rebuilt
      const updatePatch: Partial<SemanticSettings> = { ...next, enabled: descriptor?.is_cached ?? false };
      await api.updateSettings({ semantic: updatePatch as SemanticSettings });
      
      refreshState();

      // Fetch download size lazily when expanding an uncached model.
      if (descriptor && !descriptor.is_cached && descriptor.size_bytes === null) {
        setSizeFetchingFor(modelId);
        try {
          const size = await api.getModelSize(settings.engine, modelId);
          setModels((prev) =>
            prev.map((m) => (m.model_id === modelId ? { ...m, size_bytes: size } : m)),
          );
        } catch (e) {
          console.error(`getModelSize(${modelId}) failed:`, e);
        } finally {
          setSizeFetchingFor(null);
        }
      }
    },
    [settings, models, api, refreshState],
  );

  const handleAction = useCallback(async () => {
    setError(null);
    if (!settings) return;

    if (phase === "not_downloaded") {
      api.downloadModel(settings.model, settings.engine).catch((e) => console.error("downloadModel failed:", e));
    } else if (phase === "downloading" || phase === "building") {
      setIsCancelling(true);
      api.cancelEmbed().catch((e) => { setIsCancelling(false); console.error("cancelEmbed failed:", e); });
    } else if (phase === "ready" || phase === "engine_mismatch") {
      api
        .buildIndex(directory, settings.model, settings.engine)
        .catch((e) => console.error("buildIndex failed:", e));
    } else if (phase === "indexed") {
      api
        .deleteIndex()
        .then(() => {
          setPhase("ready");
          setIndexStatus(null);
          refreshSemanticReady();
        })
        .catch((e) => console.error("deleteIndex failed:", e));
    }
  }, [phase, settings, api, directory, refreshSemanticReady]);

  const isActive = phase === "downloading" || phase === "building";

  const formatBytes = (bytes: number): string => {
    if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
    return `${Math.round(bytes / 1_048_576)} MB`;
  };
  const progressPct =
    progress && progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;

  const filteredModels = useMemo(() => {
    const search = modelFilter.trim().toLowerCase();
    if (!modelFilter) return models;
    if (!search) return [];
    return models.filter(
      (m) =>
        m.model_id.toLowerCase().includes(search) ||
        m.display_name.toLowerCase().includes(search) ||
        m.description.toLowerCase().includes(search),
    );
  }, [models, modelFilter]);

  return (
    <div className="flex flex-col gap-4">
      {/* Engine Selection */}
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2 uppercase tracking-wider">Inference Engine</h3>
        <div className="flex p-0.5 bg-[var(--bg-active)] rounded-lg w-full">
          {(["Candle", "Fastembed"] as const).map((e) => (
            <button
              key={e}
              type="button"
              disabled={isActive}
              onClick={() => handleEngineChange(e)}
              className={`flex-1 px-3 py-1 rounded-md text-xs transition-all ${
                settings?.engine === e
                  ? "bg-[var(--bg-app)] text-[var(--text-main)] shadow-sm"
                  : "text-[var(--text-muted)] hover:text-[var(--text-main)] disabled:opacity-50"
              }`}
            >
              {e === "Candle" ? "Candle (Metal)" : "Fastembed (ONNX)"}
            </button>
          ))}
        </div>
        <p className="text-[10px] text-[var(--text-dim)] mt-1.5 px-1 selectable">
          {settings?.engine === "Candle" 
            ? "Native Rust implementation. Uses GPU via Metal." 
            : "Optimized ONNX Runtime. Uses CoreML acceleration (fastest)."}
        </p>
      </section>

      {/* Model list */}
      <section className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">Embedding Model</h3>
          <span className="text-[10px] text-[var(--text-dim)] uppercase">
            {modelFilter 
              ? `${filteredModels.length} match${filteredModels.length === 1 ? "" : "es"}`
              : `${models.length} available`}
          </span>
        </div>
        
        <div className="flex flex-col gap-2">
          <input
            type="text"
            placeholder="Search models…"
            value={modelFilter}
            onChange={(e) => setModelFilter(e.target.value)}
            disabled={isActive}
            className="text-xs bg-[var(--bg-input)] border border-[var(--border-main)] rounded-lg px-2.5 py-1.5 text-[var(--text-main)] placeholder-[var(--text-dim)] focus:outline-none focus:border-[var(--accent-blue)] disabled:opacity-50 transition-colors"
          />

          <div className="flex flex-col gap-1 max-h-40 overflow-y-auto pr-1 custom-scrollbar">
            {filteredModels.length === 0 && (
              <span className="text-xs text-[var(--text-muted)] py-4 text-center">No models found for this engine</span>
            )}
            {filteredModels.map((m) => {
              const selected = settings?.model === m.model_id;
              return (
                <button
                  key={m.model_id}
                  disabled={isActive}
                  type="button"
                  onClick={() => handleModelChange(m.model_id)}
                  className={`flex flex-col text-left rounded-lg p-2 transition-all ${
                    selected 
                      ? "bg-[var(--bg-active)] ring-1 ring-[var(--accent-blue)]/50" 
                      : "hover:bg-[var(--bg-active)]/50 border border-transparent"
                  } ${isActive ? "opacity-50 cursor-not-allowed" : "cursor-pointer"}`}
                >
                  <div className="flex items-center gap-2 mb-0.5 selectable">
                    <span className={`w-1.5 h-1.5 rounded-full ${selected ? "bg-[var(--accent-blue)]" : "bg-[var(--bg-active)]"}`} />
                    <span className={`text-[11px] font-medium ${m.is_cached ? "text-[var(--text-main)]" : "text-[var(--text-muted)]"}`}>
                      {m.display_name}
                    </span>
                    {m.is_cached && (
                      <span className="text-green-500 text-[9px] bg-green-500/10 px-1 rounded">Cached</span>
                    )}
                    <span className="text-[9px] text-[var(--text-dim)] ml-auto">{m.dimension}d</span>
                  </div>
                  <p className="text-[9px] text-[var(--text-dim)] leading-snug line-clamp-1 ml-3.5 selectable">
                    {m.description}
                  </p>
                  {selected && !m.is_cached && (
                    <span className="text-[9px] text-[var(--text-dim)] ml-3.5 mt-0.5">
                      {sizeFetchingFor === m.model_id
                        ? "Checking size…"
                        : m.size_bytes !== null
                          ? `Estimated download: ${formatBytes(m.size_bytes)}`
                          : "Download required"}
                    </span>
                  )}
                </button>
              );
            })}
          </div>
        </div>
      </section>

      {/* Action Area */}
      <section className="bg-[var(--bg-active)]/30 rounded-xl p-3 border border-[var(--border-main)] flex flex-col gap-3">
        {phase === "engine_mismatch" && (
          <div className="bg-amber-900/20 border border-amber-900/50 rounded-lg p-2">
            <p className="text-[10px] text-amber-200 leading-relaxed">
              ⚠️ <strong>Engine Mismatch:</strong> rebuild required.
            </p>
          </div>
        )}

        {phase === "indexed" && indexStatus && (
          <div className="flex flex-col gap-1 px-1 selectable">
            <div className="flex justify-between text-[10px]">
              <span className="text-[var(--text-muted)]">Indexed Files</span>
              <span className="text-[var(--text-main)] font-mono">{indexStatus.indexed_files}</span>
            </div>
            <div className="flex justify-between text-[10px]">
              <span className="text-[var(--text-muted)]">Total Chunks</span>
              <span className="text-[var(--text-main)] font-mono">{indexStatus.total_chunks}</span>
            </div>
            {indexStatus.built_at !== null && (
              <div className="flex justify-between text-[10px]">
                <span className="text-[var(--text-muted)]">Last Built</span>
                <span className="text-[var(--text-main)]">
                  {new Date(indexStatus.built_at * 1000).toLocaleDateString()} 
                  {indexStatus.build_duration_ms !== null && (
                    <span className="text-[var(--text-muted)] ml-1">
                      ({indexStatus.build_duration_ms < 1000 
                        ? `${indexStatus.build_duration_ms}ms`
                        : indexStatus.build_duration_ms < 60000
                          ? `${(indexStatus.build_duration_ms / 1000).toFixed(1)}s`
                          : `${Math.floor(indexStatus.build_duration_ms / 60000)}m ${Math.floor((indexStatus.build_duration_ms % 60000) / 1000)}s`
                      })
                    </span>
                  )}
                </span>
              </div>
            )}
          </div>
        )}

        <button
          onClick={handleAction}
          disabled={isCancelling}
          type="button"
          className={`w-full py-2 rounded-lg text-xs font-semibold transition-all shadow-lg active:scale-[0.98] disabled:opacity-50 ${
            isActive
              ? "bg-red-600 hover:bg-red-500 text-white"
              : phase === "indexed"
                ? "bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] border border-[var(--border-main)]"
                : phase === "engine_mismatch"
                  ? "bg-amber-600 hover:bg-amber-500 text-white"
                  : "bg-[var(--accent-blue)] hover:bg-[var(--accent-blue)] text-white"
          }`}
        >
          {phase === "not_downloaded" && "Download Model"}
          {phase === "downloading" && (isCancelling ? "Cancelling…" : "Cancel Download")}
          {phase === "ready" && "Build Semantic Index"}
          {phase === "engine_mismatch" && "Rebuild Index"}
          {phase === "building" && (isCancelling ? "Cancelling…" : "Cancel Build")}
          {phase === "indexed" && "Delete Index"}
        </button>

        {isActive && (
          <div className="flex flex-col gap-2 mt-1 px-1">
            <div className="flex justify-between text-[10px] text-[var(--text-muted)] mb-0.5">
              <span>
                {phase === "downloading" ? (
                  "Downloading model files..."
                ) : (
                  `${progressPct}%`
                )}
              </span>
              <span className="truncate max-w-[180px]">
                {phase === "building" && progress && "message" in progress ? (progress as any).message : ""}
              </span>
            </div>
            <div className="h-1.5 bg-[var(--bg-app)] rounded-full overflow-hidden">
              <div
                className={`h-full bg-[var(--accent-blue)] transition-all duration-300 ease-out ${
                  phase === "downloading" ? "w-full animate-shimmer" : ""
                }`}
                style={{ width: phase === "downloading" ? "100%" : `${progressPct}%` }}
              />
            </div>
          </div>
        )}
      </section>

      {error && (
        <div className="p-3 bg-red-900/20 border border-red-900/50 rounded-lg">
          <p className="text-xs text-red-400 leading-relaxed">{error}</p>
        </div>
      )}
    </div>
  );
}
