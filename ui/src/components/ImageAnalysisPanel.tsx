import { useCallback, useEffect, useMemo, useState } from "react";
import { Tooltip } from "@leonrjg/wilkes-reader";
import type { SearchApi } from "../services/api";
import {
  ALL_RECOGNITION_ENGINES,
  type ImageAnalysisSettings,
  type EmbedProgress,
  type ImageScope,
  type InstallableModelStatus,
  type RecognitionEngine,
  type RecognizerDescriptor,
  type RecognizerInventory,
  type RecognizerRole,
  type Settings,
} from "../lib/types";
import ModelCatalog, { formatModelBytes } from "./ModelCatalog";
import RecognizerVenn, { PAGE_REGION, type VennModel } from "./RecognizerVenn";

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

interface DownloadProgress {
  received: number;
  total: number;
}

const DEFAULTS: ImageAnalysisSettings = {
  enabled: false,
  engine: "Onnx",
  model: null,
  device: null,
  describer_model: "",
  scope: "typeset_only",
  // Empty, which is every reader spent — the reading a settings file written
  // before this field existed was already producing. De-selection is a thing
  // you do, never a thing an upgrade does to you: the other default would
  // re-read every library that had a formula reader installed.
  disabled_roles: [],
};

/** What each scope reads. The label is the whole of the choice; the cost it
 *  carries is stated once, by the re-index warning below. */
const SCOPE_OPTIONS: Array<{ value: ImageScope; label: string }> = [
  { value: "typeset_only", label: "Formulas and tables only" },
  { value: "typeset_and_embedded", label: "Those, and every picture" },
];

const ENGINE_LABELS: Record<RecognitionEngine, string> = {
  Onnx: "ONNX",
  Candle: "Candle",
  Vision: "Apple Vision",
};

/** The readers that are not the page recognizer, in the order they are shown.
 *
 *  Each is a card rather than a row in the picker below, because none of them
 *  is an *alternative* to the page reader — they run alongside one, on the
 *  areas the detector marked out for them. Which model fills each card is the
 *  `role` the backend declares on its catalogue row, never a list of ids kept
 *  here: a second copy of which-is-which is how a formula reader eventually
 *  gets offered as the page recognizer.
 *
 *  One table rather than one hand-written section apiece, so a third role is a
 *  row here and the card stays a single piece of markup. */
