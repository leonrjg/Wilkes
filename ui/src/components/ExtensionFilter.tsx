import type { FileEntry } from "../lib/types";

interface Props {
  fileList: FileEntry[];
  excluded: Set<string>;
  onChange: (excluded: Set<string>) => void;
}

export default function ExtensionFilter({ fileList, excluded, onChange }: Props) {
  const counts = fileList.reduce<Record<string, number>>((acc, f) => {
    acc[f.extension] = (acc[f.extension] ?? 0) + 1;
    return acc;
  }, {});

  const extensions = Object.keys(counts).sort();
  if (extensions.length === 0) return null;

  const toggle = (ext: string) => {
    const next = new Set(excluded);
    if (next.has(ext)) next.delete(ext);
    else next.add(ext);
    onChange(next);
  };

  return (
    <div className="flex items-center gap-1 flex-wrap">
      {extensions.map((ext) => {
        const active = !excluded.has(ext);
        return (
          <button
            key={ext}
            onClick={() => toggle(ext)}
            className={`px-2 py-0.5 rounded text-xs transition-colors ${
              active
                ? "bg-[var(--bg-active)] text-[var(--text-main)]"
                : "bg-[var(--bg-active)]/40 text-[var(--text-dim)]"
            }`}
          >
            .{ext} ({counts[ext]})
          </button>
        );
      })}
    </div>
  );
}
