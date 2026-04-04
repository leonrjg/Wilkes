import { useState, useEffect, useRef } from "react";
import type { SearchApi } from "../services/api";
import type { Settings } from "../lib/types";
import SemanticPanel from "./SemanticPanel";
import ChunkingPanel from "./ChunkingPanel";
import DataPanel from "./DataPanel";
import ExtensionsPanel from "./ExtensionsPanel";
import LogsPanel from "./LogsPanel";
import WorkersPanel from "./WorkersPanel";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { json } from "@codemirror/lang-json";
import { oneDark } from "@codemirror/theme-one-dark";
import { indentWithTab } from "@codemirror/commands";

interface SettingsModalProps {
  api: SearchApi;
  isOpen: boolean;
  onClose: () => void;
  directory: string;
  refreshSemanticReady: () => Promise<void>;
  onSettingsUpdate?: (patch: Partial<Settings>) => void;
}

function TechnicalSettings({ api, onUpdate }: { api: SearchApi; onUpdate: (s: Settings) => void }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [isDark, setIsDark] = useState(() => window.document.documentElement.classList.contains("dark"));

  useEffect(() => {
    const observer = new MutationObserver(() => {
      setIsDark(window.document.documentElement.classList.contains("dark"));
    });
    observer.observe(window.document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);

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
  const [activeTab, setActiveTab] = useState<"general" | "extensions" | "models" | "chunking" | "data" | "workers" | "logs" | "technical">("general");
  const [settings, setSettings] = useState<Settings | null>(null);

  useEffect(() => {
    if (isOpen) {
      api.getSettings().then(setSettings).catch(console.error);
    }
  }, [isOpen, api]);

  if (!isOpen) return null;

  const handleUpdateSettings = async (patch: Partial<Settings>) => {
    try {
      const newSettings = await api.updateSettings(patch);
      setSettings(newSettings);
      if (onSettingsUpdate) onSettingsUpdate(patch);
    } catch (e) {
      console.error("Failed to update settings:", e);
    }
  };

  const TabButton = ({ id, label, indent = false }: { id: typeof activeTab; label: string; indent?: boolean }) => (
    <button
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
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="bg-[var(--bg-app)] border border-[var(--border-main)] rounded-xl shadow-2xl w-full max-w-2xl h-[800px] max-h-[90vh] flex flex-col overflow-hidden animate-in fade-in zoom-in duration-200">
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-[var(--border-main)]">
          <h2 className="text-base font-semibold text-[var(--text-main)] flex items-center gap-2">
            <span className="text-[var(--text-dim)] text-lg">⚙</span> Settings
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
          <div className="w-44 border-r border-[var(--border-main)] bg-[var(--bg-sidebar)] p-2 flex flex-col gap-4">
            <div className="flex flex-col gap-0.5">
              <TabButton id="general" label="General" />
              <TabButton id="extensions" label="Extensions" />
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Semantic Search</span>
              <TabButton id="models" label="Models" indent />
              <TabButton id="chunking" label="Chunking" indent />
            </div>

            <div className="flex flex-col gap-0.5">
              <span className="px-3 py-1 text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Advanced</span>
              <TabButton id="data" label="Data" indent />
              <TabButton id="workers" label="Workers" indent />
              <TabButton id="logs" label="Logs" indent />
              <TabButton id="technical" label="Technical" indent />
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

                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Context Lines ({settings.context_lines})</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Shown around matches</p>
                        </div>
                        <input
                          type="range"
                          min="0"
                          max="10"
                          value={settings.context_lines}
                          onChange={(e) => handleUpdateSettings({ context_lines: parseInt(e.target.value) })}
                          className="w-full h-1 bg-[var(--bg-active)] rounded-lg appearance-none cursor-pointer accent-[var(--accent-blue)]"
                        />
                      </div>

                      <div className="space-y-1">
                        <div className="flex justify-between items-baseline">
                          <label className="text-xs text-[var(--text-muted)]">Max File Size (MB)</label>
                          <p className="text-[10px] text-[var(--text-dim)] italic">Skip larger files</p>
                        </div>
                        <input
                          type="number"
                          value={Math.round(settings.max_file_size / (1024 * 1024))}
                          onChange={(e) => handleUpdateSettings({ max_file_size: parseInt(e.target.value) * 1024 * 1024 })}
                          className="w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
                        />
                      </div>
                    </div>
                  </section>

                  <section>
                    <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2 uppercase tracking-wider">Appearance</h3>
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
                  </section>
                </div>
              )}
            </div>

            <div className={activeTab === "extensions" ? "block h-full" : "hidden"}>
              {settings && (
                <ExtensionsPanel settings={settings} onUpdate={handleUpdateSettings} />
              )}
            </div>

            <div className={activeTab === "models" ? "block h-full" : "hidden"}>
              <SemanticPanel
                api={api}
                directory={directory}
                refreshSemanticReady={refreshSemanticReady}
              />
            </div>

            <div className={activeTab === "chunking" ? "block h-full" : "hidden"}>
              {settings && (
                <ChunkingPanel api={api} settings={settings} onUpdate={setSettings} />
              )}
            </div>

            <div className={activeTab === "data" ? "block h-full" : "hidden"}>
              <DataPanel api={api} />
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
