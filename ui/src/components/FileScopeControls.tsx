import { useEffect, useState } from "react";
import { Check, Edit2, Plus, Sliders, X } from "react-feather";
import { api } from "../services";
import { useResearchStore } from "../stores/useResearchStore";
import { ResearchManager } from "./ResearchManager";
import { Tooltip } from "@leonrjg/wilkes-reader";

export function FileScopeControls({ matchCount }: { matchCount: number }) {
  const tags = useResearchStore((state) => state.tags);
  const collections = useResearchStore((state) => state.collections);
  const selectedCollectionId = useResearchStore((state) => state.selectedCollectionId);
  const selectedTagId = useResearchStore((state) => state.selectedTagId);
  const setSelectedCollection = useResearchStore((state) => state.setSelectedCollection);
  const setSelectedTag = useResearchStore((state) => state.setSelectedTag);
  const setDraftExpression = useResearchStore((state) => state.setDraftCollectionExpression);
  const load = useResearchStore((state) => state.load);
  const saveCollection = useResearchStore((state) => state.saveCollection);
  const [builderOpen, setBuilderOpen] = useState(false);
  const [managerOpen, setManagerOpen] = useState(false);
  const [name, setName] = useState("");
  const [expression, setExpression] = useState("");
  const [validation, setValidation] = useState<"idle" | "validating" | "valid" | "invalid">("idle");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => { load().catch(console.error); }, [load]);

  useEffect(() => {
    if (!builderOpen) return;
    const value = expression.trim();
    if (!value) {
      setValidation("idle");
      setError(null);
      setDraftExpression(null);
      return;
    }
    setValidation("validating");
    let cancelled = false;
    const timeout = window.setTimeout(() => {
      api.validateSmartCollection(value)
        .then((result) => {
          if (cancelled) return;
          if (result.valid) {
            setValidation("valid");
            setError(null);
            setDraftExpression(value);
          } else {
            setValidation("invalid");
            setError(result.error ?? "Invalid collection expression");
            setDraftExpression(null);
          }
        })
        .catch((reason) => {
          if (cancelled) return;
          setValidation("invalid");
          setError(String(reason));
          setDraftExpression(null);
        });
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timeout);
    };
  }, [builderOpen, expression, setDraftExpression]);

  const closeBuilder = () => {
    setBuilderOpen(false);
    setDraftExpression(null);
    setValidation("idle");
    setError(null);
  };

  const save = async () => {
    if (!name.trim() || validation !== "valid") return;
    setSaving(true);
    try {
      const saved = await saveCollection(null, { name: name.trim(), expression: expression.trim() });
      useResearchStore.setState({
        selectedCollectionId: saved.id,
        draftCollectionExpression: null,
      });
      setName("");
      setExpression("");
      setBuilderOpen(false);
      setValidation("idle");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex-shrink-0 border-b border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1">
      <div className="flex items-center gap-1">
        <select
          aria-label="Smart collection"
          value={selectedCollectionId ?? ""}
          onChange={(event) => {
            closeBuilder();
            setSelectedCollection(event.target.value || null);
          }}
          className="h-7 min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2 text-xs text-[var(--text-main)]"
        >
          <option value="">All documents</option>
          {collections.map((collection) => <option key={collection.id} value={collection.id}>{collection.name}</option>)}
        </select>
        <select
          aria-label="Document tag"
          value={selectedTagId ?? ""}
          onChange={(event) => setSelectedTag(event.target.value || null)}
          className="h-7 min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2 text-xs text-[var(--text-main)]"
        >
          <option value="">All tags</option>
          {tags.map((tag) => <option key={tag.id} value={tag.id}>{tag.name}</option>)}
        </select>
        <Tooltip content={builderOpen ? "Close collection builder" : "Create collection"}>
          <button
            type="button"
            aria-label={builderOpen ? "Close collection builder" : "Create collection"}
            aria-expanded={builderOpen}
            onClick={() => {
              if (builderOpen) closeBuilder();
              else {
                setSelectedCollection(null);
                setBuilderOpen(true);
              }
            }}
            className={`flex h-7 w-7 flex-shrink-0 items-center justify-center rounded border ${builderOpen ? "border-[var(--accent-blue)] text-[var(--accent-blue)]" : "border-[var(--border-main)] text-[var(--text-muted)] hover:text-[var(--text-main)]"}`}
          >
            {builderOpen ? <X size={13} /> : <Plus size={13} />}
          </button>
        </Tooltip>
        <Tooltip content="Manage tags and saved collections">
          <button type="button" aria-label="Manage tags and saved collections" onClick={() => setManagerOpen(true)} className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded border border-[var(--border-main)] text-[var(--text-muted)] hover:text-[var(--text-main)]"><Sliders size={13} /></button>
        </Tooltip>
      </div>

      {builderOpen && (
        <section aria-label="Collection builder" className="mt-2 space-y-2 rounded border border-[var(--border-main)] bg-[var(--bg-app)] p-2">
          <div className="flex items-center gap-2">
            <Edit2 size={12} className="text-[var(--accent-blue)]" />
            <span className="text-xs font-semibold text-[var(--text-main)]">New smart collection</span>
            <span className="ml-auto text-[10px] tabular-nums text-[var(--text-dim)]">
              {validation === "valid" ? `${matchCount} matching` : validation === "validating" ? "Checking…" : "Preview paused"}
            </span>
          </div>
          <input aria-label="Collection name" value={name} onChange={(event) => setName(event.target.value)} placeholder="Collection name" className="w-full rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2 py-1.5 text-xs text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]" />
          <textarea aria-label="Collection expression" value={expression} onChange={(event) => setExpression(event.target.value)} placeholder="citation_count > 1" rows={3} spellCheck={false} className="w-full resize-y rounded border border-[var(--border-main)] bg-[var(--bg-input)] p-2 font-mono text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]" />
          <div className="flex flex-wrap gap-1">
            {tags.map((tag) => <button type="button" key={tag.id} onClick={() => setExpression((value) => `${value}${value.trim() ? " && " : ""}'${tag.id}' in tags`)} className="rounded bg-[var(--accent-blue-muted)] px-1.5 py-0.5 text-[10px] text-[var(--accent-blue)]">+ {tag.name}</button>)}
          </div>
          {error && <p className="text-[10px] leading-snug text-red-400">{error}</p>}
          <div className="flex items-center gap-2">
            <p className="min-w-0 flex-1 truncate text-[9px] text-[var(--text-dim)]">tags · title · author · publication_year · citation_count · file_type · extension · path</p>
            <button type="button" disabled={!name.trim() || validation !== "valid" || saving} onClick={save} className="inline-flex items-center gap-1 rounded bg-[var(--accent-blue)] px-2 py-1 text-[10px] text-white disabled:opacity-40"><Check size={11} /> {saving ? "Saving…" : "Save"}</button>
          </div>
        </section>
      )}
      <ResearchManager open={managerOpen} onClose={() => { setManagerOpen(false); load().catch(console.error); }} />
    </div>
  );
}
