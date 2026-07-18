import { useEffect, useState } from "react";
import { Edit2, Tag as TagIcon, Trash2, X } from "react-feather";
import { api } from "../services";
import { useResearchStore } from "../stores/useResearchStore";

export function ResearchManager({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [tab, setTab] = useState<"tags" | "collections">("collections");
  const store = useResearchStore();
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [expression, setExpression] = useState("size(tags) > 0");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    store.load().catch((e) => setError(String(e)));
  }, [open]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!open) return null;

  const beginEdit = (id: string | null) => {
    const item = store.collections.find((collection) => collection.id === id);
    setEditingId(id);
    setName(item?.name ?? "");
    setExpression(item?.expression ?? "size(tags) > 0");
    setError(null);
  };

  const save = async () => {
    if (!editingId) return;
    const validation = await api.validateSmartCollection(expression);
    if (!validation.valid) {
      setError(validation.error ?? "Invalid CEL expression");
      return;
    }
    try {
      await store.saveCollection(editingId, { name, expression });
      beginEdit(null);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="fixed inset-0 z-[1000] flex items-center justify-center bg-black/50" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <section className="flex h-[min(720px,86vh)] w-[min(920px,92vw)] flex-col rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] shadow-2xl">
        <header className="flex items-center gap-2 border-b border-[var(--border-main)] px-4 py-3">
          <h2 className="flex-1 text-sm font-semibold text-[var(--text-main)]">Document organization</h2>
          <button onClick={onClose} aria-label="Close"><X size={18} /></button>
        </header>
        <nav className="flex gap-1 border-b border-[var(--border-main)] px-4 py-2">
          {(["collections", "tags"] as const).map((value) => (
            <button key={value} onClick={() => setTab(value)} className={`rounded px-3 py-1.5 text-xs capitalize ${tab === value ? "bg-[var(--accent-blue)] text-white" : "text-[var(--text-muted)] hover:bg-[var(--bg-hover)]"}`}>{value}</button>
          ))}
        </nav>
        <div className="min-h-0 flex-1 overflow-auto p-4">
          {tab === "tags" && (
            <div className="mx-auto max-w-xl space-y-3">
              {store.tags.map((tag) => (
                <div key={tag.id} className="flex items-center gap-2 rounded border border-[var(--border-main)] p-2">
                  <TagIcon size={14} className="text-[var(--accent-blue)]" />
                  <span className="flex-1 text-sm">{tag.name}</span>
                  <code className="max-w-48 truncate text-[10px] text-[var(--text-dim)]">{tag.id}</code>
                  <button aria-label={`Rename ${tag.name}`} onClick={async () => { const next = window.prompt("Tag name", tag.name); if (next?.trim()) await store.updateTag(tag.id, { name: next, color: tag.color }); }}><Edit2 size={14} /></button>
                  <button aria-label={`Delete ${tag.name}`} onClick={() => store.deleteTag(tag.id).catch((e) => setError(String(e)))}><Trash2 size={14} /></button>
                </div>
              ))}
              <p className="text-xs text-[var(--text-dim)]">Create and assign tags directly from a document’s context menu. This screen manages existing tag names; IDs remain stable when names change.</p>
            </div>
          )}
          {tab === "collections" && (
            <div className="grid min-h-full grid-cols-[minmax(180px,0.35fr)_minmax(0,1fr)] gap-4">
              <aside className="space-y-1 border-r border-[var(--border-main)] pr-3">
                {store.collections.map((collection) => (
                  <button key={collection.id} onClick={() => beginEdit(collection.id)} className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs ${editingId === collection.id ? "bg-[var(--bg-active)]" : "hover:bg-[var(--bg-hover)]"}`}><Edit2 size={12} /> <span className="truncate">{collection.name}</span></button>
                ))}
              </aside>
              <div className="space-y-3">
                {!editingId && <p className="rounded border border-dashed border-[var(--border-main)] p-4 text-center text-xs text-[var(--text-dim)]">Select a saved collection to edit it. New collections are created interactively in the file sidebar.</p>}
                <label className="block text-xs text-[var(--text-muted)]">Name<input disabled={!editingId} value={name} onChange={(e) => setName(e.target.value)} className="mt-1 block w-full rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-main)] disabled:opacity-50" /></label>
                <label className="block text-xs text-[var(--text-muted)]">CEL expression<textarea disabled={!editingId} value={expression} onChange={(e) => setExpression(e.target.value)} rows={7} spellCheck={false} className="mt-1 block w-full rounded border border-[var(--border-main)] bg-[var(--bg-input)] p-3 font-mono text-xs text-[var(--text-main)] disabled:opacity-50" /></label>
                <div className="flex flex-wrap gap-1">
                  {store.tags.map((tag) => <button disabled={!editingId} key={tag.id} onClick={() => setExpression((value) => `${value}${value.trim() ? " && " : ""}'${tag.id}' in tags`)} className="rounded bg-[var(--accent-blue-muted)] px-2 py-1 text-[10px] text-[var(--accent-blue)] disabled:opacity-40">+ {tag.name}</button>)}
                </div>
                <p className="text-[11px] text-[var(--text-dim)]">Fields: tags, title, author, publication_year, citation_count, file_type, extension, root, path. The backend validates and evaluates this expression everywhere.</p>
                {error && <p className="rounded bg-red-500/10 p-2 text-xs text-red-400">{error}</p>}
                <div className="flex gap-2">
                  <button disabled={!editingId || !name.trim() || !expression.trim()} onClick={save} className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs text-white disabled:opacity-50">Save</button>
                  {editingId && <button onClick={async () => { await store.deleteCollection(editingId); beginEdit(null); }} className="rounded border border-red-500/40 px-3 py-1.5 text-xs text-red-400">Delete</button>}
                </div>
              </div>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