const HELPER_READERS: Array<{
  role: RecognizerRole;
  /** The name of this reader's box in the diagram. The kind itself, not the
   *  model — the box is a piece of the page, and it exists whether or not a
   *  model claims it. */
  label: string;
  button: string;
  /** What is lost while it is not installed. Said where the download is
   *  offered, because it cannot be inferred from the reading afterwards. */
  absent: string;
  /** The same, for a configuration with the page reader switched off. Two
   *  sentences rather than one with a clause, because they are two different
   *  outcomes: falling through to a worse reader, and not being read. */
  absentWithNoPageReader: string;
}> = [
  {
    role: "formula",
    label: "Formulas",
    button: "Download formula reader",
    absent:
      "Not installed — formulas will go to the page reader instead, which on measurement reads almost none of them. Reading still works, and installing this later re-reads the documents that have formulas in them.",
    absentWithNoPageReader:
      "Not installed, and the page reader is switched off — so formulas are not read at all. Installing this later re-reads the documents that have formulas in them.",
  },
  {
    role: "table",
    label: "Tables",
    button: "Download table reader",
    absent:
      "Not installed — ruled tables will go to the page reader instead, which on measurement re-typed a third of them correctly and returned prose for the rest. Reading still works, and installing this later re-reads the documents that have tables in them.",
    absentWithNoPageReader:
      "Not installed, and the page reader is switched off — so ruled tables are not read at all. Installing this later re-reads the documents that have tables in them.",
  },
];

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
  /** The roles switched off. A settings file older than the field has none,
   *  which is every reader spent — the same reading it was already getting.
   *  Memoized so the absent case is one array rather than a fresh one per
   *  render, which is what the diagram's own memo is keyed on. */
  const disabledRoles = useMemo(
    () => analysis.disabled_roles ?? [],
    [analysis.disabled_roles],
  );
  const [supportedEngines, setSupportedEngines] = useState<RecognitionEngine[]>([]);
  const [detector, setDetector] = useState<InstallableModelStatus | null>(null);
  const [recognizers, setRecognizers] = useState<RecognizerDescriptor[]>([]);
  const [modelFilter, setModelFilter] = useState("");
  const [draftEngine, setDraftEngine] = useState<RecognitionEngine>(analysis.engine);
  const [draftModel, setDraftModel] = useState<string | null>(analysis.model);
  const [inventory, setInventory] = useState<RecognizerInventory | null>(null);
  const [helperInventory, setHelperInventory] = useState<
    Partial<Record<RecognizerRole, RecognizerInventory>>
  >({});
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [describerModel, setDescriberModel] = useState(analysis.describer_model ?? "");
  // Which box of the diagram is being read about. The page reader's box to
  // begin with, because it is the one choice that is always already made.
  const [region, setRegion] = useState<string>(PAGE_REGION);

  useEffect(() => {
    setDescriberModel(analysis.describer_model ?? "");
  }, [analysis.describer_model]);

  // The catalogue holds both roles in one list. Split by the role the backend
  // declares rather than by a list of ids kept here: the engine picker below
  // must never offer a formula reader as the page recognizer, and a second
  // copy of which-is-which is how it eventually would.
  const pageReaders = useMemo(
    () => recognizers.filter((model) => model.role === "page"),
    [recognizers],
  );
  // The cards above the picker, one per role in HELPER_READERS that this build
  // actually ships. A role with no catalogue row is a build that cannot read
  // that kind at all, and it is simply absent rather than shown as broken.
  const helpers = useMemo(
    () =>
      HELPER_READERS.flatMap((copy) => {
        const model = recognizers.find((entry) => entry.role === copy.role);
        return model ? [{ copy, model }] : [];
      }),
    [recognizers],
  );

  /** One box per role reader, and the kind each box stands for taken from the
   *  reader's own `emits` rather than from its role name. The role says which
   *  card it is; `emits` says which part of a page it takes, and that is what
   *  the diagram is drawing. A row that declares no kind is a catalogue that
   *  cannot say what the model reads — it is dropped from the diagram and said
   *  out loud, rather than shown as an unfillable box. */
  const helperBoxes = useMemo(
    () =>
      helpers.flatMap(({ copy, model }) => {
        const kind = model.emits[0];
        if (!kind) {
          console.error(
            `recognizer ${model.model_id} has role ${model.role} but declares no emitted kind; omitting its box`,
          );
          return [];
        }
        return [
          {
            kind,
            label: copy.label,
            // Absent from the list is chosen. Said here rather than in the
            // diagram, so the diagram never has to know what a role is.
            model: { ...model, selected: !disabledRoles.includes(model.role) },
          },
        ];
      }),
    [helpers, disabledRoles],
  );

  /** The role reader whose box is being read about, or null while the page
   *  reader's own box is. */
  const focused = useMemo(
    () =>
      helpers.find(({ model }) => model.emits[0] === region) ?? null,
    [helpers, region],
  );

  const refreshCatalogue = useCallback(async () => {
    const catalogue = await api.imageRecognizerCatalogue();
    setSupportedEngines(catalogue.engines);
    setRecognizers(catalogue.models);
    setDetector(catalogue.detector);
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
        setDetector(catalogue.detector);
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
      pageReaders.find((model) => model.engine === engine && model.is_engine_default)
        ?.model_id ?? null,
    [pageReaders],
  );

  const configuredModel = analysis.model ?? engineDefault(analysis.engine);

  const effectiveDraftModel = draftModel ?? engineDefault(draftEngine);

  const catalogModels: RecognizerCatalogEntry[] = pageReaders
    .filter((model) => model.engine === draftEngine)
    .map((model) => ({
      ...model,
      size_bytes: model.footprint_bytes,
      is_recommended: model.is_engine_default && !model.is_default,
    }));

  const selected =
    catalogModels.find((model) => model.model_id === effectiveDraftModel) ?? null;
  const configured = pageReaders.find(
    (model) => model.engine === analysis.engine && model.model_id === configuredModel,
  );
  const configuredInstalled = configured?.is_cached ?? false;
  const isConfigured =
    draftEngine === analysis.engine && effectiveDraftModel === configuredModel;

  /** Whether the page reader is one of the roles switched off. What it costs
   *  is said in several places below, and it is a different sentence from the
   *  one a specialist's absence carries — with no page reader there is nothing
   *  for an unread area to fall through to. */
  const pageOff = !!selected && disabledRoles.includes(selected.role);

  /** The roles this installation could read with: on disk, and therefore
   *  attachable. A role with no weights already contributes nothing, so it is
   *  the last *installed* one that must not be switched off — the backend
   *  refuses an analyzer with no reader left. */
  const readableRoles = useMemo(() => {
    const roles: RecognizerRole[] = [];
    if (selected?.is_cached) roles.push(selected.role);
    for (const { model } of helpers) {
      if (model.is_cached) roles.push(model.role);
    }
    return roles;
  }, [selected, helpers]);

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

  // Fetched, not read off the descriptor: the catalogue row says what a model
  // is and what it costs, and the inventory says under what terms and which
  // files. The licence is disclosed beside the download button, so it is
  // fetched whether or not the model is installed.
  useEffect(() => {
    let mounted = true;
    setHelperInventory({});
    for (const { copy, model } of helpers) {
      api
        .imageRecognizerInventory(model.engine, model.model_id)
        .then((next) => {
          if (mounted) setHelperInventory((prev) => ({ ...prev, [copy.role]: next }));
        })
        .catch((cause) => {
          if (!mounted) return;
          console.error(
            `imageRecognizerInventory failed for the ${copy.role} reader:`,
            cause,
          );
        });
    }
    return () => {
      mounted = false;
    };
  }, [api, helpers]);

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

  /** Select or de-select one region's reader.
   *
   *  Every region is one role in the same list, the page reader included. The
   *  weights are left alone either way — what changes is whether the reader is
   *  attached, and therefore what the extraction recipe is and which documents
   *  are re-read. Switching the page reader off leaves the specialists reading
   *  the areas the detector marks out for them, and everything else in a
   *  picture unread.
   *
   *  The feature switch is a different question and stays where it is: `enabled`
   *  says whether pictures are looked at at all, and this list says who reads
   *  what once they are.
   *
   *  A region with no reader in the catalogue cannot be toggled; the caller
   *  never offers one, and a call for one is a bug rather than a no-op, so it
   *  is logged rather than swallowed. */
  const handleToggleRegion = async (region: string, next: boolean) => {
    const role =
      region === PAGE_REGION
        ? selected?.role
        : helpers.find(({ model }) => model.emits[0] === region)?.model.role;
    if (!role) {
      console.error(`no reader owns the '${region}' region; nothing to select`);
      return;
    }
    // The backend refuses an analyzer with nothing left to read with, because
    // that configuration reports as a library whose pictures hold no text.
    // Said here too, before the write, so the refusal is not the first the
    // user hears of it.
    const after = next
      ? disabledRoles.filter((off) => off !== role)
      : [...disabledRoles.filter((off) => off !== role), role];
    if (!next && readableRoles.every((each) => after.includes(each))) {
      setError(
        "That would switch off every reader. Turn off \u201cread the text drawn inside " +
          "pictures\u201d above instead — it is the same reading, said once.",
      );
      return;
    }
    setBusy(true);
    try {
      await patch({ disabled_roles: after });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

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

  const handleInstallDetector = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.installLayoutDetector();
      await refreshCatalogue();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  // The same call the recognizer picker makes. There is no second install
  // path: a formula reader and a table reader are catalogue rows, and
  // installing one is installing a recognizer.
  const handleInstallHelper = async (model: RecognizerDescriptor) => {
    setBusy(true);
    setError(null);
    try {
      await api.installImageRecognizer(model.engine, model.model_id);
      await refreshCatalogue();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  const handleScope = async (scope: ImageScope) => {
    if (scope === (analysis.scope ?? DEFAULTS.scope)) return;
    setBusy(true);
    try {
      await patch({ scope });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  };

  /** The page reader's box. De-selected like any other reader: what it holds
   *  is whatever no specialist claimed — charts, embedded rasters, and a kind
   *  whose own reader is absent — and switching it off leaves those unread
   *  rather than switching the feature off. */
  const pageBox: VennModel | null = selected
    ? { ...selected, selected: !disabledRoles.includes(selected.role) }
    : null;

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
          Text in pictures
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
              Read the text a page draws rather than sets — formulas, ruled
              tables, diagrams and scanned figures
            </span>
          </label>
          {!configuredInstalled && (
            <p className="text-[10px] text-[var(--text-dim)]">
              Nothing to read with yet — install a recognizer below first.
            </p>
          )}
        </div>
      </section>

      {/* What marks the areas out. Above the engine picker on purpose: a
          recognizer with nothing to point it at reads no mathematics, and the
          order of the sections is the order of the decisions. */}
      {detector && (
        <section>
          <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
            Layout detection
          </h3>
          <div className="space-y-2 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
            <div className="flex items-center gap-2 pt-1">
              <span className="font-mono text-[10px] text-[var(--text-muted)]">
                {detector.inventory.name}
              </span>
              <span className="text-[9px] text-[var(--text-dim)]">
                {formatModelBytes(detector.inventory.footprint_bytes)}
              </span>
              <a
                href={detector.inventory.license_url}
                target="_blank"
                rel="noreferrer"
                className="text-[9px] text-[var(--accent-blue)] underline"
              >
                {detector.inventory.license}
              </a>
              {detector.is_installed ? (
                <span className="ml-auto rounded bg-[var(--bg-active)] px-1.5 py-0.5 text-[9px] uppercase tracking-tighter text-[var(--text-dim)]">
                  Installed
                </span>
              ) : (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void handleInstallDetector()}
                  className="ml-auto rounded border border-[var(--border-main)] px-2 py-1 text-[10px] text-[var(--text-main)] disabled:opacity-50"
                >
                  {busy ? "Working…" : "Download detector"}
                </button>
              )}
            </div>
            {!detector.is_installed && (
              <p className="text-[10px] text-[var(--text-dim)]">
                Not installed — nothing a page typesets will be marked out, so
                no formula or ruled table is read. Installing it later re-reads
                the documents that have them.
              </p>
            )}
          </div>
        </section>
      )}

      {/* What reads what, drawn as the containment it is rather than listed as
          three unrelated downloads. The two role readers used to have a
          section each; a user could read both and still not know that
          installing one *takes formulas away* from the page reader they had
          chosen. The boxes say it in one look, which is the whole of the
          explanation this page offers. */}
      {helperBoxes.length > 0 && (
        <section>
          <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
            Model coverage
          </h3>
          <div className="space-y-3 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
            <RecognizerVenn
              page={pageBox}
              specialists={helperBoxes}
              active={analysis.enabled}
              focus={region}
              onFocus={setRegion}
              onToggle={(target, next) => void handleToggleRegion(target, next)}
              disabled={busy}
            />

            {focused && !focused.model.is_cached && (
              <p className="text-[10px] text-[var(--text-dim)]">
                {pageOff ? focused.copy.absentWithNoPageReader : focused.copy.absent}
              </p>
            )}

            {focused && focused.model.is_cached && disabledRoles.includes(focused.model.role) && (
              <p className="text-[10px] text-[var(--text-dim)]">
                {pageOff
                  ? "Switched off, and still downloaded — and the page reader is off too, so these areas are not read at all."
                  : "Switched off, and still downloaded — these areas go to the page reader."}
              </p>
            )}

            {pageOff && analysis.enabled && (
              <p className="text-[10px] text-[var(--text-dim)]">
                The page reader is switched off: only the areas the detector
                marks out for a reader above are read. Charts, scanned figures
                and the text drawn inside a diagram are not.
              </p>
            )}

            {!analysis.enabled && (
              <p className="text-[10px] text-[var(--text-dim)]">
                Nothing is painted because nothing is read: image analysis is
                off.
              </p>
            )}

            {/* One row per role reader, always shown. The diagram says whether
                a kind is covered; this says what covering it costs and under
                what licence, which is the half a colour cannot carry. */}
            <div className="space-y-1.5 border-t border-[var(--border-main)] pt-2">
              {helpers.map(({ copy, model }) => (
                <div key={copy.role} className="flex items-center gap-2">
                  <span className="font-mono text-[10px] text-[var(--text-muted)]">
                    {model.display_name}
                  </span>
                  <span className="text-[9px] text-[var(--text-dim)]">
                    {formatModelBytes(model.footprint_bytes)}
                  </span>
                  {helperInventory[copy.role] && (
                    <a
                      href={helperInventory[copy.role]!.license_url}
                      target="_blank"
                      rel="noreferrer"
                      className="text-[9px] text-[var(--accent-blue)] underline"
                    >
                      {helperInventory[copy.role]!.license}
                    </a>
                  )}
                  {model.is_cached ? (
                    <span className="ml-auto rounded bg-[var(--bg-active)] px-1.5 py-0.5 text-[9px] uppercase tracking-tighter text-[var(--text-dim)]">
                      Installed
                    </span>
                  ) : (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void handleInstallHelper(model)}
                      className="ml-auto rounded border border-[var(--border-main)] px-2 py-1 text-[10px] text-[var(--text-main)] disabled:opacity-50"
                    >
                      {busy ? "Working…" : copy.button}
                    </button>
                  )}
                </div>
              ))}
            </div>
          </div>
        </section>
      )}

      {/* Which pictures. Hidden while the feature is off, because it is a
          question about work that is not being done. It is the setting that
          decides whether this feature costs seconds or an overnight run, so
          it sits directly under the toggle rather than among the model
          options. */}
      {analysis.enabled && (
        <section>
          <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
            Scope
          </h3>
          <div className="space-y-2 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
            {SCOPE_OPTIONS.map((option) => (
              <label
                key={option.value}
                className="flex cursor-pointer items-center gap-2.5"
              >
                <input
                  type="radio"
                  name="image-scope"
                  value={option.value}
                  checked={(analysis.scope ?? DEFAULTS.scope) === option.value}
                  disabled={busy}
                  onChange={() => void handleScope(option.value)}
                  className="h-3.5 w-3.5 accent-[var(--accent-blue)]"
                />
                <span className="text-xs text-[var(--text-main)]">
                  {option.label}
                </span>
              </label>
            ))}
            {pageOff && (
              <p className="text-[10px] text-[var(--text-dim)]">
                The page reader is switched off, and it is the only reader a
                raster goes to — so the second choice reads nothing the first
                does not, and the pictures are found and counted rather than
                decoded.
              </p>
            )}
          </div>
        </section>
      )}

      {/* Engine selection */}
      <section>
        <h3 className="mb-2 text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          Engine
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
          <div className="rounded-lg border border-[var(--accent-amber-border)] bg-[var(--accent-amber-muted)] p-1">
            <p className="text-center text-[10px] leading-relaxed text-[var(--accent-amber)]">
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
          Descriptions
        </h3>
        <div className="space-y-3 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3">
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
