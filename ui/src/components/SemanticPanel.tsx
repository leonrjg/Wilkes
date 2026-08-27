import { useState, useEffect, useCallback, useReducer } from "react";
import {
  ALL_ENGINES,
  type EmbedderModel,
  type EmbedProgress,
  type EmbedDone,
  type EmbedError,
  type ModelDescriptor,
  type SemanticSettings,
  type SelectedEmbedder,
  type IndexStatus,
  type EmbeddingEngine,
} from "../lib/types";
import type { SearchApi } from "../services/api";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";
import LogsPanel from "./LogsPanel";
import {CornerLeftDown, CornerRightUp} from "react-feather";
import { Tooltip } from "@leonrjg/wilkes-reader";
import ModelCatalog from "./ModelCatalog";

// ---------------------------------------------------------------------------
// State & reducer
// ---------------------------------------------------------------------------

type ActiveOp = "downloading" | "building" | null;

interface ProgressState {
  current: number;
  total: number;
}

interface PendingBuild {
  directory: string;
  selected: SelectedEmbedder;
}

interface PanelState {
  /** Models returned by the backend — only valid for the current engine. */
  backendModels: ModelDescriptor[];
  /** Engine the backendModels were fetched for. Used to reject stale loads. */
  modelsEngine: EmbeddingEngine | null;
  indexStatus: IndexStatus | null;
  isEngineAvailable: boolean;
  error: string | null;
  activeOp: ActiveOp;
  progress: ProgressState | null;
  isCancelling: boolean;
  supportedEngines: EmbeddingEngine[];
  pythonPath: string | null;
  pythonError: string | null;
  pendingBuild: PendingBuild | null;
  buildRequest: PendingBuild | null;
}

const INITIAL_STATE: PanelState = {
  backendModels: [],
  modelsEngine: null,
  indexStatus: null,
  isEngineAvailable: true,
  error: null,
  activeOp: null,
  progress: null,
  isCancelling: false,
  supportedEngines: [],
  pythonPath: null,
  pythonError: null,
  pendingBuild: null,
  buildRequest: null,
};

type Action =
  | { type: "init_loaded"; supportedEngines: EmbeddingEngine[] }
  | { type: "models_loaded"; models: ModelDescriptor[]; engine: EmbeddingEngine }
  | { type: "models_failed"; engine: EmbeddingEngine; error: string }
  | { type: "index_loaded"; indexStatus: IndexStatus | null }
  | { type: "error"; error: string }
  | { type: "clear_error" }
  | { type: "op_started"; op: "downloading" | "building" }
  | { type: "progress"; op: "downloading" | "building"; progress: ProgressState }
  | { type: "op_done"; operation: string; indexStatus?: IndexStatus }
  | { type: "op_error"; message: string; operation: string }
  | { type: "cancel_started" }
  | { type: "cancel_completed" }
  | { type: "cancel_failed" }
  | { type: "index_deleted" }
  | { type: "python_info"; pythonPath: string | null; pythonError: string | null }
  | { type: "model_size_fetched"; modelId: string; sizeBytes: number }
  | { type: "queue_build"; build: PendingBuild }
  | { type: "launch_pending_build" }
  | { type: "build_request_dispatched" };

