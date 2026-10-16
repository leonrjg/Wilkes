import { useCallback, useEffect, useRef, useState } from "react";
import type { SearchApi } from "../services/api";
import type {
  ImageAnalysisSettings, RecognizerCatalogue, RecognizerDescriptor,
  RecognizerInventory, RecognizerRole, Settings,
} from "../lib/types";
import { ocrConfiguration } from "../lib/ocrConfiguration";
import { formatModelBytes } from "./ModelCatalog";

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdateSettings: (patch: Partial<Settings>) => Promise<void>;
}

const DEFAULTS: ImageAnalysisSettings = {
  enabled: false, engine: "Onnx", model: null, device: null,
  describer_model: "", scope: "typeset_only", disabled_roles: ["page"],
};
const ENGINE_LABELS = { Onnx: "ONNX", Candle: "Candle", Vision: "Apple Vision" };
const modelKey = (model: RecognizerDescriptor) => `${model.engine}:${model.model_id}`;
const box = "rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] p-3";
const button = "rounded border border-[var(--border-main)] px-2.5 py-1.5 text-xs text-[var(--text-main)] disabled:opacity-50";
const select = "w-full rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-1.5 text-xs text-[var(--text-main)] disabled:opacity-50";

function Badge({ children }: { children: React.ReactNode }) {
  return <span className="rounded bg-[var(--bg-active)] px-2 py-1 text-[10px] text-[var(--text-muted)]">{children}</span>;
}

function ModelDetails({ model, inventory }: { model?: RecognizerDescriptor; inventory: RecognizerInventory }) {
  return <details className="text-[10px] text-[var(--text-dim)]">
    <summary className="cursor-pointer">Model details</summary>
    <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 break-all">
      {model && <><dt>Engine</dt><dd>{ENGINE_LABELS[model.engine]}</dd>
        <dt>Outputs</dt><dd>{model.emits.join(", ")}</dd>
        <dt>Admission threshold</dt><dd>{model.admission_threshold.toFixed(2)}</dd></>}
      <dt>Repository</dt><dd>{inventory.repo}</dd>
      <dt>Revision</dt><dd className="font-mono">{inventory.revision}</dd>
      <dt>Sources</dt><dd>{inventory.derived_from.join(", ")}</dd>
    </dl>
    <ul className="mt-2 space-y-1 font-mono">
      {inventory.artifacts.map((artifact) => <li key={artifact.filename}>
        {artifact.filename} · {formatModelBytes(artifact.size_bytes)} · SHA256 {artifact.sha256}
      </li>)}
    </ul>
  </details>;
}

function ModelInfo({ model, inventory, retry }: {
  model: RecognizerDescriptor; inventory?: RecognizerInventory; retry: () => void;
}) {
  return <div className="space-y-2">
    <div className="flex flex-wrap items-center gap-2 text-[10px] text-[var(--text-dim)]">
      <span>{formatModelBytes(model.footprint_bytes)}</span>
      {inventory ? <a href={inventory.license_url} target="_blank" rel="noreferrer"
        className="text-[var(--accent-blue)] underline">{inventory.license}</a>
        : <button type="button" onClick={retry} className="underline">Load model details</button>}
      <Badge>{model.is_cached ? "Installed" : "Download required"}</Badge>
    </div>
    {inventory && <ModelDetails model={model} inventory={inventory} />}
  </div>;
}

/** One configuration surface. Catalogue-backed readiness always uses saved
 * settings; model pickers remain drafts until their installation completes. */
