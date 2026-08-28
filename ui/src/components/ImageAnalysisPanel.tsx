import { useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type { EmbedProgress, Settings } from "../lib/types";
import { formatModelBytes } from "./ModelCatalog";

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

interface DownloadProgress {
  received: number;
  total: number;
}

const DEFAULTS = { enabled: false, device: null, describer_model: "" } as const;

/**
 * The one surface allowed to render image-enrichment UI while the recognizer
 * is not installed — it is what installs it.
 *
 * The panel is deliberately blunt about the cost of the toggle. Enrichment is
 * part of the extraction recipe, so turning it on or off, or changing the
 * describer, is not a display preference: it changes what every document in
 * the library reads as, and every document with a picture in it is re-read and
 * re-embedded.
 */
export default function ImageAnalysisPanel({ api, settings, onUpdateSettings }: Props) {
  const analysis = settings.image_analysis ?? DEFAULTS;
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [describerModel, setDescriberModel] = useState(analysis.describer_model ?? "");

  useEffect(() => {
    setDescriberModel(analysis.describer_model ?? "");
  }, [analysis.describer_model]);

  const refreshInstalled = () => {
    api
      .isImageRecognizerInstalled()
      .then(setInstalled)
      .catch((cause) => setError(String(cause)));
  };

  useEffect(refreshInstalled, [api]);

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
        refreshInstalled();
      }),
      "onImageAnalysisDone",
    );
    track(
      api.onImageAnalysisError((failure) => {
        if (!mounted) return;
        setBusy(false);
        setProgress(null);
        setError(failure.message);
        refreshInstalled();
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

  const handleInstall = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.installImageRecognizer();
    } catch (cause) {
      setBusy(false);
      setProgress(null);
      setError(String(cause));
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
              disabled={busy || installed === false}
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
            CPU — measured at around five minutes for a full-width figure, and
            longer for a large one. A library of a few hundred figures is an
            overnight job.
          </p>

          {installed === false && (
            <div className="border-t border-[var(--border-main)] pt-3">
              <p className="mb-2 text-[10px] text-[var(--text-dim)]">
                The recognizer is not installed yet (about 1.9 GB, downloaded
                once).
              </p>
              <button
                type="button"
                disabled={busy}
                onClick={() => void handleInstall()}
                className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs text-white disabled:opacity-50"
              >
                {busy ? "Installing…" : "Download recognizer"}
              </button>
            </div>
          )}

          {progress && (
            <div className="border-t border-[var(--border-main)] pt-3">
              <div className="h-1 w-full overflow-hidden rounded bg-[var(--bg-app)]">
                <div
                  className="h-full bg-[var(--accent-blue)]"
                  style={{ width: `${progressPercent}%` }}
                />
              </div>
              <p className="mt-1 text-[10px] text-[var(--text-dim)]">
                {progress.total > 0
                  ? `Downloading ${formatModelBytes(progress.received)} of ${formatModelBytes(progress.total)}`
                  : `${formatModelBytes(progress.received)} downloaded`}
              </p>
            </div>
          )}

          {installed && (
            <div className="border-t border-[var(--border-main)] pt-3">
              <label className="flex flex-col gap-1">
                <span className="text-xs text-[var(--text-muted)]">Device</span>
                <select
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
            </div>
          )}
        </div>
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
