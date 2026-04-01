import { useState, useEffect, useCallback } from "react";
import type {
  EmbedderModel,
  EmbedProgress,
  EmbedDone,
  EmbedError,
  ModelDescriptor,
  SemanticSettings,
  IndexStatus,
} from "../lib/types";
import type { TauriSearchApi } from "../services/tauri";

type Phase = "not_downloaded" | "downloading" | "ready" | "building" | "indexed";

interface ProgressState {
  current: number;
  total: number;
}

interface Props {
  api: TauriSearchApi;
  directory: string;
  refreshSemanticReady: () => void;
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

  // Load settings and model list in parallel on mount.
  useEffect(() => {
    Promise.all([api.getSettings(), api.listModels()])
      .then(([s, descriptors]) => {
        setModels(descriptors);
        const sem = s.semantic;
        setSettings(sem);

        const selected = descriptors.find((m) => m.model_id === sem.model);
        if (!sem.enabled || !selected?.is_cached) {
          setPhase("not_downloaded");
          if (sem.enabled && selected && !selected.is_cached) {
            api
              .updateSettings({ semantic: { ...sem, enabled: false, index_path: null } })
              .catch((e) => console.error("updateSettings failed:", e));
          }
          return;
        }
        if (sem.index_path === null) {
          setPhase("ready");
        } else {
          setPhase("indexed");
          api
            .getIndexStatus()
            .then(setIndexStatus)
            .catch((e) => console.error("getIndexStatus failed:", e));
        }
      })
      .catch((e) => console.error("init failed:", e));
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

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
          setSettings((prev) => {
            if (prev) {
              // Mark the now-cached model in the descriptor list.
              setModels((ms) =>
                ms.map((m) => (m.model_id === prev.model ? { ...m, is_cached: true } : m)),
              );
            }
            return prev ? { ...prev, enabled: true } : prev;
          });
        } else if (done.operation === "Build") {
          setPhase("indexed");
          api
            .getIndexStatus()
            .then(setIndexStatus)
            .catch((e) => console.error("getIndexStatus failed:", e));
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
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleModelChange = useCallback(
    (modelId: EmbedderModel) => {
      if (!settings) return;
      const next = { ...settings, model: modelId };
      setSettings(next);
      const descriptor = models.find((m) => m.model_id === modelId);
      setPhase(descriptor?.is_cached ? "ready" : "not_downloaded");
      api
        .updateSettings({ semantic: next })
        .catch((e) => console.error("updateSettings failed:", e));

      // Fetch download size lazily when expanding an uncached model.
      if (descriptor && !descriptor.is_cached && descriptor.size_bytes === null) {
        setSizeFetchingFor(modelId);
        api
          .getModelSize(modelId)
          .then((size) => {
            setModels((prev) =>
              prev.map((m) => (m.model_id === modelId ? { ...m, size_bytes: size } : m)),
            );
          })
          .catch((e) => {
            console.error(`getModelSize(${modelId}) failed:`, e);
          })
          .finally(() => setSizeFetchingFor(null));
      }
    },
    [settings, models, api],
  );

  const handleAction = useCallback(async () => {
    setError(null);
    if (phase === "not_downloaded") {
      if (!settings) return;
      api.downloadModel(settings.model).catch((e) => console.error("downloadModel failed:", e));
    } else if (phase === "downloading" || phase === "building") {
      setIsCancelling(true);
      api.cancelEmbed().catch((e) => { setIsCancelling(false); console.error("cancelEmbed failed:", e); });
    } else if (phase === "ready") {
      if (!settings) return;
      api
        .buildIndex(directory, settings.model)
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

  const filteredModels = modelFilter.trim()
    ? models.filter(
        (m) =>
          m.model_id.toLowerCase().includes(modelFilter.toLowerCase()) ||
          m.display_name.toLowerCase().includes(modelFilter.toLowerCase()) ||
          m.description.toLowerCase().includes(modelFilter.toLowerCase()),
      )
    : models;

  return (
    <div className="p-4 flex flex-col gap-4 w-72">
      <h3 className="text-sm font-semibold text-neutral-200">Semantic Search</h3>

      {/* Model list */}
      <div className="flex flex-col gap-1">
        <input
          type="text"
          placeholder="Filter models…"
          value={modelFilter}
          onChange={(e) => setModelFilter(e.target.value)}
          disabled={isActive}
          className="text-xs bg-neutral-800 border border-neutral-700 rounded px-2 py-1 text-neutral-200 placeholder-neutral-500 focus:outline-none focus:border-neutral-500 disabled:opacity-50"
        />

        <div className="flex flex-col gap-0.5 max-h-56 overflow-y-auto mt-1">
          {filteredModels.length === 0 && (
            <span className="text-xs text-neutral-500 px-1 py-2">No models match</span>
          )}
          {filteredModels.map((m) => {
            const selected = settings?.model === m.model_id;
            return (
              <label
                key={m.model_id}
                className={`flex flex-col gap-0.5 text-xs rounded px-2 py-1.5 ${
                  isActive
                    ? "opacity-50 cursor-not-allowed"
                    : "cursor-pointer hover:bg-neutral-800"
                } ${selected ? "bg-neutral-800" : ""}`}
              >
                <div className="flex items-center gap-2">
                  <input
                    type="radio"
                    name="semantic-model"
                    value={m.model_id}
                    checked={selected}
                    disabled={isActive}
                    onChange={() => handleModelChange(m.model_id)}
                    className="accent-blue-500 shrink-0"
                  />
                  <span className={m.is_cached ? "text-neutral-100" : "text-neutral-400"}>
                    {m.display_name}
                  </span>
                  {m.is_cached && (
                    <span className="text-green-500 text-[10px] leading-none">✓</span>
                  )}
                  <span className="text-neutral-500 ml-auto shrink-0">{m.dimension}d</span>
                </div>
                {selected && (
                  <div className="pl-5 flex flex-col gap-0.5">
                    <p className="text-neutral-500 text-[10px] leading-snug line-clamp-2">
                      {m.description}
                    </p>
                    <span className="text-neutral-500 text-[10px]">
                      {m.is_cached
                        ? m.size_bytes !== null
                          ? formatBytes(m.size_bytes)
                          : null
                        : sizeFetchingFor === m.model_id
                          ? "fetching size…"
                          : m.size_bytes !== null
                            ? `~${formatBytes(m.size_bytes)} download`
                            : null}
                    </span>
                  </div>
                )}
              </label>
            );
          })}
        </div>
      </div>

      {/* Action button */}
      <button
        onClick={handleAction}
        disabled={isCancelling}
        className={`text-xs px-3 py-1.5 rounded font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${
          isActive
            ? "bg-red-700 hover:bg-red-600 text-white"
            : phase === "indexed"
              ? "bg-neutral-700 hover:bg-neutral-600 text-neutral-200"
              : "bg-blue-600 hover:bg-blue-500 text-white"
        }`}
      >
        {phase === "not_downloaded" && "Download"}
        {phase === "downloading" && (isCancelling ? "Cancelling…" : "Cancel")}
        {phase === "ready" && "Build Index"}
        {phase === "building" && (isCancelling ? "Cancelling…" : "Cancel")}
        {phase === "indexed" && "Delete Index"}
      </button>

      {/* Progress bar */}
      {isActive && (
        <div className="flex flex-col gap-1">
          <div className="h-1.5 bg-neutral-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-blue-500 transition-all duration-200"
              style={{ width: `${progressPct}%` }}
            />
          </div>
          <span className="text-xs text-neutral-400 text-right">{progressPct}%</span>
        </div>
      )}

      {/* Error */}
      {error && <p className="text-xs text-red-400">{error}</p>}

      {/* Index status */}
      {phase === "indexed" && indexStatus && (
        <div className="flex flex-col gap-0.5 text-xs text-neutral-400">
          <span>
            {indexStatus.indexed_files} files · {indexStatus.total_chunks} chunks
          </span>
          <span>Model: {indexStatus.model_id}</span>
          {indexStatus.built_at !== null && (
            <span>Built: {new Date(indexStatus.built_at * 1000).toLocaleString()}</span>
          )}
        </div>
      )}
    </div>
  );
}
