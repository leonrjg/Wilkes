import { useState } from "react";
import type { Settings } from "../lib/types";

interface ExtensionsPanelProps {
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => void;
}

export default function ExtensionsPanel({ settings, onUpdate }: ExtensionsPanelProps) {
  const [newExt, setNewExt] = useState("");

  const handleAdd = () => {
    let clean = newExt.trim().toLowerCase();
    if (clean.startsWith(".")) clean = clean.substring(1);
    if (clean && !settings.supported_extensions.includes(clean)) {
      onUpdate({
        supported_extensions: [...settings.supported_extensions, clean].sort(),
      });
      setNewExt("");
    }
  };

  const handleRemove = (ext: string) => {
    onUpdate({
      supported_extensions: settings.supported_extensions.filter((e) => e !== ext),
    });
  };

  return (
    <div className="space-y-4 animate-in fade-in slide-in-from-bottom-2 duration-300 p-1">
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Manage File Extensions
        </h3>
        <p className="text-[10px] text-[var(--text-dim)] mb-4 italic">
          Files with these extensions will be indexed and searchable. Plain text and PDF are supported.
        </p>

        <div className="flex gap-2 mb-4">
          <input
            type="text"
            value={newExt}
            onChange={(e) => setNewExt(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleAdd()}
            placeholder="e.g. rs, py, txt"
            className="flex-1 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors"
          />
          <button
            onClick={handleAdd}
            disabled={!newExt.trim()}
            className="px-3 py-1.5 bg-[var(--accent-blue)] hover:bg-[var(--accent-blue-hover)] text-white text-[10px] font-bold uppercase tracking-wider rounded transition-colors disabled:opacity-50"
          >
            Add
          </button>
        </div>

        <div className="grid grid-cols-4 gap-2 max-h-[300px] overflow-y-auto pr-1 custom-scrollbar">
          {settings.supported_extensions.map((ext) => (
            <div
              key={ext}
              className="flex items-center justify-between px-2 py-1 bg-[var(--bg-active)]/50 border border-[var(--border-main)] rounded group hover:border-[var(--border-strong)] transition-colors"
            >
              <span className="text-xs text-[var(--text-main)] font-mono">.{ext}</span>
              <button
                onClick={() => handleRemove(ext)}
                className="text-[var(--text-dim)] hover:text-red-400 opacity-0 group-hover:opacity-100 transition-all p-0.5"
                title="Remove"
              >
                ✕
              </button>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
