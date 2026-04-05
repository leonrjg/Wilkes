import type { Settings } from "../lib/types";
import type { SearchApi } from "../services/api";

interface Props {
  api: SearchApi;
  settings: Settings;
  onUpdate: (s: Settings) => void;
}

export default function ChunkingPanel({ api, settings, onUpdate }: Props) {
  const updateSettings = async (patch: Partial<Settings["semantic"]>) => {
    const nextSemantic = { ...settings.semantic, ...patch };
    const nextSettings = { ...settings, semantic: nextSemantic };
    
    // Optimistic update
    onUpdate(nextSettings);
    
    try {
      await api.updateSettings({ semantic: nextSemantic });
    } catch (e) {
      console.error("Failed to update chunking settings:", e);
      // Revert on error
      onUpdate(settings);
    }
  };

  const sem = settings.semantic;

  return (
    <div className="flex flex-col gap-6">
      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">Chunking Strategy</h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Configure how files are split into smaller segments before embedding. 
            Smaller chunks provide more granular search results but less context per match.
          </p>
        </div>

        <div className="space-y-5">
          <div className="space-y-2">
            <div className="flex justify-between items-center">
              <label className="text-xs font-medium text-[var(--text-main)]">Chunk Size</label>
              <span className="text-[10px] font-mono text-[var(--accent-blue)] bg-[var(--accent-blue)]/10 px-1.5 py-0.5 rounded">
                {sem.chunk_size} characters
              </span>
            </div>
            <input
              type="range"
              min="100"
              max="5000"
              step="100"
              value={sem.chunk_size}
              onChange={(e) => updateSettings({ chunk_size: parseInt(e.target.value) })}
              className="w-full h-1.5 bg-[var(--bg-active)] rounded-lg appearance-none cursor-pointer accent-[var(--accent-blue)]"
            />
            <div className="flex justify-between text-[9px] text-[var(--text-dim)] uppercase tracking-tighter">
              <span>Granular (100)</span>
              <span>Broad (5000)</span>
            </div>
          </div>

          <div className="space-y-2">
            <div className="flex justify-between items-center">
              <label className="text-xs font-medium text-[var(--text-main)]">Overlap</label>
              <span className="text-[10px] font-mono text-[var(--accent-blue)] bg-[var(--accent-blue)]/10 px-1.5 py-0.5 rounded">
                {sem.chunk_overlap} characters
              </span>
            </div>
            <input
              type="range"
              min="0"
              max="1000"
              step="50"
              value={sem.chunk_overlap}
              onChange={(e) => updateSettings({ chunk_overlap: parseInt(e.target.value) })}
              className="w-full h-1.5 bg-[var(--bg-active)] rounded-lg appearance-none cursor-pointer accent-[var(--accent-blue)]"
            />
            <div className="flex justify-between text-[9px] text-[var(--text-dim)] uppercase tracking-tighter">
              <span>None (0)</span>
              <span>Heavy (1000)</span>
            </div>
          </div>
        </div>
      </section>

      <div className="p-3 bg-amber-900/10 border border-amber-900/20 rounded-lg flex gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-[11px] font-bold text-amber-600 uppercase tracking-tight">Rebuild Required</span>
          <p className="text-[10px] text-[var(--text-muted)] leading-relaxed">
            Changing chunking parameters only affects new files or a full index rebuild. 
            Existing embeddings will not be updated automatically.
          </p>
        </div>
      </div>
    </div>
  );
}
