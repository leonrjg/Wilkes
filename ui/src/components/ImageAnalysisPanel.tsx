import { useCallback, useEffect, useState } from "react";
import { Tooltip } from "@leonrjg/wilkes-reader";
import type { SearchApi } from "../services/api";
import {
  ALL_RECOGNITION_ENGINES,
  type EmbedProgress,
  type RecognitionEngine,
  type RecognizerDescriptor,
  type RecognizerInventory,
  type Settings,
} from "../lib/types";
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

const DEFAULTS = {
  enabled: false,
  engine: "Onnx",
  model: null,
  device: null,
  describer_model: "",
} as const;

const ENGINE_LABELS: Record<RecognitionEngine, string> = {
  Onnx: "ONNX",
  Candle: "Candle",
};

const ENGINE_BLURBS: Record<RecognitionEngine, string> = {
  Onnx: "ONNX Runtime, in the recognition worker. Reads a whole page in one pass and covers formulas, tables and code as well as prose. Runs on the CPU, on one thread less than the machine has.",
  Candle: "PaddleOCR-VL under candle. Transcribes text with precise per-region geometry, and nothing else. Uses the GPU via Metal (Apple Silicon) if available.",
};

/** What ModelCatalog needs, said in the recognizer's own terms. The footprint
 *  *is* the download, and the engine's own default is the recommendation when
 *  it is not the catalogue-wide one. */
type RecognizerCatalogEntry = RecognizerDescriptor & {
  size_bytes: number;
  is_recommended: boolean;
};

/**
 * The one surface allowed to render image-enrichment UI while the recognizer
 * is not installed — it is what installs it.
 *
 * The panel is deliberately blunt about the cost of the toggle. Enrichment is
 * part of the extraction recipe, so turning it on or off, or changing the
 * recognizer, is not a display preference: it changes what every document in
 * the library reads as, and every document with a picture in it is re-read and
 * re-embedded.
 *
 * The engine and model are picked the way embedding's are, and for the same
 * reason: the backend has dispatched recognition by engine and model id since
 * there were two of them, and an interface that offers only one of them makes
 * the other unreachable rather than absent.
 */
