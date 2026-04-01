import { useState, useEffect } from "react";
import type { SearchApi } from "../services/api";
import type { Settings } from "../lib/types";
import SemanticPanel from "./SemanticPanel";

interface SettingsModalProps {
  api: SearchApi;
  isOpen: boolean;
  onClose: () => void;
  directory: string;
  refreshSemanticReady: () => Promise<void>;
  onSettingsUpdate?: (patch: Partial<Settings>) => void;
}

export default function SettingsModal({
  api,
  isOpen,
  onClose,
  directory,
  refreshSemanticReady,
  onSettingsUpdate,
}: SettingsModalProps) {
  const [activeTab, setActiveTab] = useState<"general" | "semantic">("general");
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

  return (
    <div className="fixed inset-0 z-[100] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4">
      <div className="bg-[var(--bg-app)] border border-[var(--border-main)] rounded-xl shadow-2xl w-full max-w-xl h-[600px] flex flex-col overflow-hidden animate-in fade-in zoom-in duration-200">
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
          <div className="w-40 border-r border-[var(--border-main)] bg-[var(--bg-sidebar)] p-1.5 flex flex-col gap-0.5">
            <button
              onClick={() => setActiveTab("general")}
              className={`px-3 py-1.5 rounded-lg text-sm text-left transition-colors ${
                activeTab === "general"
                  ? "bg-[var(--bg-active)] text-[var(--text-main)]"
                  : "text-[var(--text-muted)] hover:bg-[var(--bg-active)]/50 hover:text-[var(--text-main)]"
              }`}
            >
              General
            </button>
            <button
              onClick={() => setActiveTab("semantic")}
              className={`px-3 py-1.5 rounded-lg text-sm text-left transition-colors ${
                activeTab === "semantic"
                  ? "bg-[var(--bg-active)] text-[var(--text-main)]"
                  : "text-[var(--text-muted)] hover:bg-[var(--bg-active)]/50 hover:text-[var(--text-main)]"
              }`}
            >
              Semantic Search
            </button>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto p-4 bg-[var(--bg-app)]">
            {activeTab === "general" && settings && (
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

            {activeTab === "semantic" && (
              <SemanticPanel
                api={api}
                directory={directory}
                refreshSemanticReady={refreshSemanticReady}
              />
            )}
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
