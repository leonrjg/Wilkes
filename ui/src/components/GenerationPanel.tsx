import { useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type { EmbedProgress, GeneratorDescriptor, Settings } from "../lib/types";
import { useGenerationStore } from "../stores/useGenerationStore";
import ModelCatalog, { formatModelBytes } from "./ModelCatalog";

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

interface DownloadProgress {
  received: number;
  total: number;
}

function downloadLabel(progress: DownloadProgress): string {
  if (progress.total <= 0) {
    return `${formatModelBytes(progress.received)} downloaded`;
  }
  return `Downloading ${formatModelBytes(progress.received)} of ${formatModelBytes(
    progress.total,
  )}`;
}

/**
 * The one surface allowed to render generation UI while the feature is not
 * ready — it is what makes it ready. Everywhere else the rule is the opposite:
 * not ready means render as if the feature did not exist.
 */
export default function GenerationPanel({ api, settings, onUpdateSettings }: Props) {
  const [models, setModels] = useState<GeneratorDescriptor[]>([]);
  const [modelFilter, setModelFilter] = useState("");
  const [draftModelId, setDraftModelId] = useState<string | null>(
    settings.generation.model,
  );
  const [sizeFetchingFor, setSizeFetchingFor] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const ready = useGenerationStore((state) => state.ready);
  const refreshReady = useGenerationStore((state) => state.refreshReady);

  const generation = settings.generation;

  useEffect(() => {
    let mounted = true;
    api
      .listGenerationModels()
      .then((nextModels) => {
        if (!mounted) return;
        setModels(nextModels);
        setDraftModelId(
          (current) =>
            current
            ?? generation.model
            ?? nextModels.find((model) => model.is_default)?.model_id
            ?? null,
        );
      })
      .catch((cause) => {
        if (mounted) setError(String(cause));
      });
    void refreshReady();
    return () => {
      mounted = false;
    };
  }, [api, refreshReady]);

  useEffect(() => {
    if (generation.model) setDraftModelId(generation.model);
  }, [generation.model]);

  // Generation installation has its own event stream. Settings changes can
  // start a load too, so the panel listens for backend truth instead of
  // pretending the initiating button owns the operation.
  useEffect(() => {
    let mounted = true;
    const unlisteners: Array<() => void> = [];
    const track = (subscribe: Promise<() => void>, label: string) => {
      subscribe
        .then((unlisten) => {
          if (mounted) unlisteners.push(unlisten);
          else unlisten();
        })
        .catch((cause) => console.error(`${label} subscription failed:`, cause));
    };

    track(
      api.onGenerationProgress((nextProgress: EmbedProgress) => {
        if (!mounted || !("Download" in nextProgress)) return;
        setBusy(true);
        setProgress({
          received: nextProgress.Download.bytes_received,
          total: nextProgress.Download.total_bytes,
        });
      }),
      "onGenerationProgress",
    );
    track(
      api.onGenerationDone(() => {
        if (!mounted) return;
        setBusy(false);
        setProgress(null);
        api
          .listGenerationModels()
          .then((nextModels) => {
            if (mounted) setModels(nextModels);
          })
          .catch(() => {});
        void refreshReady();
      }),
      "onGenerationDone",
    );
    track(
      api.onGenerationError((generationError) => {
        if (!mounted) return;
        setBusy(false);
        setProgress(null);
        setError(generationError.message);
        void refreshReady();
      }),
      "onGenerationError",
    );

    return () => {
      mounted = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [api, refreshReady]);

  const patchGeneration = async (patch: Partial<Settings["generation"]>) => {
    setError(null);
    await onUpdateSettings({ generation: { ...generation, ...patch } });
  };

  const handleSelectModel = async (model: GeneratorDescriptor) => {
    setDraftModelId(model.model_id);
    setError(null);
    if (model.is_cached || model.size_bytes !== null) return;

    setSizeFetchingFor(model.model_id);
    try {
      const sizeBytes = await api.getGenerationModelSize(model.model_id);
      setModels((current) =>
        current.map((candidate) =>
          candidate.model_id === model.model_id
            ? { ...candidate, size_bytes: sizeBytes }
            : candidate,
        ),
      );
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSizeFetchingFor(null);
    }
  };

  const handleToggle = async (enabled: boolean) => {
    if (!enabled) {
      try {
        await patchGeneration({ enabled: false });
        await refreshReady();
      } catch (cause) {
        setError(String(cause));
      }
      return;
    }

    const modelId =
      draftModelId
      ?? generation.model
      ?? models.find((model) => model.is_default)?.model_id
      ?? null;
    if (!modelId) {
      setError("Select a generation model before enabling local generation.");
      return;
    }

    setBusy(true);
    try {
      await patchGeneration({ enabled: true, model: modelId });
    } catch (cause) {
      setBusy(false);
      setError(String(cause));
    }
  };

  const handleAction = async () => {
    if (!draftModelId) return;
    setBusy(true);
    setError(null);
    try {
      if (!generation.enabled || generation.model !== draftModelId) {
        // Persisting a new selection is the single trigger for its first load.
        // Calling loadGenerationModel too would queue a duplicate install.
        await patchGeneration({ enabled: true, model: draftModelId });
      } else {
        await api.loadGenerationModel();
        setBusy(false);
        setModels(await api.listGenerationModels());
        await refreshReady();
      }
    } catch (cause) {
      setBusy(false);
      setProgress(null);
      setError(String(cause));
    }
  };

  const selected =
    models.find((model) => model.model_id === draftModelId) ?? null;
  const selectionChanged = draftModelId !== generation.model;
  const progressPercent =
    progress && progress.total > 0
      ? Math.round((progress.received / progress.total) * 100)
      : 0;

  const actionLabel = (() => {
    if (busy) return "Preparing…";
    if (!selected) return "Select a model";
    if (!generation.enabled) {
      return selected.is_cached ? "Enable generation" : "Download model and enable";
    }
    if (selectionChanged) {
      return selected.is_cached ? "Save model" : "Download model and enable";
    }
    return selected.is_cached ? "Reload model" : "Download model and enable";
  })();

  return (
    <div className="flex flex-col gap-4 p-1">
      <section>
        <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          Local Generation
        </h3>
        <div className="space-y-3 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
          <label className="flex cursor-pointer items-center gap-2.5">
            <input
              type="checkbox"
              checked={generation.enabled}
              onChange={(event) => void handleToggle(event.target.checked)}
              className="h-3.5 w-3.5 accent-[var(--accent-blue)]"
            />
            <span className="text-xs text-[var(--text-main)]">
              Name bookmark clusters and explain related documents locally
            </span>
          </label>
          <p className="text-[10px] italic text-[var(--text-dim)]">
            Runs entirely on this machine in a separate process. Disabling it hides
            generation features throughout the app.
          </p>

          {generation.enabled && (
            <div className="border-t border-[var(--border-main)] pt-3">
              <label className="flex flex-col gap-1">
                <span className="text-xs text-[var(--text-muted)]">Device</span>
                <select
                  value={generation.device ?? "auto"}
                  onChange={(event) =>
                    void patchGeneration({
                      device: event.target.value === "auto" ? null : event.target.value,
                    })
                  }
                  className="w-full rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2.5 py-1.5 text-xs text-[var(--text-main)]"
                >
                  <option value="auto">Auto (Metal preferred)</option>
                  <option value="metal">Require Metal</option>
                  <option value="cpu">CPU</option>
                </select>
              </label>
            </div>
          )}
        </div>
      </section>

      <ModelCatalog
        title="Generation Model"
        catalogKey="generation"
        models={models}
        filter={modelFilter}
        selectedModelId={draftModelId}
        activeModelId={generation.enabled ? generation.model : null}
        sizeFetchingFor={sizeFetchingFor}
        disabled={busy}
        emptyMessage="No generation models found"
        onFilterChange={setModelFilter}
        onSelect={(model) => void handleSelectModel(model)}
      />

      <section className="flex flex-col gap-3 rounded-xl border border-[var(--border-main)] bg-[var(--bg-active)]/30 p-3">
        <div className="flex items-center justify-between px-1 text-[10px]">
          <span className="text-[var(--text-muted)]">Model status</span>
          <span
            className={
              ready && generation.enabled
                ? "font-medium text-emerald-500"
                : "text-[var(--text-dim)]"
            }
          >
            {progress
              ? downloadLabel(progress)
              : ready && generation.enabled
                ? "Ready"
                : generation.enabled
                  ? "Not ready"
                  : "Disabled"}
          </span>
        </div>

        {progress && progress.total > 0 && (
          <div className="relative h-5 overflow-hidden rounded-full border border-[var(--border-main)]/60 bg-[var(--bg-app)]">
            <div
              className="h-full rounded-full bg-[var(--accent-blue)] transition-all duration-300 ease-out"
              style={{ width: `${progressPercent}%` }}
            />
            <div className="pointer-events-none absolute inset-0 flex items-center justify-between px-2">
              <span className="text-[9px] font-medium uppercase tracking-[0.14em] text-[var(--text-dim)]">
                Downloading model
              </span>
              <span className="rounded-full bg-black/20 px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-white backdrop-blur-sm">
                {progressPercent}%
              </span>
            </div>
          </div>
        )}

        <button
          type="button"
          disabled={busy || !selected}
          onClick={() => void handleAction()}
          className="w-full rounded-lg bg-[var(--accent-blue)] py-2 text-xs font-semibold text-white shadow-sm transition-all hover:bg-[var(--accent-blue-hover)] active:scale-[0.98] disabled:opacity-50"
        >
          {actionLabel}
        </button>

        <p className="px-1 text-[10px] italic text-[var(--text-dim)]">
          Generation and embedding workers remain resident for five minutes by default.
          The Workers panel reports the device actually in use.
        </p>

        {error && (
          <div
            role="alert"
            className="whitespace-pre-wrap break-all rounded border border-red-900/50 bg-red-900/20 p-2 font-mono text-[10px] text-red-400"
          >
            {error}
          </div>
        )}
      </section>
    </div>
  );
}
