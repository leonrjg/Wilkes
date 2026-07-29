import type { ReactNode } from "react";

export interface CatalogModel {
  model_id: string;
  display_name: string;
  description: string;
  is_cached: boolean;
  is_default: boolean;
  is_recommended: boolean;
  size_bytes: number | null;
}

interface ModelCatalogProps<T extends CatalogModel> {
  title: string;
  catalogKey: string;
  models: T[];
  filter: string;
  selectedModelId?: string | null;
  activeModelId?: string | null;
  sizeFetchingFor?: string | null;
  disabled?: boolean;
  emptyMessage?: string;
  toolbarAction?: ReactNode;
  toolbarContent?: ReactNode;
  onFilterChange: (filter: string) => void;
  onSelect: (model: T) => void;
}

export function formatModelBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  return `${Math.round(bytes / 1_048_576)} MB`;
}

/**
 * Shared model-browser presentation. Semantic and generation own different
 * lifecycle actions, but searching, selecting, and describing a model should
 * never drift into two subtly different interfaces.
 */
export default function ModelCatalog<T extends CatalogModel>({
  title,
  catalogKey,
  models,
  filter,
  selectedModelId,
  activeModelId,
  sizeFetchingFor = null,
  disabled = false,
  emptyMessage = "No models found",
  toolbarAction,
  toolbarContent,
  onFilterChange,
  onSelect,
}: ModelCatalogProps<T>) {
  const search = filter.trim().toLowerCase();
  const filtered = search
    ? models.filter(
        (model) =>
          model.model_id.toLowerCase().includes(search)
          || model.display_name.toLowerCase().includes(search)
          || model.description.toLowerCase().includes(search),
      )
    : models;

  const sorted = [...filtered].sort((a, b) => {
    if (activeModelId === a.model_id && activeModelId !== b.model_id) return -1;
    if (activeModelId !== a.model_id && activeModelId === b.model_id) return 1;
    return 0;
  });

  return (
    <section className="flex flex-col gap-2">
      <div className="flex gap-2">
        <input
          type="text"
          aria-label={`Search ${title.toLowerCase()}`}
          placeholder="Search models…"
          value={filter}
          onChange={(event) => onFilterChange(event.target.value)}
          disabled={disabled}
          className="flex-1 rounded-lg border border-[var(--border-main)] bg-[var(--bg-input)] px-2.5 py-1.5 text-xs text-[var(--text-main)] placeholder-[var(--text-dim)] transition-colors focus:border-[var(--accent-blue)] focus:outline-none disabled:opacity-50"
        />
        {toolbarAction}
      </div>

      {toolbarContent}

      <div className="flex items-center justify-between">
        <h3 className="text-[10px] font-medium uppercase tracking-wider text-[var(--text-dim)]">
          {title}
        </h3>
        <span className="text-[10px] uppercase text-[var(--text-dim)]">
          {filter
            ? `${sorted.length} match${sorted.length === 1 ? "" : "es"}`
            : `${sorted.length} available`}
        </span>
      </div>

      <div
        key={`${catalogKey}:${filter}`}
        className="custom-scrollbar flex max-h-40 flex-col gap-1 overflow-y-auto pr-1"
      >
        {sorted.length === 0 && (
          <span className="py-4 text-center text-xs text-[var(--text-muted)]">
            {emptyMessage}
          </span>
        )}
        {sorted.map((model) => {
          const selected = selectedModelId === model.model_id;
          return (
            <button
              key={`${catalogKey}:${model.model_id}`}
              disabled={disabled}
              type="button"
              onClick={() => onSelect(model)}
              className={`flex flex-col rounded-lg p-2 text-left transition-all ${
                selected
                  ? "bg-[var(--bg-active)] ring-1 ring-[var(--accent-blue)]/50"
                  : "border border-transparent hover:bg-[var(--bg-active)]/50"
              } ${disabled ? "cursor-not-allowed opacity-50" : "cursor-pointer"}`}
            >
              <div className="selectable mb-0.5 flex items-center gap-2">
                <span
                  className={`h-1.5 w-1.5 rounded-full ${
                    selected ? "bg-[var(--accent-blue)]" : "bg-[var(--bg-active)]"
                  }`}
                />
                <span
                  className={`text-[11px] font-medium ${
                    model.is_cached ? "text-[var(--text-main)]" : "text-[var(--text-muted)]"
                  }`}
                >
                  {model.display_name}
                </span>
                {activeModelId === model.model_id && (
                  <span className="rounded bg-[var(--accent-blue)]/10 px-1 text-[9px] font-bold uppercase tracking-tighter text-[var(--accent-blue)]">
                    Active
                  </span>
                )}
                {model.is_default && (
                  <span className="rounded bg-amber-500/10 px-1 text-[9px] font-bold uppercase tracking-tighter text-amber-500">
                    Default
                  </span>
                )}
                {model.is_recommended && !model.is_default && (
                  <span className="rounded bg-purple-500/10 px-1 text-[9px] font-bold uppercase tracking-tighter text-purple-500">
                    Recommended
                  </span>
                )}
                {model.is_cached && (
                  <span className="rounded bg-green-500/10 px-1 text-[9px] text-green-500">
                    Cached
                  </span>
                )}
                <span className="ml-auto text-[9px] text-[var(--text-dim)]">
                  {model.size_bytes ? formatModelBytes(model.size_bytes) : ""}
                </span>
              </div>
              <p className="selectable ml-3.5 line-clamp-1 text-[9px] leading-snug text-[var(--text-dim)]">
                {model.description}
              </p>
              {selected && !model.is_cached && (
                <span className="ml-3.5 mt-0.5 text-[9px] text-[var(--text-dim)]">
                  {sizeFetchingFor === model.model_id
                    ? "Checking size…"
                    : model.size_bytes !== null
                      ? `Estimated download: ${formatModelBytes(model.size_bytes)}`
                      : "Download required"}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </section>
  );
}