function reducer(state: PanelState, action: Action): PanelState {
  switch (action.type) {
    case "init_loaded":
      return { ...state, supportedEngines: action.supportedEngines };
    case "models_loaded":
      return { ...state, backendModels: action.models, modelsEngine: action.engine, isEngineAvailable: true, error: null };
    case "models_failed":
      return { ...state, backendModels: [], modelsEngine: action.engine, isEngineAvailable: false, error: action.error };
    case "index_loaded":
      return { ...state, indexStatus: action.indexStatus };
    case "error":
      return { ...state, error: action.error };
    case "clear_error":
      return { ...state, error: null };
    case "op_started":
      return {
        ...state,
        activeOp: action.op,
        progress: { current: 0, total: 0 },
        error: null,
        isCancelling: false,
      };
    case "progress":
      return { ...state, activeOp: action.op, progress: action.progress };
    case "op_done":
      return {
        ...state,
        activeOp: null,
        progress: null,
        error: null,
        isCancelling: false,
        indexStatus: action.indexStatus ?? state.indexStatus,
        pendingBuild: null,
        buildRequest: null,
      };
    case "op_error":
      return {
        ...state,
        activeOp: null,
        progress: null,
        isCancelling: false,
        error: action.message || null,
        pendingBuild: null,
        buildRequest: null,
      };
    case "cancel_started":
      return { ...state, isCancelling: true, pendingBuild: null, buildRequest: null };
    case "cancel_completed":
      return {
        ...state,
        activeOp: null,
        progress: null,
        isCancelling: false,
        error: null,
        pendingBuild: null,
        buildRequest: null,
      };
    case "cancel_failed":
      return { ...state, isCancelling: false };
    case "index_deleted":
      return { ...state, indexStatus: null };
    case "python_info":
      return { ...state, pythonPath: action.pythonPath, pythonError: action.pythonError };
    case "model_size_fetched":
      return {
        ...state,
        backendModels: state.backendModels.map((m) =>
          m.model_id === action.modelId ? { ...m, size_bytes: action.sizeBytes } : m,
        ),
      };
    case "queue_build":
      return { ...state, pendingBuild: action.build };
    case "launch_pending_build":
      if (!state.pendingBuild) return state;
      return {
        ...state,
        pendingBuild: null,
        buildRequest: state.pendingBuild,
        activeOp: "building",
        progress: { current: 0, total: 0 },
        error: null,
        isCancelling: false,
      };
    case "build_request_dispatched":
      return { ...state, buildRequest: null };
    default:
      return state;
  }
}

// ---------------------------------------------------------------------------
// Derive phase from state (never stored)
// ---------------------------------------------------------------------------

type Phase = "not_downloaded" | "downloading" | "ready" | "building" | "indexed" | "engine_mismatch";

