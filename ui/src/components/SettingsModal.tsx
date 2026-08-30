import { useState, useEffect, useRef } from "react";
import type { SearchApi } from "../services/api";
import type { ExternalMcpStatus, HttpApiStatus, Settings } from "../lib/types";
import SemanticPanel from "./SemanticPanel";
import GenerationPanel from "./GenerationPanel";
import ImageAnalysisPanel from "./ImageAnalysisPanel";
import { useGenerationStore } from "../stores/useGenerationStore";
import ChunkingPanel from "./ChunkingPanel";
import DataPanel from "./DataPanel";
import ExtensionsPanel from "./ExtensionsPanel";
import IntegrationsPanel from "./IntegrationsPanel";
import LogsPanel from "./LogsPanel";
import WorkersPanel from "./WorkersPanel";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { json } from "@codemirror/lang-json";
import { oneDark } from "@codemirror/theme-one-dark";
import { indentWithTab } from "@codemirror/commands";
import {Tool} from "react-feather";
import { isTauri } from "../services";
import type { AgentBackend, MetadataSourcePreference } from "../lib/types";
import { useSettingsStore } from "../stores/useSettingsStore";

const CHAT_BACKENDS: { value: AgentBackend; label: string }[] = [
  { value: "ClaudeCode", label: "Claude Code" },
  { value: "Codex", label: "Codex" },
  { value: "Nanocoder", label: "Nanocoder" },
];

const METADATA_SOURCES: { value: MetadataSourcePreference; label: string }[] = [
  { value: "file", label: "File" },
  { value: "zotero", label: "Zotero" },
  { value: "semantic_scholar", label: "Semantic Scholar" },
  { value: "openalex", label: "OpenAlex" },
];

interface SettingsModalProps {
  api: SearchApi;
  isOpen: boolean;
  onClose: () => void;
  directory: string;
  refreshSemanticReady: () => Promise<boolean>;
  onSettingsUpdate?: (patch: Partial<Settings>) => void;
}

function TechnicalSettings({ api, onUpdate }: { api: SearchApi; onUpdate: (s: Settings) => void }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const isDark = useSettingsStore((state) => state.colorScheme) === "dark";

  useEffect(() => {
    let mounted = true;
    
    const init = async () => {
      try {
        const s = await api.getSettings();
        if (!mounted) return;
        
        // Clean up previous view
        if (viewRef.current) {
          viewRef.current.destroy();
          viewRef.current = null;
        }

        if (!containerRef.current) return;

        const content = JSON.stringify(s, null, 2);
        const extensions = [
          basicSetup,
          json(),
          keymap.of([indentWithTab]),
          EditorView.lineWrapping,
          EditorView.theme({
            "&": { height: "100%", fontSize: "12px" },
            ".cm-scroller": { overflow: "auto" }
          })
        ];
        if (isDark) extensions.push(oneDark);

        const state = EditorState.create({
          doc: content,
          extensions
        });

        const view = new EditorView({
          state,
          parent: containerRef.current
        });
        viewRef.current = view;
        setLoading(false);
      } catch (e: any) {
        if (mounted) {
          setError(`Failed to load settings: ${e.toString()}`);
          setLoading(false);
        }
      }
    };

    init();

    return () => {
      mounted = false;
      if (viewRef.current) {
        viewRef.current.destroy();
        viewRef.current = null;
      }
    };
  }, [api, isDark]);

  const handleSave = async () => {
    if (!viewRef.current) return;
    const content = viewRef.current.state.doc.toString();
    try {
      const parsed = JSON.parse(content);
      const updated = await api.updateSettings(parsed);
      onUpdate(updated);
      setError(null);
      // Brief visual feedback could go here
    } catch (e: any) {
      setError(e.toString());
    }
  };

  return (
    <div className="flex flex-col h-full gap-3">
      <div className="flex items-center justify-between">
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">Direct JSON Editor</h3>
        <button
          onClick={handleSave}
          disabled={loading}
          className="px-3 py-1 bg-[var(--accent-blue)] hover:bg-[var(--accent-blue-hover)] text-white text-[10px] font-bold uppercase tracking-wider rounded transition-colors disabled:opacity-50"
        >
          Apply Changes
        </button>
      </div>
      <div className="flex-1 border border-[var(--border-main)] rounded-lg overflow-hidden bg-[var(--bg-active)]/20 relative min-h-[300px]">
        {loading && (
          <div className="absolute inset-0 flex items-center justify-center bg-[var(--bg-app)]/50 z-10">
            <div className="w-5 h-5 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
          </div>
        )}
        <div ref={containerRef} className="absolute inset-0" />
      </div>
      {error && (
        <div className="p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 font-mono break-all whitespace-pre-wrap">
          {error}
        </div>
      )}
    </div>
  );
}