export default function ImageAnalysisPanel({ api, settings, onUpdateSettings }: Props) {
  const analysis = settings.image_analysis ?? DEFAULTS;
  const [catalogue, setCatalogue] = useState<RecognizerCatalogue | null>(null);
  const [catalogueLoading, setCatalogueLoading] = useState(true);
  const catalogueRequest = useRef(0);
  const [draftPage, setDraftPage] = useState<string | null>(null);
  const [draftFormula, setDraftFormula] = useState<string | null>(null);
  const [inventories, setInventories] = useState<Record<string, RecognizerInventory>>({});
  const [inventoryRevision, setInventoryRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const operation = useRef(false);
  const [progress, setProgress] = useState<{ received: number; total: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [describerModel, setDescriberModel] = useState(analysis.describer_model ?? "");
  const [descriptionRequested, setDescriptionRequested] = useState(!!analysis.describer_model);

  const refreshCatalogue = useCallback(async () => {
    const request = ++catalogueRequest.current;
    const next = await api.imageRecognizerCatalogue();
    if (request === catalogueRequest.current) setCatalogue(next);
    return next;
  }, [api]);

  useEffect(() => {
    let mounted = true;
    const request = ++catalogueRequest.current;
    setCatalogue(null);
    setCatalogueLoading(true);
    api.imageRecognizerCatalogue().then((next) => {
      if (mounted && request === catalogueRequest.current) setCatalogue(next);
    }).catch((cause) => {
      if (mounted) setError(String(cause));
    }).finally(() => { if (mounted) setCatalogueLoading(false); });
    return () => { mounted = false; };
  }, [api]);

  useEffect(() => { setDraftPage(null); }, [analysis.engine, analysis.model]);
  useEffect(() => { setDraftFormula(null); }, [analysis.formula_model]);
  useEffect(() => {
    setDescriberModel(analysis.describer_model ?? "");
    setDescriptionRequested(!!analysis.describer_model);
  }, [analysis.describer_model]);

  // Disclosure is fetched before installation. An absent inventory cannot
  // authorize a download; retry remains available beside that model.
  useEffect(() => {
    let mounted = true;
    setInventories({});
    if (catalogue) {
      void Promise.all(catalogue.models.map(async (model) => {
        try {
          const inventory = await api.imageRecognizerInventory(model.engine, model.model_id);
          if (mounted) setInventories((previous) => ({ ...previous, [modelKey(model)]: inventory }));
        } catch (cause) {
          console.error(`Model inventory unavailable: ${model.model_id}`, cause);
        }
      }));
    }
    return () => { mounted = false; };
  }, [api, catalogue, inventoryRevision]);

  useEffect(() => {
    let mounted = true;
    const unlisteners: Array<() => void> = [];
    const track = (subscription: Promise<() => void>) => {
      subscription.then((unlisten) => {
        if (mounted) unlisteners.push(unlisten); else unlisten();
      }).catch((cause) => { if (mounted) setError(String(cause)); });
    };
    track(api.onImageAnalysisProgress((next) => {
      if (!mounted || !("Download" in next)) return;
      setBusy(true);
      setProgress({ received: next.Download.bytes_received, total: next.Download.total_bytes });
    }));
    const settled = () => {
      if (!mounted) return;
      // A setup action may download multiple components. A terminal event for
      // one component must not unlock the controls for the remaining steps.
      if (!operation.current) { setBusy(false); setProgress(null); }
      refreshCatalogue().catch((cause) => { if (mounted) setError(String(cause)); });
    };
    track(api.onImageAnalysisDone(settled));
    track(api.onImageAnalysisError((failure) => {
      if (mounted) setError(failure.message);
      settled();
    }));
    return () => { mounted = false; unlisteners.forEach((unlisten) => unlisten()); };
  }, [api, refreshCatalogue]);

  const run = async (action: () => Promise<void>) => {
    if (operation.current) return;
    operation.current = true;
    setBusy(true);
    setError(null);
    try { await action(); }
    catch (cause) { setError(String(cause)); }
    finally { operation.current = false; setBusy(false); setProgress(null); }
  };
  const patch = async (next: Partial<ImageAnalysisSettings>, readinessCatalogue = catalogue) => {
    const updated = { ...analysis, ...next };
    if (updated.enabled && readinessCatalogue && !ocrConfiguration(updated, readinessCatalogue).canEnable) {
      throw new Error("Complete OCR setup before applying this configuration.");
    }
    await onUpdateSettings({ image_analysis: updated });
  };

  if (!catalogue) return <div className="space-y-3 p-1">
    <h3 className="text-xs text-[var(--text-main)]">OCR</h3>
    {catalogueLoading ? <p role="status" className="text-xs text-[var(--text-dim)]">Loading OCR components…</p>
      : <button className={button} onClick={() => void run(async () => { await refreshCatalogue(); })}
        disabled={busy}>Retry loading OCR components</button>}
    {error && <p role="alert" className="text-xs text-[var(--accent-red)]">{error}</p>}
  </div>;

  const config = ocrConfiguration(analysis, catalogue);
  const pageReaders = catalogue.models.filter((model) => model.role === "page");
  const formulaReaders = catalogue.models.filter((model) => model.role === "formula");
  const pageChoice = draftPage === null ? config.page : pageReaders.find((model) => modelKey(model) === draftPage);
  const formulaChoice = draftFormula === null ? config.formula : formulaReaders.find((model) => model.model_id === draftFormula);
  const pageChanged = !!pageChoice && modelKey(pageChoice) !== (config.page && modelKey(config.page));
  const formulaChanged = !!formulaChoice && formulaChoice.model_id !== config.formula?.model_id;
  const retryInventory = () => setInventoryRevision((previous) => previous + 1);
  const detector = catalogue.detector;
  const toggleRole = (role: RecognizerRole, enabled: boolean) => void run(async () => {
    const disabled_roles = (analysis.disabled_roles ?? []).filter((entry) => entry !== role);
    if (!enabled) disabled_roles.push(role);
    await patch({ disabled_roles });
  });

  const installAndApply = (model: RecognizerDescriptor, needsLayout: boolean, changed: boolean) => void run(async () => {
    if (!model.is_cached) await api.installImageRecognizer(model.engine, model.model_id);
    if (needsLayout && detector && !detector.is_installed) await api.installLayoutDetector();
    const nextCatalogue = await refreshCatalogue();
    if (changed) {
      await patch(model.role === "page"
        ? { engine: model.engine, model: model.model_id }
        : { formula_model: model.model_id }, nextCatalogue);
      if (model.role === "page") setDraftPage(null); else setDraftFormula(null);
    }
  });

  const setupAction = (model: RecognizerDescriptor, needsLayout: boolean, changed: boolean, label: string) => {
    const missingLayout = needsLayout && !!detector && !detector.is_installed;
    const missingModel = !model.is_cached;
    if (!missingModel && !missingLayout && !changed) return null;
    const size = (missingModel ? model.footprint_bytes : 0)
      + (missingLayout ? detector!.inventory.footprint_bytes : 0);
    return <div className="flex flex-wrap items-center gap-2">
      <button type="button" className={button}
        disabled={busy || (missingModel && !inventories[modelKey(model)])}
        onClick={() => installAndApply(model, needsLayout, changed)}>
        {missingLayout ? `Install required models for ${label}`
          : missingModel ? `Install ${label} reader${changed ? " and use it" : ""}` : `Use selected ${label} reader`}
      </button>
      {size > 0 && <span className="text-[10px] text-[var(--text-dim)]">{formatModelBytes(size)} download</span>}
    </div>;
  };

  const percent = progress && progress.total > 0 ? Math.round(100 * progress.received / progress.total) : 0;
  return <div className="flex flex-col gap-4 p-1">
    <section className={box}>
      <div className="flex items-center justify-between gap-3">
        <label className="flex items-center gap-2.5 text-sm font-medium text-[var(--text-main)]">
          <input type="checkbox" aria-label="Enable OCR" checked={analysis.enabled}
            disabled={busy || (!analysis.enabled && !config.canEnable)}
            onChange={(event) => void run(() => patch({ enabled: event.target.checked }))}
            className="accent-[var(--accent-blue)]" />OCR
        </label>
        <span role="status"><Badge>{config.status}</Badge></span>
      </div>
      {!analysis.enabled && !config.canEnable && <div className="mt-2 text-xs text-[var(--text-dim)]">Setup required</div>}
    </section>

    <section aria-label="Page regions" className="space-y-3">
      <h3 className="text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">Page regions</h3>
      <div className={`${box} space-y-2`}>
        <div className="flex flex-wrap items-center gap-2 text-xs text-[var(--text-main)]">
          <span className="font-medium">Layout detection</span><Badge>Required for page regions</Badge>
          <span className="ml-auto"><Badge>{detector ? detector.is_installed ? "Installed" : "Download required" : "Unavailable in this build"}</Badge></span>
        </div>
        {detector && <>
          <div className="flex flex-wrap items-center gap-2 text-[10px] text-[var(--text-dim)]">
            <span>{detector.inventory.name}</span><span>{formatModelBytes(detector.inventory.footprint_bytes)}</span>
            <a href={detector.inventory.license_url} target="_blank" rel="noreferrer" className="text-[var(--accent-blue)] underline">{detector.inventory.license}</a>
            {!detector.is_installed && <button type="button" className={`${button} ml-auto`} disabled={busy}
              onClick={() => void run(async () => { await api.installLayoutDetector(); await refreshCatalogue(); })}>Install layout detection</button>}
          </div>
          <ModelDetails inventory={detector.inventory} />
        </>}
      </div>

      <div className="ml-3 space-y-3 border-l-2 border-[var(--border-main)] pl-3">
        {(["formula", "table"] as const).map((role) => {
          const label = role === "formula" ? "Formulas" : "Tables";
          const model = role === "formula" ? formulaChoice : config.table;
          const saved = role === "formula" ? config.formula : config.table;
          return <section key={role} aria-label={label} className={`${box} space-y-3`}>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="text-xs font-medium text-[var(--text-main)]">{label}</h4>
              <Badge>{!config.layoutReady ? detector ? "Requires layout detection" : "Unavailable in this build"
                : !analysis.enabled ? "OCR off" : "Layout ready"}</Badge>
            </div>
            <div className="text-[11px] text-[var(--text-muted)]">Effective reader: {analysis.enabled ? config.effectiveReader(role) : "OCR off"}</div>
            <label className="flex items-center gap-2 text-xs text-[var(--text-main)]">
              <input type="checkbox" checked={config.selected(role)} disabled={busy || !saved?.is_cached}
                onChange={(event) => toggleRole(role, event.target.checked)} className="accent-[var(--accent-blue)]" />
              Use dedicated {role} reader
            </label>
            {role === "formula" && <select aria-label="Formula model" className={select}
              disabled={busy || formulaReaders.length === 0}
              value={formulaChoice?.model_id ?? analysis.formula_model ?? ""}
              onChange={(event) => setDraftFormula(event.target.value)}>
              {!config.formula && <option value={analysis.formula_model ?? ""}>{analysis.formula_model ? `Unavailable: ${analysis.formula_model}` : "Unavailable in this build"}</option>}
              {formulaReaders.map((entry) => <option key={entry.model_id} value={entry.model_id}>{entry.display_name}</option>)}
            </select>}
            {model ? <>
              {role === "table" && <div className="text-xs text-[var(--text-main)]">{model.display_name}</div>}
              {role === "formula" && formulaChanged && <Badge>Selected model · not applied</Badge>}
              <ModelInfo model={model} inventory={inventories[modelKey(model)]} retry={retryInventory} />
              {setupAction(model, true, role === "formula" && formulaChanged, role)}
            </> : <Badge>{role === "formula" && analysis.formula_model ? "Unavailable saved model" : "Dedicated reader unavailable in this build"}</Badge>}
          </section>;
        })}

        <section aria-label="General reader" className={`${box} space-y-3`}>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label className="flex items-center gap-2 text-xs font-medium text-[var(--text-main)]">
              <input type="checkbox" checked={config.selected("page")} disabled={busy || (!config.selected("page") && !config.page?.is_cached)}
                onChange={(event) => toggleRole("page", event.target.checked)} className="accent-[var(--accent-blue)]" />Use general reader
            </label><Badge>Optional with dedicated readers</Badge>
          </div>
          <div className="text-[11px] text-[var(--text-muted)]">Saved reader: {config.page?.display_name ?? `Unavailable: ${analysis.model ?? ENGINE_LABELS[analysis.engine]}`}</div>
          <div className="flex flex-wrap gap-2"><Badge>Other page regions · {!analysis.enabled ? "OCR off" : !config.layoutReady ? "Requires layout detection" : config.pageReady && config.validReaders ? "General reader" : "None"}</Badge><Badge>Embedded images</Badge></div>
          <select aria-label="General reader model" className={select} disabled={busy || pageReaders.length === 0}
            value={pageChoice ? modelKey(pageChoice) : `${analysis.engine}:${analysis.model ?? ""}`}
            onChange={(event) => setDraftPage(event.target.value)}>
            {!config.page && <option value={`${analysis.engine}:${analysis.model ?? ""}`}>Unavailable: {analysis.model ?? ENGINE_LABELS[analysis.engine]}</option>}
            {pageReaders.map((model) => <option key={modelKey(model)} value={modelKey(model)}>
              {model.display_name} · {ENGINE_LABELS[model.engine]}{model.is_default ? " · Default" : ""}
            </option>)}
          </select>
          {pageChoice && <>
            {pageChanged && <Badge>Selected model · not applied</Badge>}
            <ModelInfo model={pageChoice} inventory={inventories[modelKey(pageChoice)]} retry={retryInventory} />
            {setupAction(pageChoice, analysis.scope !== "typeset_and_embedded", pageChanged, "general")}
          </>}
          {(config.page?.engine === "Candle" || config.formula?.engine === "Candle") && <details className="text-[10px] text-[var(--text-dim)]">
            <summary className="cursor-pointer">Device settings</summary>
            <select aria-label="Recognition device" className={`${select} mt-2`} value={analysis.device ?? "auto"} disabled={busy}
              onChange={(event) => void run(() => patch({ device: event.target.value === "auto" ? null : event.target.value }))}>
              <option value="auto">Auto (Metal preferred)</option><option value="metal">Require Metal</option><option value="cpu">CPU</option>
            </select>
          </details>}
        </section>
      </div>
    </section>

    <section aria-label="Embedded images" className={`${box} space-y-2`}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <label className="flex items-center gap-2 text-xs text-[var(--text-main)]">
          <input type="checkbox" checked={analysis.scope === "typeset_and_embedded"} disabled={busy || (analysis.scope !== "typeset_and_embedded" && !config.pageReady)}
            onChange={(event) => void run(() => patch({ scope: event.target.checked ? "typeset_and_embedded" : "typeset_only" }))}
            className="accent-[var(--accent-blue)]" />Include embedded images
        </label><Badge>Requires general reader</Badge>
      </div>
      <div className="text-[11px] text-[var(--text-muted)]">Effective reader: {analysis.enabled ? config.imageReader : "OCR off"}</div>
    </section>

    <details className={box} open={descriptionRequested || undefined}>
      <summary className="cursor-pointer text-xs text-[var(--text-main)]">Image descriptions <span className="ml-2 text-[10px] text-[var(--text-dim)]">Optional</span></summary>
      <div className="mt-3 space-y-3">
        <label className="flex items-center gap-2 text-xs text-[var(--text-main)]">
          <input type="checkbox" checked={descriptionRequested} disabled={busy}
            onChange={(event) => {
              const enabled = event.target.checked;
              if (enabled) setDescriptionRequested(true);
              else if (analysis.describer_model) void run(async () => { await patch({ describer_model: "" }); setDescriptionRequested(false); });
              else setDescriptionRequested(false);
            }} className="accent-[var(--accent-blue)]" />Enable image descriptions
        </label>
        {descriptionRequested && <div className="flex gap-2">
          <input aria-label="Ollama description model" className={select} placeholder="qwen3-vl:2b" value={describerModel} disabled={busy}
            onChange={(event) => setDescriberModel(event.target.value)} />
          <button type="button" className={button} disabled={busy || !describerModel.trim() || describerModel.trim() === analysis.describer_model}
            onClick={() => void run(() => patch({ describer_model: describerModel.trim() }))}>Save</button>
        </div>}
        <div className="text-[10px] text-[var(--text-dim)]">Image destination: <span className="font-mono">{settings.generation?.ollama_url}</span></div>
        <Badge>{!analysis.describer_model ? "Off" : analysis.enabled && config.pageReady && analysis.scope === "typeset_and_embedded" ? "Configured" : "Paused · requires OCR and embedded images"}</Badge>
      </div>
    </details>

    <div className="text-[10px] text-[var(--text-dim)]">OCR changes reprocess affected documents.</div>
    {progress && <div className="space-y-1 text-[10px] text-[var(--text-muted)]" role="status">
      <div>Downloading · {formatModelBytes(progress.received)}{progress.total > 0 ? ` / ${formatModelBytes(progress.total)} · ${percent}%` : ""}</div>
      <progress aria-label="Model download" className="w-full accent-[var(--accent-blue)]" value={progress.total > 0 ? progress.received : undefined} max={progress.total || undefined} />
    </div>}
    {busy && !progress && <div role="status" className="text-xs text-[var(--text-dim)]">Working…</div>}
    {error && <p role="alert" className="rounded border border-[var(--accent-red)] p-2 text-xs text-[var(--accent-red)]">{error}</p>}
  </div>;
}