function derivePhase(state: PanelState, sem: SemanticSettings | null): Phase {
  if (state.activeOp === "downloading") return "downloading";
  if (state.activeOp === "building") return "building";

  if (state.indexStatus && sem) {
    if (
      state.indexStatus.engine !== sem.selected.engine ||
      state.indexStatus.model_id !== sem.selected.model
    ) {
      return "engine_mismatch";
    }
    if (state.indexStatus.indexed_files > 0 && state.indexStatus.total_chunks > 0) {
      return "indexed";
    }
  }

  if (!sem) return "not_downloaded";
  if (sem.selected.engine === "SBERT") return "ready";

  const selected = state.backendModels.find((m) => m.model_id === sem.selected.model);
  if (selected?.is_cached) return "ready";
  return "not_downloaded";
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

interface Props {
  api: SearchApi;
  directory: string;
  refreshSemanticReady: () => Promise<boolean>;
}

export default function SemanticPanel({ api, directory, refreshSemanticReady }: Props) {
  const handleCurrentRootIndexRemoved = useSemanticStore((s) => s.handleCurrentRootIndexRemoved);
  const activeWorkspace = useWorkspaceStore((s) =>
    s.workspaces.find((workspace) => workspace.id === s.activeWorkspaceId) ?? null,
  );
  const readOnly = activeWorkspace?.read_only ?? false;
  const managedBy = activeWorkspace?.managed_by ?? null;
  const [state, dispatch] = useReducer(reducer, INITIAL_STATE);
  const [modelFilter, setModelFilter] = useState("");
  const [draftSelected, setDraftSelected] = useState<SelectedEmbedder | null>(null);
  const [sizeFetchingFor, setSizeFetchingFor] = useState<string | null>(null);
  const [customModelInput, setCustomModelInput] = useState("");
  const [isAddingCustom, setIsAddingCustom] = useState(false);
  const [showLogs, setShowLogs] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  // Bumped to re-trigger model + index fetches after async ops (download, build, etc.)
  const [fetchEpoch, setFetchEpoch] = useState(0);
  const settings = useSettingsStore((s) => s.semantic);
  const replaceSettings = useSettingsStore((s) => s.replaceSettings);
  const refreshSettings = useSettingsStore((s) => s.refreshSettings);

  const { indexStatus, isEngineAvailable, backendModels, supportedEngines, pendingBuild, buildRequest } = state;
  const effectiveSelected = draftSelected ?? settings?.selected;
  const effectiveSemantic = settings && effectiveSelected
    ? { ...settings, selected: effectiveSelected }
    : settings;
  const phase = derivePhase(state, effectiveSemantic);
  const isActive = phase === "downloading" || phase === "building";
  const currentEngine = effectiveSelected?.engine;

  const invalidate = useCallback(() => setFetchEpoch((e) => e + 1), []);

  useEffect(() => {
    if (phase === "building") {
      setShowLogs(true);
    }
  }, [phase]);

  // ---------------------------------------------------------------------------
  // Effect: load settings + supported engines (once on mount)
  // ---------------------------------------------------------------------------

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      api.getSettings(),
      api.getSupportedEngines().catch(() => ["SBERT"] as EmbeddingEngine[]),
    ]).then(([s, engines]) => {
      if (!cancelled) {
        replaceSettings(s);
        dispatch({ type: "init_loaded", supportedEngines: engines });
        setDraftSelected(null);
      }
    });
    return () => { cancelled = true; };
  }, [api, replaceSettings]);

  // ---------------------------------------------------------------------------
  // Effect: fetch models when engine changes (or after invalidate)
  // ---------------------------------------------------------------------------

  useEffect(() => {
    if (!settings) return;
    let cancelled = false;
    const engine = currentEngine ?? settings.selected.engine;
    api.listModels(engine).then((models) => {
      if (!cancelled) dispatch({ type: "models_loaded", models, engine });
    }).catch((e: any) => {
      if (!cancelled) dispatch({ type: "models_failed", engine, error: e.toString() });
    });
    return () => { cancelled = true; };
  }, [api, currentEngine, settings?.selected.engine, fetchEpoch]);

  // ---------------------------------------------------------------------------
  // Effect: fetch index status when engine/model changes (or after invalidate)
  // ---------------------------------------------------------------------------

  useEffect(() => {
    if (!settings) return;
    let cancelled = false;
    api.getIndexStatus(directory).then((idx) => {
      if (!cancelled) dispatch({ type: "index_loaded", indexStatus: idx });
    }).catch(() => {
      if (!cancelled) dispatch({ type: "index_loaded", indexStatus: null });
    });
    return () => { cancelled = true; };
  }, [api, directory, effectiveSelected?.engine, effectiveSelected?.model, fetchEpoch]);

  // ---------------------------------------------------------------------------
  // Effect: python info (only for SBERT)
  // ---------------------------------------------------------------------------

  useEffect(() => {
    if (currentEngine !== "SBERT") {
      dispatch({ type: "python_info", pythonPath: null, pythonError: null });
      return;
    }
    let cancelled = false;
    api.getPythonInfo().then((p) => {
      if (!cancelled) dispatch({ type: "python_info", pythonPath: p, pythonError: null });
    }).catch((e) => {
      if (!cancelled) dispatch({ type: "python_info", pythonPath: null, pythonError: e.toString() });
    });
    return () => { cancelled = true; };
  }, [api, currentEngine]);

  // ---------------------------------------------------------------------------
  // Effect: embed event subscriptions
  // ---------------------------------------------------------------------------

  useEffect(() => {
    let mounted = true;
    const unlisteners: Array<() => void> = [];

    api
      .onEmbedProgress((p: EmbedProgress) => {
        if ("Download" in p) {
          dispatch({ type: "progress", op: "downloading", progress: { current: p.Download.bytes_received, total: p.Download.total_bytes } });
        } else if ("Build" in p) {
          dispatch({ type: "progress", op: "building", progress: { current: p.Build.files_processed, total: p.Build.total_files } });
        }
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedProgress subscription failed:", e));

    api
      .onEmbedDone((done: EmbedDone) => {
        if (done.operation === "Download") {
          invalidate();
          if (pendingBuild) dispatch({ type: "launch_pending_build" });
          else dispatch({ type: "op_done", operation: "Download" });
        } else if (done.operation === "Build") {
          Promise.all([
            api.getIndexStatus(directory).catch(() => null),
            refreshSettings().catch(() => null),
          ]).then(([idx]) => {
            dispatch({
              type: "op_done",
              operation: "Build",
              indexStatus: idx ?? undefined,
            });
            refreshSemanticReady();
            setDraftSelected(null);
          });
        }
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedDone subscription failed:", e));

    api
      .onEmbedError((err: EmbedError) => {
        if (err.message) console.error(`Embed error (${err.operation}):`, err.message);
        dispatch({ type: "op_error", message: err.message, operation: err.operation });
      })
      .then((u) => { if (mounted) unlisteners.push(u); else u(); })
      .catch((e) => console.error("onEmbedError subscription failed:", e));

    return () => {
      mounted = false;
      unlisteners.forEach((u) => u());
    };
  }, [api, directory, invalidate, pendingBuild, refreshSemanticReady, refreshSettings]);

  useEffect(() => {
    if (!buildRequest) return;

    dispatch({ type: "build_request_dispatched" });
    api.buildIndex(buildRequest.directory, buildRequest.selected).catch((e) => {
      console.error("buildIndex failed after download:", e);
      dispatch({ type: "op_error", message: e?.toString?.() ?? "Build failed", operation: "Build" });
    });
  }, [api, buildRequest]);

  // Merge custom models for current engine into the backend list.
  // Computed inline — no memoization, always fresh.
  const mergedModels: ModelDescriptor[] = (() => {
    const customs: ModelDescriptor[] = (settings?.custom_models ?? [])
      .filter((m) => m.engine === currentEngine)
      .map((m) => ({
        model_id: m.model_id,
        display_name: m.model_id.split("/").pop() || m.model_id,
        description: "User-defined HuggingFace model",
        dimension: 0,
        is_cached: false,
        is_default: false,
        is_recommended: false,
        size_bytes: null,
        preferred_batch_size: 32,
      }));
    const merged = [...backendModels];
    for (const c of customs) {
      if (!merged.find((m) => m.model_id === c.model_id)) merged.push(c);
    }
    return merged;
  })();

  // ---------------------------------------------------------------------------
  // Handlers
  // ---------------------------------------------------------------------------

  const ENGINE_DEFAULT_DEVICES: Record<EmbeddingEngine, string> = {
    SBERT: "auto",
    Candle: "auto",
    Fastembed: "cpu",
  };
  const ENGINE_DEFAULT_MODELS: Record<EmbeddingEngine, EmbedderModel> = {
    SBERT: "intfloat/e5-small-v2",
    Candle: "sentence-transformers/all-MiniLM-L12-v2",
    Fastembed: "AllMiniLML6V2",
  };

  const isForceCpu = (engine: EmbeddingEngine) => {
    const override = settings?.engine_devices?.[engine];
    return (override ?? ENGINE_DEFAULT_DEVICES[engine]) === "cpu";
  };

  const handleDeviceChange = async (engine: EmbeddingEngine, forceCpu: boolean) => {
    if (!settings) return;
    const next = {
      ...settings,
      engine_devices: { ...settings.engine_devices, [engine]: forceCpu ? "cpu" : "auto" },
    };
    replaceSettings(await api.updateSettings({ semantic: next }));
  };

  const supportsCustomModels = (engine: EmbeddingEngine) => engine !== "Fastembed";

  const handleEngineChange = async (engine: EmbeddingEngine) => {
    if (!settings) return;
    setModelFilter("");
    setDraftSelected({
      engine,
      model: ENGINE_DEFAULT_MODELS[engine],
      dimension: 384,
    });
  };

  const handleModelChange = useCallback(
    async (modelId: EmbedderModel) => {
      if (!effectiveSelected) return;
      setDraftSelected({
        ...effectiveSelected,
        model: modelId,
      });

      const descriptor = backendModels.find((m) => m.model_id === modelId);
      if (descriptor && !descriptor.is_cached && descriptor.size_bytes === null) {
        setSizeFetchingFor(modelId);
        try {
          const size = await api.getModelSize(effectiveSelected.engine, modelId);
          dispatch({ type: "model_size_fetched", modelId, sizeBytes: size });
        } catch (e) {
          console.error(`getModelSize(${modelId}) failed:`, e);
        } finally {
          setSizeFetchingFor(null);
        }
      }
    },
    [effectiveSelected, backendModels, api],
  );

  const handleAction = useCallback(async () => {
    dispatch({ type: "clear_error" });
    if (!settings) return;
    console.log("[SemanticPanel] handleAction", {
      phase,
      directory,
      engine: effectiveSelected?.engine ?? null,
      model: effectiveSelected?.model ?? null,
    });

    if (phase === "downloading" || phase === "building") {
      console.log("[SemanticPanel] cancelEmbed -> invoking backend");
      dispatch({ type: "cancel_started" });
      api.cancelEmbed()
        .then(() => {
          console.log("[SemanticPanel] cancelEmbed -> resolved");
          dispatch({ type: "cancel_completed" });
        })
        .catch((e) => {
          dispatch({ type: "cancel_failed" });
          console.error("cancelEmbed failed:", e);
        });
      return;
    }

    if (!effectiveSelected) return;

    if (phase === "not_downloaded") {
      console.log("[SemanticPanel] downloadModel -> invoking backend");
      dispatch({ type: "op_started", op: "downloading" });
      dispatch({
        type: "queue_build",
        build: {
          directory,
          selected: effectiveSelected,
        },
      });
      api.downloadModel(effectiveSelected).catch((e) => {
        console.error("downloadModel failed:", e);
        dispatch({
          type: "op_error",
          message: e?.toString?.() ?? "Failed to start download",
          operation: "Download",
        });
      });
    } else if (phase === "ready" || phase === "engine_mismatch") {
      console.log("[SemanticPanel] buildIndex -> invoking backend");
      dispatch({ type: "op_started", op: "building" });
      api.buildIndex(directory, effectiveSelected).catch((e) => {
        console.error("buildIndex failed:", e);
        dispatch({
          type: "op_error",
          message: e?.toString?.() ?? "Failed to start build",
          operation: "Build",
        });
      });
    } else if (phase === "indexed") {
      api.deleteIndex(directory).then(() => {
        dispatch({ type: "index_deleted" });
        handleCurrentRootIndexRemoved().catch(console.error);
        refreshSemanticReady().catch(console.error);
      }).catch((e) => console.error("deleteIndex failed:", e));
    }
  }, [phase, settings, effectiveSelected, api, directory, handleCurrentRootIndexRemoved, refreshSemanticReady]);

  const handleAddCustomModel = async () => {
    if (!settings || !customModelInput.trim()) return;

    let modelId = customModelInput.trim();
    const hfUrlRegex = /(?:https?:\/\/)?(?:www\.)?(?:huggingface\.co\/|hf\.co\/)([a-zA-Z0-9._-]+)\/([a-zA-Z0-9._-]+)/i;
    const hfIdRegex = /([a-zA-Z0-9._-]+)\/([a-zA-Z0-9._-]+)/;
    const match = modelId.match(hfUrlRegex) || modelId.match(hfIdRegex);

    if (match && match[1] && match[2]) {
      modelId = `${match[1]}/${match[2]}`;
    } else {
      dispatch({ type: "error", error: "Please enter a valid HuggingFace repository ID (e.g., org/model) or URL" });
      return;
    }

    const targetEngine = currentEngine ?? settings.selected.engine;
    if (settings.custom_models.find((m) => m.model_id === modelId && m.engine === targetEngine)) {
      dispatch({ type: "error", error: "Model already added for this engine" });
      return;
    }

    const next = {
      ...settings,
      custom_models: [...(settings.custom_models || []), { engine: targetEngine, model_id: modelId }],
    };

    dispatch({ type: "clear_error" });
    setCustomModelInput("");
    setIsAddingCustom(false);
    setDraftSelected({
      engine: targetEngine,
      model: modelId,
      dimension: effectiveSelected?.dimension ?? settings.selected.dimension,
    });

    replaceSettings(await api.updateSettings({ semantic: next }));
    invalidate();
  };

  const progressPct =
    state.progress && state.progress.total > 0
      ? Math.round((state.progress.current / state.progress.total) * 100)
      : 0;
  const progressLabel = phase === "downloading" ? "Downloading model" : "Indexing";

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  // The corpus of a read-only workspace is built and owned by another
  // application, so the panel reports the index rather than offering to
  // rebuild or delete it. The backend refuses those calls either way.
  if (readOnly) {
    return (
      <div className="flex flex-col gap-2 p-1">
        <p className="text-xs text-[var(--text-muted)]">
          {managedBy
            ? `This workspace's semantic index is managed by ${managedBy}.`
            : "This workspace's semantic index is managed by another application."}
        </p>
        <p className="text-xs text-[var(--text-dim)]">
          Its documents are searchable here, but the index cannot be rebuilt or
          deleted from Wilkes.
        </p>
        {indexStatus && (
          <p className="text-xs text-[var(--text-dim)]">
            {indexStatus.indexed_files} indexed file
            {indexStatus.indexed_files === 1 ? "" : "s"}
            {" · "}
            {indexStatus.total_chunks} chunk{indexStatus.total_chunks === 1 ? "" : "s"}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4 p-1">
      {/* Engine Selection */}
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2 uppercase tracking-wider">Embedding Engine</h3>
        <div className="flex p-0.5 bg-[var(--bg-active)] rounded-lg w-full">
          {ALL_ENGINES.map((e) => {
            const isSupported = supportedEngines.includes(e);
            return (
            <Tooltip key={e} content={!isSupported ? "Feature disabled in this build" : undefined}>
              <button
                type="button"
                disabled={isActive || (!isEngineAvailable && currentEngine === e) || !isSupported}
                onClick={() => handleEngineChange(e)}
                className={`flex-1 px-3 py-1 rounded-md text-xs transition-all ${
                  currentEngine === e
                    ? "bg-[var(--bg-app)] text-[var(--text-main)] shadow-sm"
                    : !isSupported
                      ? "text-[var(--text-muted)]/50 opacity-50 cursor-not-allowed"
                      : "text-[var(--text-muted)] hover:text-[var(--text-main)] disabled:opacity-50"
                }`}
              >
                {e}
              </button>
            </Tooltip>
            );
          })}
        </div>
        <p className="text-[10px] text-[var(--text-dim)] mt-1.5 px-1 selectable">
          {currentEngine === "SBERT"
            ? "Sentence-Transformers via Python. Supports almost any model. Good performance. Uses GPU via MPS (Apple Silicon) if available."
            : currentEngine === "Candle"
              ? "Medium performance. Uses GPU via Metal (Apple Silicon) if available."
              : "For ONNX models. Good performance on CPU."}
        </p>
        {currentEngine === "SBERT" && (
          <div className="mt-1.5 px-2 py-1.5 rounded bg-[var(--bg-active)] flex items-start gap-1.5">
            <span className="text-[10px] text-[var(--text-dim)] shrink-0 mt-px uppercase font-bold tracking-tighter">python runtime</span>
            {state.pythonPath ? (
              <span className="text-[10px] text-[var(--text-main)] font-mono break-all selectable">{state.pythonPath}</span>
            ) : (
              <span className="text-[10px] text-[var(--accent-red,#f87171)] font-mono break-all selectable">
                {state.pythonError ?? "Resolving…"}
              </span>
            )}
          </div>
        )}
      </section>

      {/* Model list */}
      {settings && (
        <ModelCatalog
          title="Embedding Model"
          catalogKey={`embedding:${currentEngine ?? settings.selected.engine}`}
          models={mergedModels}
          filter={modelFilter}
          selectedModelId={effectiveSelected?.model}
          activeModelId={settings.selected.model}
          sizeFetchingFor={sizeFetchingFor}
          disabled={isActive || !isEngineAvailable}
          emptyMessage="No models found for this engine"
          onFilterChange={setModelFilter}
          onSelect={(model) => void handleModelChange(model.model_id)}
          toolbarAction={
            currentEngine && supportsCustomModels(currentEngine) ? (
              <button
                type="button"
                onClick={() => setIsAddingCustom(!isAddingCustom)}
                className={`rounded-lg border px-2 py-1.5 text-[10px] font-medium transition-all ${
                  isAddingCustom
                    ? "border-[var(--accent-blue)] bg-[var(--accent-blue)] text-white"
                    : "border-[var(--border-main)] bg-[var(--bg-active)] text-[var(--text-muted)] hover:text-[var(--text-main)]"
                }`}
              >
                {isAddingCustom ? "Cancel" : "Add Custom"}
              </button>
            ) : undefined
          }
          toolbarContent={
            isAddingCustom ? (
              <div className="animate-in fade-in slide-in-from-top-1 flex flex-col gap-2 rounded-lg border border-[var(--border-main)]/30 bg-[var(--bg-active)] p-2">
                <p className="text-[10px] text-[var(--text-dim)]">
                  Enter HuggingFace Repository ID:
                </p>
                <div className="flex gap-2">
                  <input
                    type="text"
                    placeholder="e.g. org/repo-name"
                    value={customModelInput}
                    onChange={(event) => setCustomModelInput(event.target.value)}
                    onKeyDown={(event) => event.key === "Enter" && handleAddCustomModel()}
                    className="flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-1 text-[11px] text-[var(--text-main)] placeholder-[var(--text-dim)] focus:border-[var(--accent-blue)] focus:outline-none"
                    autoFocus
                  />
                  <button
                    type="button"
                    onClick={handleAddCustomModel}
                    className="rounded bg-[var(--accent-blue)] px-3 py-1 text-[10px] font-semibold text-white hover:bg-[var(--accent-blue-hover)]"
                  >
                    Add
                  </button>
                </div>
              </div>
            ) : undefined
          }
        />
      )}

      {/* Action Area */}
      <section className="bg-[var(--bg-active)]/30 rounded-xl p-3 border border-[var(--border-main)] flex flex-col gap-3">
        {phase === "engine_mismatch" && (
          <div className="bg-amber-900/20 border border-amber-900/50 rounded-lg p-1">
            <p className="text-center text-[10px] leading-relaxed text-[var(--text-muted)]">
              Choosing this model and/or engine will trigger a reindex.
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
          disabled={!isEngineAvailable || state.isCancelling}
          type="button"
          className={`w-full py-2 rounded-lg text-xs font-semibold transition-all shadow-sm active:scale-[0.98] disabled:opacity-50 ${
            isActive
              ? "bg-red-700 hover:bg-red-600 text-white"
              : phase === "indexed"
                ? "bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] border border-[var(--border-main)]"
                : phase === "engine_mismatch"
                  ? "bg-amber-700 hover:bg-amber-600 text-white"
                  : "bg-[var(--accent-blue)] hover:bg-[var(--accent-blue)] text-white"
          }`}
        >
          {phase === "not_downloaded" && "Download model and index files"}
          {phase === "downloading" && (state.isCancelling ? "Cancelling…" : "Cancel download")}
          {phase === "ready" && "Build semantic index"}
          {phase === "engine_mismatch" && "Save model"}
          {phase === "building" && (state.isCancelling ? "Cancelling…" : "Cancel build")}
          {phase === "indexed" && "Delete Index"}
        </button>

        {(isActive || (phase === "indexed" && showLogs) || (phase === "ready" && showLogs) || (phase === "engine_mismatch" && showLogs)) && (
          <div className="flex flex-col gap-3 mt-1 px-1">
            {isActive && (
              <div className="flex flex-col gap-1.5">
                <div className="relative h-5 bg-[var(--bg-app)] rounded-full overflow-hidden border border-[var(--border-main)]/60">
                  <div
                    className="h-full bg-[var(--accent-blue)] transition-all duration-300 ease-out animate-shimmer rounded-full"
                    style={{ width: `${progressPct}%` }}
                  />
                  <div className="absolute inset-0 flex items-center justify-between px-2 pointer-events-none">
                    <span className="text-[9px] font-medium uppercase tracking-[0.14em] text-[var(--text-dim)]">
                      {progressLabel}
                    </span>
                    <span className="rounded-full bg-black/20 px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-white backdrop-blur-sm">
                      {progressPct}%
                    </span>
                  </div>
                </div>
                <div className="flex justify-end text-[10px] text-[var(--text-muted)] min-h-[1rem]">
                  <span className="truncate max-w-[180px]">
                    {phase === "building" && state.progress && "message" in state.progress ? (state.progress as any).message : ""}
                  </span>
                </div>
              </div>
            )}

            {showLogs && (
              <div className="h-48 border border-[var(--border-main)] rounded-lg overflow-hidden bg-[var(--bg-input)] p-2">
                <LogsPanel api={api} />
              </div>
            )}
          </div>
        )}
      </section>

      {/* Advanced Section */}
      <section className="mt-2 border-t border-[var(--border-main)]/50 pt-4 p-1">
        <button
          type="button"
          onClick={() => setShowAdvanced(!showAdvanced)}
          className="flex items-center gap-2 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider hover:text-[var(--text-muted)] transition-colors w-full text-left"
        >
          <span className="w-2 mr-2">{showAdvanced ? <CornerRightUp /> : <CornerLeftDown />}</span>
          Advanced
        </button>
        {showAdvanced && settings && (
          <div className="mt-3 px-1 animate-in fade-in slide-in-from-top-1">
            <label className="flex items-center gap-2.5 cursor-pointer group">
              <input
                type="checkbox"
                checked={isForceCpu(currentEngine ?? settings.selected.engine)}
                disabled={isActive}
                onChange={(e) => handleDeviceChange(currentEngine ?? settings.selected.engine, e.target.checked)}
                className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)] disabled:opacity-50"
              />
              <span className="text-xs text-[var(--text-main)] group-hover:text-[var(--text-main)] transition-colors">Disable hardware acceleration</span>
            </label>
            <p className="text-[10px] text-[var(--text-dim)] mt-1.5 ml-6 leading-relaxed">Recommended for Fastembed on macOS</p>
          </div>
        )}
      </section>

      {state.error && (
        <div className="p-3 bg-red-900/20 border border-red-900/50 rounded-lg">
          <p className="text-xs text-red-400 leading-relaxed">{state.error}</p>
        </div>
      )}
    </div>
  );
}