export default function ImageAnalysisPanel({ api, settings, onUpdateSettings }: Props) {
  const analysis = settings.image_analysis ?? DEFAULTS;
  const [supportedEngines, setSupportedEngines] = useState<RecognitionEngine[]>([]);
  const [recognizers, setRecognizers] = useState<RecognizerDescriptor[]>([]);
  const [modelFilter, setModelFilter] = useState("");
  const [draftEngine, setDraftEngine] = useState<RecognitionEngine>(analysis.engine);
  const [draftModel, setDraftModel] = useState<string | null>(analysis.model);
  const [inventory, setInventory] = useState<RecognizerInventory | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [describerModel, setDescriberModel] = useState(analysis.describer_model ?? "");

  useEffect(() => {
    setDescriberModel(analysis.describer_model ?? "");
  }, [analysis.describer_model]);

  const refreshCatalogue = useCallback(async () => {
    const catalogue = await api.imageRecognizerCatalogue();
    setSupportedEngines(catalogue.engines);
    setRecognizers(catalogue.models);
    return catalogue;
  }, [api]);

  useEffect(() => {
    let mounted = true;
    api
      .imageRecognizerCatalogue()
      .then((catalogue) => {
        if (!mounted) return;
        setSupportedEngines(catalogue.engines);
        setRecognizers(catalogue.models);
      })
      .catch((cause) => {
        if (mounted) setError(String(cause));
      });
    return () => {
      mounted = false;
    };
  }, [api]);

  // The settings are the commitment; the draft is what is being considered.
  // A settings edit — this panel's own, or one from elsewhere — resets it,
  // the way the generation panel's does.
  useEffect(() => {
    setDraftEngine(analysis.engine);
    setDraftModel(analysis.model);
  }, [analysis.engine, analysis.model]);

  /** The model an engine reads with when nothing names one. Answered by the
   *  catalogue rather than by a table kept here: a second copy of it is how a
   *  library ends up read under a recognizer nobody chose. */
  const engineDefault = useCallback(
    (engine: RecognitionEngine) =>
      recognizers.find((model) => model.engine === engine && model.is_engine_default)
        ?.model_id ?? null,
    [recognizers],
  );

  const configuredModel = analysis.model ?? engineDefault(analysis.engine);
  const effectiveDraftModel = draftModel ?? engineDefault(draftEngine);

  const catalogModels: RecognizerCatalogEntry[] = recognizers
    .filter((model) => model.engine === draftEngine)
    .map((model) => ({
      ...model,
      size_bytes: model.footprint_bytes,
      is_recommended: model.is_engine_default && !model.is_default,
    }));

  const selected =
    catalogModels.find((model) => model.model_id === effectiveDraftModel) ?? null;
  const configured = recognizers.find(
    (model) => model.engine === analysis.engine && model.model_id === configuredModel,
  );
  const configuredInstalled = configured?.is_cached ?? false;
  const isConfigured =
    draftEngine === analysis.engine && effectiveDraftModel === configuredModel;

  // The inventory describes the recipe rather than this machine, so it is read
  // for whatever is *selected* and whether or not it is installed: it is what
  // the download is disclosed by, and disclosing it afterwards would be no
  // disclosure at all.
  useEffect(() => {
    if (!effectiveDraftModel) {
      setInventory(null);
      return;
    }
    let mounted = true;
    api
      .imageRecognizerInventory(draftEngine, effectiveDraftModel)
      .then((next) => {
        if (mounted) setInventory(next);
      })
      .catch((cause) => {
        if (!mounted) return;
        setInventory(null);
        console.error("imageRecognizerInventory failed:", cause);
      });
    return () => {
      mounted = false;
    };
  }, [api, draftEngine, effectiveDraftModel]);

  // The install has its own event stream, and a settings change can start one
  // too, so the panel listens for backend truth rather than assuming the
  // button it rendered owns the operation.
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
      api.onImageAnalysisProgress((next: EmbedProgress) => {
        if (!mounted || !("Download" in next)) return;
        setBusy(true);
        setProgress({
          received: next.Download.bytes_received,
          total: next.Download.total_bytes,
        });
      }),
      "onImageAnalysisProgress",
    );
    track(
      api.onImageAnalysisDone(() => {
        if (!mounted) return;
        setBusy(false);
        setProgress(null);
        refreshCatalogue().catch((cause) => setError(String(cause)));
      }),
      "onImageAnalysisDone",
    );
    track(
      api.onImageAnalysisError((failure) => {
        if (!mounted) return;
        setBusy(false);
        setProgress(null);
        setError(failure.message);
        refreshCatalogue().catch(() => {});
      }),
      "onImageAnalysisError",
    );

    return () => {
      mounted = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  const patch = async (next: Partial<Settings["image_analysis"]>) => {
    setError(null);
    await onUpdateSettings({ image_analysis: { ...analysis, ...next } });
  };

  const handleEngineChange = (engine: RecognitionEngine) => {
    if (engine === draftEngine) return;
    setError(null);
    setDraftEngine(engine);
    setDraftModel(engineDefault(engine));
  };

  /**
   * Install first, commit second.
   *
   * `build_analyzer` refuses a recognizer that is enabled but not on disk, so
   * writing the choice into the settings before the weights exist would put
   * the app into exactly the state the recipe exists to prevent — and would
   * announce it as a failure the user did not cause.
   */
  const handleAction = async () => {
    if (!selected) return;
    setBusy(true);
    setError(null);
    try {
      if (!selected.is_cached) {
        await api.installImageRecognizer(draftEngine, selected.model_id);
      }
      if (!isConfigured) {
        await patch({ engine: draftEngine, model: selected.model_id });
      }
      await refreshCatalogue();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  const handleToggle = async (enabled: boolean) => {
    setBusy(true);
    try {
      await patch({ enabled });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  const actionLabel = (() => {
    if (busy) return "Working…";
    if (!selected) return "Select a recognizer";
    if (!selected.is_cached) {
      return isConfigured ? "Download recognizer" : "Download recognizer and use it";
    }
    return isConfigured ? "In use" : "Use this recognizer";
  })();

  const progressPercent =
    progress && progress.total > 0
      ? Math.round((progress.received / progress.total) * 100)
      : 0;

  return (
    <div className="flex flex-col gap-4 p-1">
      <section>
        <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          Text inside pictures
        </h3>
        <div className="space-y-3 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
          <label className="flex cursor-pointer items-center gap-2.5">
            <input
              type="checkbox"
              checked={analysis.enabled}
              disabled={busy || !configuredInstalled}
              onChange={(event) => void handleToggle(event.target.checked)}
              className="h-3.5 w-3.5 accent-[var(--accent-blue)]"
            />
            <span className="text-xs text-[var(--text-main)]">
              Read the text drawn inside diagrams, charts and scanned figures
            </span>
          </label>
          <p className="text-[10px] italic text-[var(--text-dim)]">
            A label inside a picture is invisible to search today. With this on,
            it is transcribed into the document's text at the place the page
            draws the picture, and search finds it there.
          </p>
          <p className="text-[10px] italic text-[var(--text-dim)]">
            This is part of how a document is read, not a display option:
            changing it re-reads and re-embeds every document that has a
            picture in it. Recognition runs on this machine and is slow on a
            CPU — measured at about half a minute for a small diagram, four
            minutes for a full-width one, and several times that for a large
            one. A library of a few hundred figures is an overnight job.
          </p>
          {!configuredInstalled && (
            <p className="text-[10px] text-[var(--text-dim)]">
              Nothing to read with yet — install a recognizer below first.
            </p>
          )}
        </div>
      </section>

      {/* Engine selection */}
      <section>
        <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          Recognition Engine
        </h3>
        <div className="flex w-full rounded-lg bg-[var(--bg-active)] p-0.5">
          {ALL_RECOGNITION_ENGINES.map((engine) => {
            const isSupported = supportedEngines.includes(engine);
            return (
              <Tooltip
                key={engine}
                content={!isSupported ? "Feature disabled in this build" : undefined}
              >
                <button
                  type="button"
                  disabled={busy || !isSupported}
                  onClick={() => handleEngineChange(engine)}
                  className={`flex-1 rounded-md px-3 py-1 text-xs transition-all ${
                    draftEngine === engine
                      ? "bg-[var(--bg-app)] text-[var(--text-main)] shadow-sm"
                      : !isSupported
                        ? "cursor-not-allowed text-[var(--text-muted)]/50 opacity-50"
                        : "text-[var(--text-muted)] hover:text-[var(--text-main)] disabled:opacity-50"
                  }`}
                >
                  {ENGINE_LABELS[engine]}
                </button>
              </Tooltip>
            );
          })}
        </div>
        <p className="selectable mt-1.5 px-1 text-[10px] text-[var(--text-dim)]">
          {ENGINE_BLURBS[draftEngine]}
        </p>
      </section>

      {/* Model list */}
      <ModelCatalog
        title="Recognition Model"
        catalogKey={`recognition:${draftEngine}`}
        models={catalogModels}
        filter={modelFilter}
        selectedModelId={effectiveDraftModel}
        activeModelId={analysis.enabled ? configuredModel : null}
        disabled={busy}
        emptyMessage="No recognizers found for this engine"
        onFilterChange={setModelFilter}
        onSelect={(model) => setDraftModel(model.model_id)}
      />

      {/* What choosing it would mean, and what it would download */}
      <section className="flex flex-col gap-3 rounded-xl border border-[var(--border-main)] bg-[var(--bg-active)]/30 p-3">
        {analysis.enabled && !isConfigured && (
          <div className="rounded-lg border border-amber-900/50 bg-amber-900/20 p-1">
            <p className="text-center text-[10px] leading-relaxed text-[var(--text-muted)]">
              Changing the recognizer re-reads and re-embeds every document with
              a picture in it.
            </p>
          </div>
        )}

        {selected && (
          <div className="flex flex-wrap items-center gap-1 px-1">
            {selected.emits.map((kind) => (
              <span
                key={kind}
                className="rounded bg-[var(--bg-active)] px-1.5 py-0.5 text-[9px] uppercase tracking-tighter text-[var(--text-dim)]"
              >
                {kind}
              </span>
            ))}
            <span className="ml-auto text-[9px] text-[var(--text-dim)]">
              admits at {selected.admission_threshold.toFixed(2)}
            </span>
          </div>
        )}

        {inventory && (
          <div className="space-y-1.5 px-1">
            <p className="text-[10px] text-[var(--text-dim)]">
              {selected?.is_cached ? "Running " : "Downloading it fetches "}
              <span className="text-[var(--text-muted)]">{inventory.name}</span>,{" "}
              {formatModelBytes(inventory.footprint_bytes)} from{" "}
              <span className="text-[var(--text-muted)]">{inventory.repo}</span> at{" "}
              <span className="font-mono">{inventory.revision.slice(0, 12)}</span>,
              under{" "}
              <a
                href={inventory.license_url}
                target="_blank"
                rel="noreferrer"
                className="text-[var(--accent-blue)] underline"
              >
                {inventory.license}
              </a>
              .
            </p>
            <details className="text-[10px] text-[var(--text-dim)]">
              <summary className="cursor-pointer">What it is made of</summary>
              <ul className="mt-1 list-disc pl-4">
                {inventory.derived_from.map((component) => (
                  <li key={component}>{component}</li>
                ))}
              </ul>
              <ul className="mt-1.5 space-y-0.5">
                {inventory.artifacts.map((artifact) => (
                  <li key={artifact.filename} className="font-mono">
                    {artifact.filename} · {formatModelBytes(artifact.size_bytes)} ·
                    sha256 {artifact.sha256.slice(0, 12)}…
                  </li>
                ))}
              </ul>
              <p className="mt-1.5">
                Each file is checked against the size and digest above after it
                downloads; one that does not match is deleted rather than used.
              </p>
            </details>
          </div>
        )}

        {progress && (
          <div className="relative h-5 overflow-hidden rounded-full border border-[var(--border-main)]/60 bg-[var(--bg-app)]">
            <div
              className="h-full rounded-full bg-[var(--accent-blue)] transition-all duration-300 ease-out"
              style={{ width: `${progressPercent}%` }}
            />
            <div className="pointer-events-none absolute inset-0 flex items-center justify-between px-2">
              <span className="text-[9px] font-medium uppercase tracking-[0.14em] text-[var(--text-dim)]">
                {progress.total > 0
                  ? `Downloading ${formatModelBytes(progress.received)} of ${formatModelBytes(progress.total)}`
                  : `${formatModelBytes(progress.received)} downloaded`}
              </span>
              <span className="rounded-full bg-black/20 px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-white backdrop-blur-sm">
                {progressPercent}%
              </span>
            </div>
          </div>
        )}

        <button
          type="button"
          disabled={busy || !selected || (isConfigured && selected.is_cached)}
          onClick={() => void handleAction()}
          className="w-full rounded-lg bg-[var(--accent-blue)] py-2 text-xs font-semibold text-white shadow-sm transition-all hover:bg-[var(--accent-blue-hover)] active:scale-[0.98] disabled:opacity-50"
        >
          {actionLabel}
        </button>

        {/* ONNX Runtime is told how many threads to use rather than which
            device, so offering it a device here would be a setting that reads
            as a promise. */}
        {draftEngine === "Candle" && (
          <label className="flex flex-col gap-1 px-1">
            <span className="text-xs text-[var(--text-muted)]">Device</span>
            <select
              aria-label="Recognition device"
              value={analysis.device ?? "auto"}
              disabled={busy}
              onChange={(event) =>
                void patch({
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
        )}
      </section>

      <section>
        <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          What the picture shows
        </h3>
        <div className="space-y-3 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
          <p className="text-[10px] italic text-[var(--text-dim)]">
            Transcribing the labels of a diagram does not capture its arrows. A
            vision model can describe them, in its own words, kept separate
            from the document's own text and labelled as a description
            throughout. Optional, and off unless you name a model.
          </p>
          <label className="flex flex-col gap-1">
            <span className="text-xs text-[var(--text-muted)]">
              Ollama model, or empty for transcription only
            </span>
            <div className="flex gap-2">
              <input
                type="text"
                value={describerModel}
                placeholder="qwen3-vl:2b"
                disabled={busy}
                onChange={(event) => setDescriberModel(event.target.value)}
                className="flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2.5 py-1.5 text-xs text-[var(--text-main)]"
              />
              <button
                type="button"
                disabled={busy || describerModel === (analysis.describer_model ?? "")}
                onClick={() => void patch({ describer_model: describerModel.trim() })}
                className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-main)] disabled:opacity-50"
              >
                Save
              </button>
            </div>
          </label>
          <p className="text-[10px] italic text-[var(--text-dim)]">
            Served by the Ollama server configured under Generation
            {settings.generation?.ollama_url
              ? ` (${settings.generation.ollama_url})`
              : ""}
            . A server that is not on this machine means the pictures in your
            documents are sent to it.
          </p>
        </div>
      </section>

      {error && (
        <p className="rounded border border-[var(--accent-red)] bg-[var(--bg-input)] p-2 text-[10px] text-[var(--accent-red)]">
          {error}
        </p>
      )}
    </div>
  );
}
