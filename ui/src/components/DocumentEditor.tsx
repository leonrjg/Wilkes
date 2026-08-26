import { useEffect, useRef, useState } from "react";
import { EditorState, Prec, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  WidgetType,
} from "@codemirror/view";
import { basicSetup } from "codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { Check, ChevronDown, RefreshCw, Save, X } from "react-feather";
import { api } from "../services";
import { randomId, type CompletionEvent, type SessionSteering } from "../lib/types";
import { useEditorStore } from "../stores/useEditorStore";
import { useViewerStore } from "../stores/useViewerStore";
import { fileName } from "./DocumentEntryRow";
import { getLanguageExtension } from "./preview/CodeViewer";
import { useSettingsStore } from "../stores/useSettingsStore";

const setGhost = StateEffect.define<{ position: number; text: string } | null>();

class GhostWidget extends WidgetType {
  constructor(readonly text: string) {
    super();
  }

  eq(other: GhostWidget) {
    return other.text === this.text;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-grounded-ghost";
    span.textContent = this.text;
    return span;
  }

  ignoreEvent() {
    return true;
  }
}

const ghostField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, transaction) {
    for (const effect of transaction.effects) {
      if (!effect.is(setGhost)) continue;
      if (effect.value === null || !effect.value.text) return Decoration.none;
      return Decoration.set([
        Decoration.widget({
          widget: new GhostWidget(effect.value.text),
          side: 1,
        }).range(effect.value.position),
      ]);
    }
    return value.map(transaction.changes);
  },
  provide: (field) => EditorView.decorations.from(field),
});

const completionTheme = EditorView.baseTheme({
  ".cm-grounded-ghost": {
    color: "var(--text-dim)",
    opacity: "0.62",
    whiteSpace: "pre-wrap",
    pointerEvents: "none",
  },
});

function unicodeScalarOffset(text: string, utf16Offset: number): number {
  return Array.from(text.slice(0, utf16Offset)).length;
}

function displayedCompletion(prefix: string, completion: string): string {
  const before = Array.from(prefix).pop();
  const first = completion[Symbol.iterator]().next().value as string | undefined;
  return before && first && /[\p{L}\p{N}]/u.test(before) && /[\p{L}\p{N}]/u.test(first)
    ? ` ${completion}`
    : completion;
}

interface Props {
  content: string;
  language: string | null;
  documentPath: string;
  semanticReady: boolean;
  generationReady: boolean;
  onSaved?: () => void;
}

