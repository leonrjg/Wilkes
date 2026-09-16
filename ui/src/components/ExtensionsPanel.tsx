import { useState } from "react";
import type { Settings } from "../lib/types";
import { Tooltip } from "@leonrjg/wilkes-reader";
import {
  ALWAYS_ENABLED_DOCUMENT_EXTENSIONS,
  isDocumentExtension,
} from "../lib/documentFormats";

interface ExtensionsPanelProps {
  settings: Settings;
  onUpdate: (patch: Partial<Settings>) => void;
}

export default function ExtensionsPanel({ settings, onUpdate }: ExtensionsPanelProps) {
  const [newExt, setNewExt] = useState("");
  const [notice, setNotice] = useState<string | null>(null);

  // Documents never appear in the setting — the backend strips them on read,
  // because `FileType::detect` admits them whatever this list holds. Filtered
  // here as well so a settings object built before that (a stale store, a test
  // fixture) cannot show a book as a removable entry.
  const textExtensions = settings.supported_extensions.filter(
    (ext) => !isDocumentExtension(ext),
  );

  const handleAdd = () => {
    let clean = newExt.trim().toLowerCase();
    if (clean.startsWith(".")) clean = clean.substring(1);
    if (!clean) return;
    if (isDocumentExtension(clean)) {
      setNotice(`.${clean} is a document format and is always read.`);
      setNewExt("");
      return;
    }
    if (!textExtensions.includes(clean)) {
      onUpdate({
        supported_extensions: [...textExtensions, clean].sort(),
      });
      setNewExt("");
      setNotice(null);
    }
  };

  const handleRemove = (ext: string) => {
    onUpdate({
      supported_extensions: textExtensions.filter((e) => e !== ext),
    });
  };

  return (
    <div className="space-y-4 animate-in fade-in slide-in-from-bottom-2 duration-300 p-1">
      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Documents
        </h3>
        <p className="text-[10px] text-[var(--text-dim)] mb-3 italic">
          Always read, and not a setting: every document in a library is indexed and
          searchable whatever the text extensions below say.
        </p>
        <div className="grid grid-cols-4 gap-2 mb-6">
          {ALWAYS_ENABLED_DOCUMENT_EXTENSIONS.map((ext) => (
            <Tooltip key={ext} content="Always enabled">
              <div className="flex items-center justify-between px-2 py-1 bg-[var(--bg-active)]/30 border border-[var(--border-main)] rounded">
                <span className="text-xs text-[var(--text-main)] font-mono">.{ext}</span>
                <span className="text-[var(--text-dim)] text-[10px]" aria-hidden="true">
                  🔒
                </span>
              </div>
            </Tooltip>
          ))}
        </div>
      </section>

      <section>
        <h3 className="text-[10px] font-medium text-[var(--text-dim)] mb-2.5 uppercase tracking-wider">
          Manage Text Extensions
        </h3>
        <p className="text-[10px] text-[var(--text-dim)] mb-4 italic">
          Text files with these extensions will be indexed and searchable.
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

        {notice && (
          <p className="text-[10px] text-[var(--text-dim)] mb-3" role="status">
            {notice}
          </p>
        )}

        <div className="grid grid-cols-4 gap-2 max-h-[300px] overflow-y-auto pr-1 custom-scrollbar">
          {textExtensions.map((ext) => (
            <div
              key={ext}
              className="flex items-center justify-between px-2 py-1 bg-[var(--bg-active)]/50 border border-[var(--border-main)] rounded group hover:border-[var(--border-strong)] transition-colors"
            >
              <span className="text-xs text-[var(--text-main)] font-mono">.{ext}</span>
              <Tooltip content="Remove">
                <button
                  onClick={() => handleRemove(ext)}
                  className="text-[var(--text-dim)] hover:text-red-400 opacity-0 group-hover:opacity-100 transition-all p-0.5"
                >
                  ✕
                </button>
              </Tooltip>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