export default function SettingsModal({
  api,
  isOpen,
  onClose,
  directory,
  refreshSemanticReady,
  onSettingsUpdate,
}: SettingsModalProps) {
  type SettingsTab =
    | "general"
    | "extensions"
    | "integrations"
    | "semantic-models"
    | "semantic-chunking"
    | "generation-chat"
    | "generation-models"
    | "servers"
    | "extraction-images"
    | "data"
    | "workers"
    | "logs"
    | "technical";

  const [activeTab, setActiveTab] = useState<SettingsTab>("general");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [customInstructionsDraft, setCustomInstructionsDraft] = useState("");
  const [externalMcpStatus, setExternalMcpStatus] = useState<ExternalMcpStatus | null>(null);
  const [externalMcpRequireToken, setExternalMcpRequireToken] = useState(false);
  const [externalMcpBindAddress, setExternalMcpBindAddress] = useState("127.0.0.1");
  const [externalMcpPort, setExternalMcpPort] = useState(39217);
  const [externalMcpBusy, setExternalMcpBusy] = useState(false);
  const [externalMcpError, setExternalMcpError] = useState<string | null>(null);
  const [httpApiStatus, setHttpApiStatus] = useState<HttpApiStatus | null>(null);
  const [httpApiBindAddress, setHttpApiBindAddress] = useState("127.0.0.1");
  const [httpApiPort, setHttpApiPort] = useState(2020);
  const [httpApiBusy, setHttpApiBusy] = useState(false);
  const [httpApiError, setHttpApiError] = useState<string | null>(null);
  const customInstructionsSaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const generationReady = useGenerationStore((state) => state.ready);

  useEffect(() => {
    if (isOpen) {
      api.getSettings().then((nextSettings) => {
        setSettings(nextSettings);
        setCustomInstructionsDraft(nextSettings.chat_custom_instructions ?? "");
        setExternalMcpRequireToken(nextSettings.external_mcp?.require_token ?? false);
        setExternalMcpBindAddress(nextSettings.external_mcp?.bind_address ?? "127.0.0.1");
        setExternalMcpPort(nextSettings.external_mcp?.port ?? 39217);
        setHttpApiBindAddress(nextSettings.http_api?.bind_address ?? "127.0.0.1");
        setHttpApiPort(nextSettings.http_api?.port ?? 2020);
      }).catch(console.error);
      if (api.getExternalMcpStatus) {
        api.getExternalMcpStatus()
          .then((status) => {
            setExternalMcpStatus(status);
            setExternalMcpRequireToken(status.require_token);
            setExternalMcpBindAddress(status.bind_address);
            setExternalMcpPort(status.port);
            setExternalMcpError(status.error);
          })
          .catch((error) => setExternalMcpError(error.toString()));
      }
      if (api.getHttpApiStatus) {
        api.getHttpApiStatus()
          .then((status) => {
            setHttpApiStatus(status);
            setHttpApiBindAddress(status.bind_address);
            setHttpApiPort(status.port);
            setHttpApiError(status.error);
          })
          .catch((error) => setHttpApiError(error.toString()));
      }
    }
  }, [isOpen, api]);

  useEffect(() => () => {
    if (customInstructionsSaveTimer.current) {
      clearTimeout(customInstructionsSaveTimer.current);
    }
  }, []);

  const handleUpdateSettings = async (patch: Partial<Settings>) => {
    try {
      const newSettings = await api.updateSettings(patch);
      setSettings(newSettings);
      if (onSettingsUpdate) onSettingsUpdate(patch);
      // Attaching the generation model is asynchronous, so readiness has to be
      // re-read from the backend rather than inferred from the flag the user
      // just flipped. Done here because this is the one place arbitrary
      // settings patches are applied from the UI, including the JSON editor.
      if (patch.generation) {
        void useGenerationStore.getState().refreshReady();
      }
    } catch (e) {
      console.error("Failed to update settings:", e);
      throw e;
    }
  };

  const persistCustomInstructions = (value: string) => {
    if (customInstructionsSaveTimer.current) {
      clearTimeout(customInstructionsSaveTimer.current);
    }
    customInstructionsSaveTimer.current = null;
    void handleUpdateSettings({ chat_custom_instructions: value });
  };

  const handleCustomInstructionsChange = (value: string) => {
    setCustomInstructionsDraft(value);
    if (customInstructionsSaveTimer.current) {
      clearTimeout(customInstructionsSaveTimer.current);
    }
    customInstructionsSaveTimer.current = setTimeout(() => {
      customInstructionsSaveTimer.current = null;
      void handleUpdateSettings({ chat_custom_instructions: value });
    }, 300);
  };

  const configureExternalMcp = async (
    enabled: boolean,
    bindAddress = externalMcpBindAddress,
    port = externalMcpPort,
    requireToken = externalMcpRequireToken,
  ) => {
    if (!api.configureExternalMcp) return;
    setExternalMcpBusy(true);
    setExternalMcpError(null);
    try {
      const status = await api.configureExternalMcp(
        enabled,
        requireToken,
        bindAddress.trim(),
        port,
      );
      setExternalMcpStatus(status);
      setExternalMcpRequireToken(status.require_token);
      setExternalMcpBindAddress(status.bind_address);
      setExternalMcpPort(status.port);
      setSettings((current) => current ? {
        ...current,
        external_mcp: {
          enabled: status.enabled,
          require_token: status.require_token,
          bind_address: status.bind_address,
          port: status.port,
        },
      } : current);
      onSettingsUpdate?.({
        external_mcp: {
          enabled: status.enabled,
          require_token: status.require_token,
          bind_address: status.bind_address,
          port: status.port,
        },
      });
      setExternalMcpError(status.error);
    } catch (error: any) {
      setExternalMcpError(error.toString());
    } finally {
      setExternalMcpBusy(false);
    }
  };

  const configureHttpApi = async (
    enabled: boolean,
    bindAddress = httpApiBindAddress,
    port = httpApiPort,
  ) => {
    if (!api.configureHttpApi) return;
    setHttpApiBusy(true);
    setHttpApiError(null);
    try {
      const status = await api.configureHttpApi(enabled, bindAddress.trim(), port);
      setHttpApiStatus(status);
      setHttpApiBindAddress(status.bind_address);
      setHttpApiPort(status.port);
      const http_api = {
        enabled: status.enabled,
        bind_address: status.bind_address,
        port: status.port,
      };
      setSettings((current) => current ? { ...current, http_api } : current);
      onSettingsUpdate?.({ http_api });
      setHttpApiError(status.error);
    } catch (error: any) {
      setHttpApiError(error.toString());
    } finally {
      setHttpApiBusy(false);
    }
  };

  const rotateExternalMcpToken = async () => {
    if (!api.rotateExternalMcpToken) return;
    setExternalMcpBusy(true);
    setExternalMcpError(null);
    try {
      const status = await api.rotateExternalMcpToken();
      setExternalMcpStatus(status);
      setExternalMcpError(status.error);
    } catch (error: any) {
      setExternalMcpError(error.toString());
    } finally {
      setExternalMcpBusy(false);
    }
  };

  const copyExternalMcpText = async (text: string) => {
    try {
      await api.writeClipboard(text);
    } catch (error: any) {
      setExternalMcpError(`Could not copy to clipboard: ${error.toString()}`);
    }
  };

  const TabButton = ({
    id,
    label,
    accessibleLabel,
    indent = false,
  }: {
    id: SettingsTab;
    label: string;
    accessibleLabel?: string;
    indent?: boolean;
  }) => (
    <button
      aria-label={accessibleLabel}
      onClick={() => setActiveTab(id)}
      className={`px-3 py-1.5 rounded-lg text-sm text-left transition-colors ${
        indent ? "ml-2" : ""
      } ${
        activeTab === id
          ? "bg-[var(--bg-active)] text-[var(--text-main)] font-medium shadow-sm"
          : "text-[var(--text-muted)] hover:bg-[var(--bg-active)]/50 hover:text-[var(--text-main)]"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div className={`fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4${isOpen ? "" : " hidden"}`}>
      <div className="bg-[var(--bg-app)] border border-[var(--border-main)] rounded-xl shadow-2xl w-full max-w-2xl h-[800px] max-h-[90vh] flex flex-col overflow-hidden animate-in fade-in zoom-in duration-200">
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-[var(--border-main)]">
          <h2 className="text-base font-semibold text-[var(--text-main)] flex items-center gap-2">
            <span className="text-[var(--text-dim)] text-lg"><Tool /></span> Settings
          </h2>
          <button
            onClick={onClose}
            className="text-[var(--text-dim)] hover:text-[var(--text-main)] transition-colors p-1"
          >
            ✕
          </button>
        </div>

        <div className="flex flex-1 overflow-hidden">
          {/* Sidebar */}
          <div className="w-40 border-r border-[var(--border-main)] bg-[var(--bg-sidebar)] p-2 flex flex-col gap-3">
            <div className="flex flex-col gap-0.5">
              <TabButton id="general" label="General" />
              <TabButton id="extensions" label="File extensions" />
              <TabButton id="integrations" label="Integrations" />
              {isTauri && (
                <TabButton
                  id="servers"
                  label="Servers"
                  accessibleLabel="MCP and HTTP API servers"
                />
              )}
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Semantic Search</span>
              <TabButton
                id="semantic-models"
                label="Models"
                accessibleLabel="Semantic Search Models"
                indent
              />
              <TabButton id="semantic-chunking" label="Chunking" indent />
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Generation</span>
              {isTauri && <TabButton id="generation-chat" label="Chat" indent />}
              <TabButton
                id="generation-models"
                label="Models"
                accessibleLabel="Generation Models"
                indent
              />
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Extraction</span>
              <TabButton
                id="extraction-images"
                label="Images"
                accessibleLabel="Image Analysis"
                indent
              />
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Advanced</span>
              <TabButton id="data" label="Data" indent />
              <TabButton id="workers" label="Workers" indent />
              <TabButton id="logs" label="Logs" indent />
              <TabButton id="technical" label="Settings (JSON)" indent />
            </div>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-4 bg-[var(--bg-app)] relative">
            <div className={activeTab === "general" ? "block h-full" : "hidden"}>
              {settings && (
                <div className="space-y-4">
                  <section>
                    <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">Search Preferences</h3>
                    <div className="space-y-3">
                      <label className="flex items-center gap-2.5 cursor-pointer group">
                        <input
                          type="checkbox"
                          checked={settings.respect_gitignore}
                          onChange={(e) => handleUpdateSettings({ respect_gitignore: e.target.checked })}
                          className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
                        />
                        <span className="text-xs text-[var(--text-main)] group-hover:text-[var(--text-main)] transition-colors">Respect .gitignore files</span>
                      </label>

                      <label className="flex items-start gap-2.5 cursor-pointer group">
                        <input
                          type="checkbox"
                          checked={settings.grep_use_index}
                          onChange={(e) => handleUpdateSettings({ grep_use_index: e.target.checked })}
                          className="mt-0.5 w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
                        />
                        <span className="flex flex-col">
                          <span className="text-xs text-[var(--text-main)] group-hover:text-[var(--text-main)] transition-colors">Use index for exact search</span>
                          <span className="text-[10px] text-[var(--text-dim)] italic">Reads PDF text from the semantic index instead of re-extracting; falls back to reading files directly when a file isn't indexed</span>
                        </span>
                      </label>

                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Max file size (MB)</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Skip larger files</p>
                        </div>
                        <input
                          type="number"
                          value={Math.round(settings.max_file_size / (1024 * 1024))}
                          onChange={(e) => handleUpdateSettings({ max_file_size: parseInt(e.target.value) * 1024 * 1024 })}
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        />
                      </div>

                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Max results</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">0 = unlimited</p>
                        </div>
                        <input
                          type="number"
                          min={0}
                          value={settings.max_results}
                          onChange={(e) => handleUpdateSettings({ max_results: parseInt(e.target.value) || 0 })}
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        />
                      </div>

                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Primary metadata source</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Displayed first</p>
                        </div>
                        <select
                          value={settings.primary_metadata_source ?? "zotero"}
                          onChange={(e) =>
                            handleUpdateSettings({
                              primary_metadata_source: e.target.value as MetadataSourcePreference,
                            })
                          }
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        >
                          {METADATA_SOURCES.map((source) => (
                            <option key={source.value} value={source.value}>
                              {source.label}
                            </option>
                          ))}
                        </select>
                      </div>
                    </div>
                  </section>

                  {(() => {
                    const retrieval = settings.retrieval ?? {
                      hyde: { enabled: false, hypotheticals: 1, include_query: true },
                      pseudo_relevance_feedback: { enabled: false, feedback_docs: 5, alpha: 1, beta: 0.5 },
                    };
                    const hyde = retrieval.hyde;
                    const prf = retrieval.pseudo_relevance_feedback;
                    const canToggleHyde = generationReady || hyde.enabled;
                    const updateRetrieval = (patch: Partial<typeof retrieval>) =>
                      handleUpdateSettings({ retrieval: { ...retrieval, ...patch } });
                    const numberBox =
                      "w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors";

                    return (
                      <section>
                        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-1 uppercase tracking-wider">Query enhancement</h3>
                        <p className="text-[10px] text-[var(--text-dim)] italic mb-2.5">
                          Reshapes the query vector before semantic search. No effect on exact (grep) search.
                        </p>
                        <div className="space-y-4">
                          {/* HyDE */}
                          <div className="space-y-2">
                            <label
                              className={`flex items-center gap-2.5 group ${canToggleHyde ? "cursor-pointer" : "cursor-not-allowed opacity-60"}`}
                            >
                              <input
                                type="checkbox"
                                checked={hyde.enabled}
                                disabled={!canToggleHyde}
                                onChange={(e) => updateRetrieval({ hyde: { ...hyde, enabled: e.target.checked } })}
                                className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
                              />
                              <span className="text-xs text-[var(--text-main)]">
                                HyDE (hypothetical document embeddings)
                              </span>
                            </label>
                            <p className="text-[10px] text-[var(--text-dim)] italic pl-6">
                              {generationReady
                                ? "Searches with the embedding of an LLM-generated answer, adding generation latency to each search."
                                : "Requires a generation model — enable and download one under Generation → Models."}
                            </p>
                            {hyde.enabled && generationReady && (
                              <div className="pl-6 space-y-2">
                                <div className="space-y-1">
                                  <div className="flex justify-between items-baseline">
                                    <label className="text-xs text-[var(--text-muted)]">Hypothetical passages</label>
                                    <p className="text-[10px] text-[var(--text-dim)] italic">More = broader, slower</p>
                                  </div>
                                  <input
                                    type="number"
                                    min={1}
                                    max={5}
                                    value={hyde.hypotheticals}
                                    onChange={(e) =>
                                      updateRetrieval({
                                        hyde: { ...hyde, hypotheticals: Math.max(1, parseInt(e.target.value) || 1) },
                                      })
                                    }
                                    className={numberBox}
                                  />
                                </div>
                                <label className="flex items-center gap-2.5 cursor-pointer">
                                  <input
                                    type="checkbox"
                                    checked={hyde.include_query}
                                    onChange={(e) => updateRetrieval({ hyde: { ...hyde, include_query: e.target.checked } })}
                                    className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
                                  />
                                  <span className="text-xs text-[var(--text-muted)]">Blend with the original query vector</span>
                                </label>
                              </div>
                            )}
                          </div>

                          {/* Pseudo-relevance feedback */}
                          <div className="space-y-2">
                            <label className="flex items-center gap-2.5 cursor-pointer group">
                              <input
                                type="checkbox"
                                checked={prf.enabled}
                                onChange={(e) =>
                                  updateRetrieval({ pseudo_relevance_feedback: { ...prf, enabled: e.target.checked } })
                                }
                                className="w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]"
                              />
                              <span className="text-xs text-[var(--text-main)]">Pseudo-relevance feedback (Rocchio)</span>
                            </label>
                            <p className="text-[10px] text-[var(--text-dim)] italic pl-6">
                              Folds the top initial hits back into the query and searches again. No generation model needed.
                            </p>
                            {prf.enabled && (
                              <div className="pl-6 grid grid-cols-3 gap-2">
                                <div className="space-y-1">
                                  <label className="text-[10px] text-[var(--text-muted)]">Feedback docs</label>
                                  <input
                                    type="number"
                                    min={1}
                                    value={prf.feedback_docs}
                                    onChange={(e) =>
                                      updateRetrieval({
                                        pseudo_relevance_feedback: {
                                          ...prf,
                                          feedback_docs: Math.max(1, parseInt(e.target.value) || 1),
                                        },
                                      })
                                    }
                                    className={numberBox}
                                  />
                                </div>
                                <div className="space-y-1">
                                  <label className="text-[10px] text-[var(--text-muted)]">α (query)</label>
                                  <input
                                    type="number"
                                    min={0}
                                    step={0.1}
                                    value={prf.alpha}
                                    onChange={(e) =>
                                      updateRetrieval({
                                        pseudo_relevance_feedback: { ...prf, alpha: Math.max(0, Number(e.target.value)) },
                                      })
                                    }
                                    className={numberBox}
                                  />
                                </div>
                                <div className="space-y-1">
                                  <label className="text-[10px] text-[var(--text-muted)]">β (feedback)</label>
                                  <input
                                    type="number"
                                    min={0}
                                    step={0.1}
                                    value={prf.beta}
                                    onChange={(e) =>
                                      updateRetrieval({
                                        pseudo_relevance_feedback: { ...prf, beta: Math.max(0, Number(e.target.value)) },
                                      })
                                    }
                                    className={numberBox}
                                  />
                                </div>
                              </div>
                            )}
                          </div>
                        </div>
                      </section>
                    );
                  })()}

                  <section>
                    <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2 uppercase tracking-wider">Appearance</h3>
                    <div className="space-y-3">
                      <div className="flex p-0.5 bg-[var(--bg-active)] rounded-lg w-fit">
                        {(["System", "Light", "Dark"] as const).map((t) => (
                          <button
                            key={t}
                            type="button"
                            onClick={() => handleUpdateSettings({ theme: t })}
                            className={`px-3 py-1 rounded-md text-xs transition-all ${
                              settings.theme === t
                                ? "bg-[var(--bg-app)] text-[var(--text-main)] shadow-sm"
                                : "text-[var(--text-dim)] hover:text-[var(--text-muted)]"
                            }`}
                          >
                            {t}
                          </button>
                        ))}
                      </div>
                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label htmlFor="pdf-auto-zoom-target" className="text-xs text-[var(--text-muted)]">
                            PDF auto-zoom target (px)
                          </label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Body-text height</p>
                        </div>
                        <input
                          id="pdf-auto-zoom-target"
                          type="number"
                          min={1}
                          step={0.5}
                          value={settings.pdf_auto_zoom_target_px}
                          onChange={(e) => {
                            const value = Number(e.target.value);
                            if (value > 0) {
                              handleUpdateSettings({ pdf_auto_zoom_target_px: value });
                            }
                          }}
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        />
                      </div>
                    </div>
                  </section>
                </div>
              )}
            </div>

            <div className={activeTab === "generation-chat" ? "block h-full" : "hidden"}>
              {settings && isTauri && (
                <div className="space-y-4">
                  <section>
                    <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2 uppercase tracking-wider">Chat</h3>
                    <div className="space-y-3">
                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Default chat agent</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Ask the documents pane</p>
                        </div>
                        <select
                          value={settings.chat_backend ?? "ClaudeCode"}
                          onChange={(e) =>
                            handleUpdateSettings({ chat_backend: e.target.value as AgentBackend })
                          }
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        >
                          {CHAT_BACKENDS.map((b) => (
                            <option key={b.value} value={b.value}>
                              {b.label}
                            </option>
                          ))}
                        </select>
                      </div>
                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label htmlFor="chat-custom-instructions" className="text-xs text-[var(--text-muted)]">
                            Custom instructions
                          </label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Applied to every chat turn</p>
                        </div>
                        <textarea
                          id="chat-custom-instructions"
                          value={customInstructionsDraft}
                          onChange={(e) => handleCustomInstructionsChange(e.target.value)}
                          onBlur={(e) => persistCustomInstructions(e.target.value)}
                          placeholder="e.g. Keep answers concise and include page references."
                          rows={5}
                          className="w-full resize-y bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        />
                      </div>
                    </div>
                  </section>
                </div>
              )}
            </div>

            <div className={activeTab === "extensions" ? "block h-full" : "hidden"}>
              {settings && (
                <ExtensionsPanel settings={settings} onUpdate={handleUpdateSettings} />
              )}
            </div>

            <div className={activeTab === "integrations" ? "block h-full" : "hidden"}>
              {settings && (
                <IntegrationsPanel api={api} settings={settings} onUpdate={handleUpdateSettings} />
              )}
            </div>

            <div className={activeTab === "semantic-models" ? "block h-full" : "hidden"}>
              <SemanticPanel
                api={api}
                directory={directory}
                refreshSemanticReady={refreshSemanticReady}
              />
            </div>

            <div className={activeTab === "generation-models" ? "block h-full" : "hidden"}>
              {settings && (
                <GenerationPanel
                  api={api}
                  settings={settings}
                  onUpdateSettings={handleUpdateSettings}
                />
              )}
            </div>

            {/* Both listeners serve Wilkes to other programs; neither is a
                property of the chat agent they used to be filed under. */}
            <div className={activeTab === "servers" ? "block h-full" : "hidden"}>
              {settings && isTauri && (
                <div className="space-y-4">
                  <section>
                    <div className="flex items-center justify-between mb-2">
                      <div>
                        <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">
                          External MCP
                        </h3>
                        <p className="text-[10px] text-[var(--text-dim)] mt-1">
                          Available to Claude Code and Codex sessions while Wilkes is running.
                        </p>
                      </div>
                      <span className={`text-[10px] font-medium ${
                        externalMcpStatus?.running ? "text-emerald-500" : "text-[var(--text-dim)]"
                      }`}>
                        {externalMcpStatus?.running ? "Listening" : "Stopped"}
                      </span>
                    </div>

                    <div className="space-y-3">
                      <label className="flex items-center gap-2.5 cursor-pointer">
                        <input
                          type="checkbox"
                          aria-label="Serve MCP for external clients"
                          checked={externalMcpStatus?.enabled ?? settings.external_mcp?.enabled ?? false}
                          disabled={externalMcpBusy}
                          onChange={(event) => void configureExternalMcp(event.target.checked)}
                          className="w-3.5 h-3.5 accent-[var(--accent-blue)]"
                        />
                        <span className="text-xs text-[var(--text-muted)]">
                          Serve MCP for external clients
                        </span>
                      </label>

                      <label className="flex items-center gap-2.5 cursor-pointer">
                        <input
                          type="checkbox"
                          aria-label="Require bearer token"
                          checked={externalMcpRequireToken}
                          disabled={externalMcpBusy}
                          onChange={(event) => void configureExternalMcp(
                            externalMcpStatus?.enabled ?? settings.external_mcp?.enabled ?? false,
                            externalMcpBindAddress,
                            externalMcpPort,
                            event.target.checked,
                          )}
                          className="w-3.5 h-3.5 accent-[var(--accent-blue)]"
                        />
                        <span className="text-xs text-[var(--text-muted)]">
                          Require bearer token
                        </span>
                      </label>

                      <div className="grid grid-cols-[minmax(0,2fr)_minmax(6rem,1fr)_auto] items-end gap-2">
                        <label className="space-y-1">
                          <span className="text-xs text-[var(--text-muted)]">Bind address</span>
                          <input
                            aria-label="External MCP bind address"
                            type="text"
                            value={externalMcpBindAddress}
                            disabled={externalMcpBusy}
                            spellCheck={false}
                            onChange={(event) => setExternalMcpBindAddress(event.target.value)}
                            placeholder="127.0.0.1"
                            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs font-mono text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)]"
                          />
                        </label>
                        <label className="flex-1 space-y-1">
                          <span className="text-xs text-[var(--text-muted)]">Port</span>
                          <input
                            aria-label="External MCP port"
                            type="number"
                            min={1}
                            max={65535}
                            value={externalMcpPort}
                            disabled={externalMcpBusy}
                            onChange={(event) => setExternalMcpPort(Number(event.target.value))}
                            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)]"
                          />
                        </label>
                        <button
                          type="button"
                          disabled={
                            externalMcpBusy
                            || externalMcpBindAddress.trim().length === 0
                            || externalMcpPort < 1
                            || externalMcpPort > 65535
                          }
                          onClick={() => void configureExternalMcp(
                            externalMcpStatus?.enabled ?? settings.external_mcp?.enabled ?? false,
                            externalMcpBindAddress,
                            externalMcpPort,
                            externalMcpRequireToken,
                          )}
                          className="px-3 py-1.5 text-xs rounded border border-[var(--border-main)] bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
                        >
                          Apply
                        </button>
                      </div>

                      {externalMcpBindAddress.trim() !== "::1" && !externalMcpBindAddress.trim().startsWith("127.") && (
                        <div role="status" className="p-2 bg-amber-900/20 border border-amber-800/50 rounded text-[10px] text-amber-300">
                          {externalMcpRequireToken
                            ? "A non-loopback address exposes Wilkes MCP to the network. Keep the bearer token private and use host firewall rules where appropriate."
                            : "A non-loopback address exposes Wilkes MCP without authentication. Anyone who can reach this address can use its tools; use host firewall rules to restrict access."}
                          {(externalMcpBindAddress.trim() === "0.0.0.0" || externalMcpBindAddress.trim() === "::") && (
                            <> Wildcard addresses listen on all interfaces; remote clients must replace the wildcard in the copied endpoint with this machine&apos;s reachable address.</>
                          )}
                        </div>
                      )}

                      {externalMcpStatus?.running && externalMcpStatus.url && (
                        <div className="space-y-2 rounded border border-[var(--border-main)] bg-[var(--bg-active)]/30 p-2.5">
                          <div>
                            <p className="text-[10px] text-[var(--text-dim)] mb-1">Endpoint</p>
                            <button
                              type="button"
                              onClick={() => void copyExternalMcpText(externalMcpStatus.url!)}
                              className="w-full text-left text-[10px] font-mono break-all text-[var(--text-main)] hover:text-[var(--accent-blue)]"
                              title="Copy endpoint"
                            >
                              {externalMcpStatus.url}
                            </button>
                          </div>
                          <div className="flex flex-wrap gap-2">
                            {externalMcpStatus.require_token && externalMcpStatus.token && (
                              <button
                                type="button"
                                onClick={() => void copyExternalMcpText(externalMcpStatus.token!)}
                                className="px-2.5 py-1 text-[10px] rounded border border-[var(--border-main)] hover:bg-[var(--bg-hover)]"
                              >
                                Copy token
                              </button>
                            )}
                            <button
                              type="button"
                              onClick={() => void copyExternalMcpText(
                                externalMcpStatus.require_token && externalMcpStatus.token
                                  ? `claude mcp add --transport http --scope user --header "Authorization: Bearer ${externalMcpStatus.token}" wilkes ${externalMcpStatus.url}`
                                  : `claude mcp add --transport http --scope user wilkes ${externalMcpStatus.url}`,
                              )}
                              className="px-2.5 py-1 text-[10px] rounded border border-[var(--border-main)] hover:bg-[var(--bg-hover)]"
                            >
                              Copy Claude setup
                            </button>
                            <button
                              type="button"
                              onClick={() => void copyExternalMcpText(
                                externalMcpStatus.require_token && externalMcpStatus.token
                                  ? `export WILKES_MCP_TOKEN='${externalMcpStatus.token}'\ncodex mcp add --url ${externalMcpStatus.url} --bearer-token-env-var WILKES_MCP_TOKEN wilkes`
                                  : `codex mcp add --url ${externalMcpStatus.url} wilkes`,
                              )}
                              className="px-2.5 py-1 text-[10px] rounded border border-[var(--border-main)] hover:bg-[var(--bg-hover)]"
                            >
                              Copy Codex setup
                            </button>
                            {externalMcpStatus.require_token && externalMcpStatus.token && (
                              <button
                                type="button"
                                disabled={externalMcpBusy}
                                onClick={() => void rotateExternalMcpToken()}
                                className="px-2.5 py-1 text-[10px] rounded border border-red-900/50 text-red-400 hover:bg-red-900/20 disabled:opacity-50"
                              >
                                Rotate token
                              </button>
                            )}
                          </div>
                          <p className="text-[10px] text-[var(--text-dim)]">
                            {externalMcpStatus.require_token
                              ? "Codex needs WILKES_MCP_TOKEN in the environment of every session. Rotating the token disconnects existing clients."
                              : "No bearer token is required. Enable token authentication before exposing this endpoint to an untrusted network."}
                          </p>
                        </div>
                      )}

                      {externalMcpError && (
                        <div role="alert" className="p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 break-all">
                          {externalMcpError}
                        </div>
                      )}
                    </div>
                  </section>

                  <section className="border-t border-[var(--border-main)] pt-4">
                    <div className="flex items-center justify-between mb-2">
                      <div>
                        <h3 className="text-[10px] font-medium text-[var(--text-dim)] uppercase tracking-wider">
                          HTTP API
                        </h3>
                        <p className="text-[10px] text-[var(--text-dim)] mt-1">
                          The same API <code>wilkes-server</code> serves, over the library this
                          window has open. Lets another program read it without opening the
                          workspace itself — two processes on one workspace overwrite each
                          other&apos;s settings and index.
                        </p>
                      </div>
                      <span className={`text-[10px] font-medium ${
                        httpApiStatus?.running ? "text-emerald-500" : "text-[var(--text-dim)]"
                      }`}>
                        {httpApiStatus?.running ? "Listening" : "Stopped"}
                      </span>
                    </div>

                    <div className="space-y-3">
                      <label className="flex items-center gap-2.5 cursor-pointer">
                        <input
                          type="checkbox"
                          aria-label="Serve the HTTP API"
                          checked={httpApiStatus?.enabled ?? settings.http_api?.enabled ?? false}
                          disabled={httpApiBusy}
                          onChange={(event) => void configureHttpApi(event.target.checked)}
                          className="w-3.5 h-3.5 accent-[var(--accent-blue)]"
                        />
                        <span className="text-xs text-[var(--text-muted)]">
                          Serve the HTTP API
                        </span>
                      </label>

                      <div className="grid grid-cols-[minmax(0,2fr)_minmax(6rem,1fr)_auto] items-end gap-2">
                        <label className="space-y-1">
                          <span className="text-xs text-[var(--text-muted)]">Bind address</span>
                          <input
                            aria-label="HTTP API bind address"
                            type="text"
                            value={httpApiBindAddress}
                            disabled={httpApiBusy}
                            spellCheck={false}
                            onChange={(event) => setHttpApiBindAddress(event.target.value)}
                            placeholder="127.0.0.1"
                            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs font-mono text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)]"
                          />
                        </label>
                        <label className="flex-1 space-y-1">
                          <span className="text-xs text-[var(--text-muted)]">Port</span>
                          <input
                            aria-label="HTTP API port"
                            type="number"
                            min={1}
                            max={65535}
                            value={httpApiPort}
                            disabled={httpApiBusy}
                            onChange={(event) => setHttpApiPort(Number(event.target.value))}
                            className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)]"
                          />
                        </label>
                        <button
                          type="button"
                          // Named, unlike the MCP section's: two buttons
                          // reading "Apply" in one dialog say nothing about
                          // which listener they move.
                          aria-label="Apply HTTP API settings"
                          disabled={
                            httpApiBusy
                            || httpApiBindAddress.trim().length === 0
                            || httpApiPort < 1
                            || httpApiPort > 65535
                          }
                          onClick={() => void configureHttpApi(
                            httpApiStatus?.enabled ?? settings.http_api?.enabled ?? false,
                            httpApiBindAddress,
                            httpApiPort,
                          )}
                          className="px-3 py-1.5 text-xs rounded border border-[var(--border-main)] bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
                        >
                          Apply
                        </button>
                      </div>

                      {httpApiStatus?.running && httpApiStatus.url && (
                        <div className="space-y-2 rounded border border-[var(--border-main)] bg-[var(--bg-active)]/30 p-2.5">
                          <p className="text-[10px] text-[var(--text-dim)] mb-1">Endpoint</p>
                          <button
                            type="button"
                            onClick={() => void copyExternalMcpText(httpApiStatus.url!)}
                            className="w-full text-left text-[10px] font-mono break-all text-[var(--text-main)] hover:text-[var(--accent-blue)]"
                            title="Copy endpoint"
                          >
                            {httpApiStatus.url}
                          </button>
                        </div>
                      )}

                      {httpApiBindAddress.trim() !== "::1" && !httpApiBindAddress.trim().startsWith("127.") && (
                        <div role="status" className="p-2 bg-amber-900/20 border border-amber-800/50 rounded text-[10px] text-amber-300">
                          A non-loopback address exposes this API without authentication, and it
                          can write as well as read — anyone who can reach the address can change
                          settings and rebuild the index. Use host firewall rules to restrict access.
                        </div>
                      )}

                      {httpApiError && (
                        <div role="alert" className="p-2 bg-red-900/20 border border-red-900/50 rounded text-[10px] text-red-400 break-all">
                          {httpApiError}
                        </div>
                      )}
                    </div>
                  </section>
                </div>
              )}
            </div>

            <div className={activeTab === "extraction-images" ? "block h-full" : "hidden"}>
              {settings && (
                <ImageAnalysisPanel
                  api={api}
                  settings={settings}
                  onUpdateSettings={handleUpdateSettings}
                />
              )}
            </div>

            <div className={activeTab === "semantic-chunking" ? "block h-full" : "hidden"}>
              {settings && (
                <ChunkingPanel api={api} settings={settings} onUpdate={setSettings} />
              )}
            </div>

            <div className={activeTab === "data" ? "block h-full" : "hidden"}>
              <DataPanel api={api} isActive={activeTab === "data"} />
            </div>

            <div className={activeTab === "workers" ? "block h-full" : "hidden"}>
              {settings && (
                <WorkersPanel api={api} settings={settings} onUpdateSettings={handleUpdateSettings} />
              )}
            </div>

            <div className={activeTab === "logs" ? "block h-full" : "hidden"}>
              <LogsPanel api={api} />
            </div>

            <div className={activeTab === "technical" ? "block h-full" : "hidden"}>
              <TechnicalSettings api={api} onUpdate={setSettings} />
            </div>
          </div>
        </div>

        <div className="px-4 py-3 border-t border-[var(--border-main)] bg-[var(--bg-header)] flex justify-end">
          <button
            onClick={onClose}
            className="px-4 py-1.5 bg-[var(--bg-active)] hover:bg-[var(--bg-hover)] text-[var(--text-main)] text-sm font-medium rounded-lg transition-colors border border-[var(--border-main)]"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