export default function DocumentEditor({
  content,
  language,
  documentPath,
  semanticReady,
  generationReady,
  onSaved,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const debounceRef = useRef<number | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);
  const activeRequestRef = useRef<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [steering, setSteering] = useState<SessionSteering | null>(null);
  const isDark = useSettingsStore((state) => state.colorScheme) === "dark";
  const buffer = useEditorStore((state) => state.buffers[documentPath]);
  const tabs = useViewerStore((state) => state.tabs);
  const openMatch = useViewerStore((state) => state.openMatch);
  const ensureBuffer = useEditorStore((state) => state.ensureBuffer);

  useEffect(() => ensureBuffer(documentPath, content), [content, documentPath, ensureBuffer]);

  useEffect(() => {
    useEditorStore.getState().setActiveEditor(documentPath);
    return () => {
      if (useEditorStore.getState().activeEditorPath === documentPath) {
        useEditorStore.getState().setActiveEditor(null);
      }
    };
  }, [documentPath]);

  const disposeListener = () => {
    unlistenRef.current?.();
    unlistenRef.current = null;
  };

  const cancelActive = (feedback?: "dismissed" | "typed_through") => {
    const id = activeRequestRef.current;
    if (!id) return;
    activeRequestRef.current = null;
    disposeListener();
    void api.cancelCompletion(id).catch(() => {});
    if (feedback) void api.completionFeedback(id, feedback).catch(() => {});
    useEditorStore.getState().clearCompletion(documentPath);
    const view = viewRef.current;
    if (view) view.dispatch({ effects: setGhost.of(null) });
  };

  const requestCompletion = async () => {
    if (!semanticReady || !generationReady) return;
    const view = viewRef.current;
    if (!view || !view.state.selection.main.empty) return;
    const previous = useEditorStore.getState().buffers[documentPath]?.completion;
    cancelActive(previous?.text ? "dismissed" : undefined);
    const id = `completion-${randomId()}`;
    const text = view.state.doc.toString();
    const cursor = view.state.selection.main.head;
    const scope = useEditorStore.getState().buffers[documentPath]?.scope ?? {
      mode: "library" as const,
      pinned: [],
      excluded: [],
    };
    const avoidSuggestions = useEditorStore.getState().buffers[documentPath]?.suggestionHistory ?? [];
    activeRequestRef.current = id;
    useEditorStore.getState().beginCompletion(documentPath, id);
    try {
      unlistenRef.current = await api.onCompletion(id, (event: CompletionEvent) => {
        useEditorStore.getState().applyCompletionEvent(documentPath, id, event);
        if (event.kind === "shown") {
          const currentView = viewRef.current;
          if (!currentView || currentView.state.selection.main.head !== cursor) return;
          const shown = displayedCompletion(text.slice(0, cursor), event.text);
          currentView.dispatch({ effects: setGhost.of({ position: cursor, text: shown }) });
        }
        if (event.kind === "suppressed" || event.kind === "error") {
          activeRequestRef.current = null;
          disposeListener();
        }
      });
      await api.requestCompletion(id, {
        path: documentPath,
        text,
        cursor: unicodeScalarOffset(text, cursor),
        scope,
        avoid_suggestions: avoidSuggestions,
      });
    } catch (error) {
      useEditorStore.getState().applyCompletionEvent(documentPath, id, {
        kind: "error",
        message: error instanceof Error ? error.message : String(error),
      });
      activeRequestRef.current = null;
      disposeListener();
    }
  };

  const scheduleCompletion = () => {
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => void requestCompletion(), 450);
  };

  const accept = (partial: boolean): boolean => {
    const view = viewRef.current;
    const current = useEditorStore.getState().buffers[documentPath]?.completion;
    if (!view || !current?.text) return false;
    const cursor = view.state.selection.main.head;
    const full = displayedCompletion(view.state.sliceDoc(0, cursor), current.text);
    const inserted = partial ? full.match(/^\s*\S+\s*/)?.[0] ?? full : full;
    const id = current.id;
    activeRequestRef.current = null;
    disposeListener();
    useEditorStore.getState().clearCompletion(documentPath);
    void api.completionFeedback(id, partial ? "partial" : "accepted").catch(() => {});
    view.dispatch({ changes: { from: cursor, insert: inserted }, effects: setGhost.of(null) });
    return true;
  };

  const save = async () => {
    const view = viewRef.current;
    if (!view || saving) return;
    setSaving(true);
    setSaveError(null);
    try {
      const text = view.state.doc.toString();
      await api.saveDocument(documentPath, text);
      useEditorStore.getState().markSaved(documentPath);
      useViewerStore.setState((state) => ({
        tabs: state.tabs.map((tab) =>
          tab.path === documentPath && tab.previewData && "Text" in tab.previewData
            ? {
                ...tab,
                previewData: {
                  Text: { ...tab.previewData.Text, content: text },
                },
              }
            : tab,
        ),
      }));
      onSaved?.();
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    if (!containerRef.current || !buffer) return;
    const languageExtension = getLanguageExtension(language);
    const extensions = [
      basicSetup,
      ghostField,
      completionTheme,
      EditorView.lineWrapping,
      Prec.highest(keymap.of([
        { key: "Tab", run: () => accept(false) },
        { key: "Mod-ArrowRight", run: () => accept(true) },
        { key: "Escape", run: () => { cancelActive("dismissed"); return true; } },
        { key: "Mod-Space", run: () => { void requestCompletion(); return true; } },
        { key: "Mod-s", run: () => { void save(); return true; } },
      ])),
      EditorView.updateListener.of((update) => {
        const position = update.state.selection.main.head;
        if (update.docChanged) {
          const active = useEditorStore.getState().buffers[documentPath]?.completion;
          cancelActive(active?.text ? "typed_through" : undefined);
          useEditorStore.getState().updateBuffer(documentPath, update.state.doc.toString(), position);
          scheduleCompletion();
        } else if (update.selectionSet) {
          useEditorStore.getState().setCursor(documentPath, position);
          if (activeRequestRef.current) {
            const active = useEditorStore.getState().buffers[documentPath]?.completion;
            cancelActive(active?.text ? "dismissed" : undefined);
          }
        }
      }),
    ];
    if (isDark) extensions.push(oneDark);
    if (languageExtension) extensions.push(languageExtension);
    const state = EditorState.create({ doc: buffer.text, extensions });
    const view = new EditorView({ state, parent: containerRef.current });
    viewRef.current = view;
    view.dispatch({ selection: { anchor: Math.min(buffer.cursor, view.state.doc.length) } });
    return () => {
      if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
      const active = useEditorStore.getState().buffers[documentPath]?.completion;
      cancelActive(active?.text ? "dismissed" : undefined);
      view.destroy();
      viewRef.current = null;
    };
    // The editor owns subsequent buffer changes; remount only for a new path/theme/language.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentPath, language, isDark, semanticReady, generationReady, Boolean(buffer)]);

  const shown = buffer?.completion?.text ? buffer.completion : null;
  const inspect = buffer?.completion ?? buffer?.lastCompletion ?? null;
  const otherTabs = tabs.filter((tab) => tab.path !== documentPath);
  const contextUsagePercent = inspect?.composition && inspect.composition.windowTokens > 0
    ? Math.min(100, Math.round(
        (inspect.composition.usedTokens / inspect.composition.windowTokens) * 100,
      ))
    : null;

  const openInspector = async () => {
    setInspectorOpen((open) => !open);
    try {
      setSteering(await api.getSessionSteering());
    } catch {
      setSteering(null);
    }
  };

  const regenerateAfterScopeChange = (updateScope: () => void) => {
    const active = useEditorStore.getState().buffers[documentPath]?.completion;
    updateScope();
    cancelActive(active?.text ? "dismissed" : undefined);
    useEditorStore.getState().clearCompletion(documentPath);
    viewRef.current?.dispatch({ effects: setGhost.of(null) });
    void requestCompletion();
  };

  return (
    <div className="relative flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-main)] bg-[var(--bg-header)] px-2 py-1.5 text-[10px]">
        <span className="text-[var(--text-muted)]">Context</span>
        {buffer?.scope.pinned.map((path) => (
          <button
            key={path}
            type="button"
            title={path}
            onClick={() => useEditorStore.getState().togglePin(documentPath, path)}
            className="inline-flex items-center gap-1 rounded bg-[var(--bg-active)] px-1.5 py-0.5 text-[var(--text-main)]"
          >
            {fileName(path)} <X size={9} />
          </button>
        ))}
        {otherTabs.length > 0 && (
          <label className="relative">
            <select
              aria-label="Pin open document to completion context"
              value=""
              onChange={(event) => {
                if (event.target.value) useEditorStore.getState().togglePin(documentPath, event.target.value);
              }}
              className="appearance-none rounded border border-[var(--border-main)] bg-[var(--bg-app)] py-0.5 pl-1.5 pr-5 text-[var(--text-muted)]"
            >
              <option value="">Pin open document…</option>
              {otherTabs.filter((tab) => !buffer?.scope.pinned.includes(tab.path)).map((tab) => (
                <option key={tab.path} value={tab.path}>{fileName(tab.path)}</option>
              ))}
            </select>
            <ChevronDown size={10} className="pointer-events-none absolute right-1 top-1/2 -translate-y-1/2" />
          </label>
        )}
        {Boolean(buffer?.scope.pinned.length) && (
          <select
            aria-label="Completion context mode"
            value={buffer?.scope.mode}
            onChange={(event) => useEditorStore.getState().setScopeMode(documentPath, event.target.value as "prefer" | "only")}
            className="rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 py-0.5 text-[var(--text-muted)]"
          >
            <option value="prefer">Prefer</option>
            <option value="only">Only</option>
          </select>
        )}
        <span className="ml-auto text-[var(--text-dim)]">
          {saveError ?? (buffer?.status === "searching" ? "Searching…" : buffer?.status === "nothing-relevant" ? "Nothing relevant" : buffer?.status === "error" ? buffer.error : "Ready")}
        </span>
        <button
          type="button"
          onClick={() => void openInspector()}
          className="rounded border border-[var(--border-main)] px-1.5 py-0.5 text-[var(--text-muted)]"
        >
          Inspect
        </button>
        <button
          type="button"
          disabled={!buffer?.dirty || saving}
          onClick={() => void save()}
          className="inline-flex items-center gap-1 rounded border border-[var(--border-main)] px-1.5 py-0.5 text-[var(--text-muted)] disabled:opacity-40"
        >
          {buffer?.dirty ? <Save size={10} /> : <Check size={10} />} {saving ? "Saving…" : "Save"}
        </button>
      </div>
      {!semanticReady && (
        <div className="border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">
          Build the semantic index in Settings → Semantic to enable grounded completions.
        </div>
      )}
      {semanticReady && !generationReady && (
        <div className="border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">
          Select and enable a model in Settings → Generation to enable grounded completions.
        </div>
      )}
      <div ref={containerRef} className="plain-text-editor min-h-0 flex-1 overflow-auto text-sm" />
      {shown && (
        <div className="relative flex items-center gap-2 border-t border-[var(--border-main)] bg-[var(--bg-header)] px-2 py-1 text-[10px]">
          <button type="button" onClick={() => void openInspector()} className="rounded bg-[var(--bg-active)] px-2 py-0.5 text-[var(--text-main)]">
            {shown.sources.length} source{shown.sources.length === 1 ? "" : "s"}
          </button>
          <button
            type="button"
            aria-label="Regenerate completion"
            title="Generate a different grounded suggestion"
            onClick={() => void requestCompletion()}
            className="inline-flex items-center gap-1 rounded border border-[var(--border-main)] px-1.5 py-0.5 text-[var(--text-muted)]"
          >
            <RefreshCw size={9} /> Regenerate
          </button>
          <span className="text-[var(--text-dim)]">Tab accepts · ⌘→ accepts one word · Esc dismisses · ⌘Space regenerates</span>
        </div>
      )}
      {inspectorOpen && (
        <div className="absolute bottom-8 right-3 z-40 max-h-[70%] w-[min(30rem,calc(100%-1.5rem))] overflow-auto rounded-lg border border-[var(--border-main)] bg-[var(--bg-sidebar)] p-3 text-xs shadow-xl">
          <div className="mb-2 flex items-center justify-between">
            <strong>Completion context</strong>
            <button type="button" onClick={() => setInspectorOpen(false)}><X size={14} /></button>
          </div>
          <div className="space-y-1">
            {inspect?.sources.map((source) => (
              <div
                key={`${source.path}:${source.chunkIds.join(",")}`}
                className="flex items-center rounded bg-[var(--bg-active)]"
              >
                <button
                  type="button"
                  onClick={() => openMatch({
                    path: source.path,
                    origin: source.page ? { PdfPage: { page: source.page, bbox: null } } : { TextFile: { line: 0, col: 0 } },
                  })}
                  className="min-w-0 flex-1 px-2 py-1 text-left hover:text-[var(--text-main)]"
                >
                  {source.title}{source.page ? `, p.${source.page}` : ""} · {source.score.toFixed(2)}{source.pinned ? " · pinned" : ""}
                </button>
                <button
                  type="button"
                  aria-label={`Remove ${source.title} from completion context`}
                  title={`Remove ${source.path} from completion context`}
                  onClick={() => regenerateAfterScopeChange(() => {
                    useEditorStore.getState().excludeFromContext(documentPath, source.path);
                  })}
                  className="self-stretch px-2 text-[var(--text-dim)] hover:text-[var(--text-main)]"
                >
                  <X size={12} />
                </button>
              </div>
            ))}
          </div>
          {inspect?.composition && (
            <div className="mt-2 text-[var(--text-dim)]">
              <p>
                Estimated context use: {inspect.composition.usedTokens.toLocaleString()} / {inspect.composition.windowTokens.toLocaleString()} tokens{contextUsagePercent === null ? "" : ` (${contextUsagePercent}%)`} · {inspect.composition.scopeMode}
              </p>
              {contextUsagePercent !== null && (
                <div
                  role="progressbar"
                  aria-label="Estimated context window usage"
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={contextUsagePercent}
                  className="mt-1 h-1.5 overflow-hidden rounded-full bg-[var(--border-main)]"
                >
                  <div
                    className="h-full rounded-full bg-[var(--accent-blue)]"
                    style={{ width: `${contextUsagePercent}%` }}
                  />
                </div>
              )}
              <p>
                Working document {inspect.composition.docTokens.toLocaleString()} · retrieval {inspect.composition.retrievalTokens.toLocaleString()} tokens · {inspect.composition.docCoverage.kind === "full" ? "full document" : `head ${inspect.composition.docCoverage.head_tokens} + tail ${inspect.composition.docCoverage.tail_tokens}, middle elided`}
              </p>
              {inspect.hydeQuery && <p className="mt-1 line-clamp-3">HyDE: {inspect.hydeQuery}</p>}
            </div>
          )}
          {Boolean(buffer?.scope.excluded.length) && (
            <div className="mt-3 border-t border-[var(--border-main)] pt-2">
              <strong>Excluded files</strong>
              <div className="mt-1 space-y-1">
                {buffer?.scope.excluded.map((path) => (
                  <div key={path} className="flex items-center gap-2 rounded bg-[var(--bg-active)] px-2 py-1">
                    <span className="min-w-0 flex-1 truncate" title={path}>{fileName(path)}</span>
                    <button
                      type="button"
                      aria-label={`Restore ${fileName(path)} to completion context`}
                      onClick={() => regenerateAfterScopeChange(() => {
                        useEditorStore.getState().restoreToContext(documentPath, path);
                      })}
                      className="text-[var(--accent-blue)]"
                    >
                      Restore
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}
          {steering && (
            <div className="mt-3 border-t border-[var(--border-main)] pt-2">
              <div className="flex items-center justify-between">
                <strong>Session steering</strong>
                <button
                  type="button"
                  onClick={async () => { await api.resetSessionSteering(); setSteering(await api.getSessionSteering()); }}
                  className="text-[var(--accent-blue)]"
                >
                  Clear
                </button>
              </div>
              {steering.documents.map((entry) => <p key={entry.path}>{fileName(entry.path)} · {entry.weight.toFixed(2)}</p>)}
              {steering.suppressions.map((entry, index) => (
                <div key={`${entry.reason}-${index}`} className="mt-1 rounded bg-[var(--bg-active)] p-1.5 text-[var(--text-dim)]">
                  <p>Suppressed: {entry.reason}</p>
                  {entry.candidate && <p className="line-clamp-2">Candidate: {entry.candidate}</p>}
                  {entry.hydeQuery && <p className="line-clamp-2">HyDE: {entry.hydeQuery}</p>}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
