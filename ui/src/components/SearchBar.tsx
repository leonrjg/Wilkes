import { useState, useCallback, useEffect, useRef } from "react";
import { Search, Database, Check, Clock, Globe, Trash2, X } from "react-feather";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import type { SearchQuery } from "../lib/types";
import { Tooltip } from "./Tooltip";
import { api } from "../services";
import { useResearchStore } from "../stores/useResearchStore";

interface Props {
  sourceSlot: React.ReactNode;
  settingsSlot?: React.ReactNode;
}

export default function SearchBar({ sourceSlot, settingsSlot }: Props) {
  const search = useSearchStore((s) => s.search);
  const deferSemanticSearch = useSearchStore((s) => s.deferSemanticSearch);
  const searching = useSearchStore((s) => s.searching);
  const setHasQuery = useSearchStore((s) => s.setHasQuery);
  const clearResults = useSearchStore((s) => s.clearResults);

  const directory = useSettingsStore((s) => s.directory);
  const respectGitignore = useSettingsStore((s) => s.respectGitignore);
  const maxFileSize = useSettingsStore((s) => s.maxFileSize);
  const contextLines = useSettingsStore((s) => s.contextLines);
  const supportedExtensions = useSettingsStore((s) => s.supportedExtensions);
  const preferSemantic = useSettingsStore((s) => s.preferSemantic);
  const setPreferSemantic = useSettingsStore((s) => s.setPreferSemantic);
  const maxResults = useSettingsStore((s) => s.maxResults);
  const semanticReady = useSemanticStore((s) => s.readyForCurrentRoot);
  const semanticReadyGlobally = useSemanticStore((s) => s.readyGlobally);
  const refreshGlobalStatus = useSemanticStore((s) => s.refreshGlobalStatus);
  const ensureCurrentRootIndexed = useSemanticStore((s) => s.ensureCurrentRootIndexed);
  const semanticBuildRoot = useSemanticStore((s) => s.buildRoot);

  const [pattern, setPattern] = useState("");
  const [isRegex, setIsRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [isSemanticMode, setIsSemanticMode] = useState(preferSemantic);
  const [searchAll, setSearchAll] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const collections = useResearchStore((s) => s.collections);
  const tags = useResearchStore((s) => s.tags);
  const history = useResearchStore((s) => s.history);
  const selectedCollectionId = useResearchStore((s) => s.selectedCollectionId);
  const selectedTagId = useResearchStore((s) => s.selectedTagId);
  const setSelectedCollection = useResearchStore((s) => s.setSelectedCollection);
  const setSelectedTag = useResearchStore((s) => s.setSelectedTag);
  const loadHistory = useResearchStore((s) => s.loadHistory);
  const deleteHistory = useResearchStore((s) => s.deleteHistory);
  const clearHistory = useResearchStore((s) => s.clearHistory);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const prevSemanticReady = useRef(semanticReady);
  const prevFilterKey = useRef(`${selectedCollectionId ?? ""}|${selectedTagId ?? ""}`);
  const replayQueryRef = useRef<SearchQuery | null>(null);
  const historyRef = useRef<HTMLDivElement | null>(null);

  // Sync semantic mode when the setting is loaded from the backend
  useEffect(() => {
    setIsSemanticMode(preferSemantic);
  }, [preferSemantic]);

  const buildQuery = useCallback(
    (
      pat: string,
      opts: { isRegex?: boolean; caseSensitive?: boolean; isSemanticMode?: boolean; searchAll?: boolean } = {},
    ): SearchQuery => {
      return {
        pattern: pat,
        is_regex: opts.isRegex ?? isRegex,
        case_sensitive: opts.caseSensitive ?? caseSensitive,
        root: directory,
        max_results: maxResults,
        respect_gitignore: respectGitignore,
        max_file_size: maxFileSize,
        context_lines: contextLines,
        mode: (opts.isSemanticMode ?? isSemanticMode) ? "Semantic" : "Grep",
        scope: (opts.searchAll ?? searchAll) ? { type: "all" } : { type: "corpus" },
        supported_extensions: supportedExtensions,
        collection_id: selectedCollectionId,
        tag_ids: selectedTagId ? [selectedTagId] : [],
      };
    },
    [
      isRegex,
      caseSensitive,
      directory,
      respectGitignore,
      maxFileSize,
      contextLines,
      isSemanticMode,
      supportedExtensions,
      maxResults,
      searchAll,
      selectedCollectionId,
      selectedTagId,
    ],
  );

  useEffect(() => {
    if (!historyOpen) return;
    const closeOnOutsideInteraction = (event: PointerEvent) => {
      if (!historyRef.current?.contains(event.target as Node)) setHistoryOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setHistoryOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsideInteraction);
    window.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsideInteraction);
      window.removeEventListener("keydown", closeOnEscape);
    };
  }, [historyOpen]);

  const triggerSearch = useCallback(
    (
      pat: string,
      opts?: { isRegex?: boolean; caseSensitive?: boolean; isSemanticMode?: boolean; searchAll?: boolean },
      source: "user" | "reactive" = "reactive",
    ) => {
      const all = opts?.searchAll ?? searchAll;
      if (!pat.trim() || (!all && !directory)) return;
      const semantic = opts?.isSemanticMode ?? isSemanticMode;
      const query = buildQuery(pat, opts);
      const ready = all ? semanticReadyGlobally : semanticReady;
      if (semantic && !ready && !all) {
        deferSemanticSearch(query);
        ensureCurrentRootIndexed(source === "user").catch(console.error);
        return;
      }
      search(query);
    },
    [search, buildQuery, deferSemanticSearch, directory, ensureCurrentRootIndexed, isSemanticMode, searchAll, semanticReady, semanticReadyGlobally],
  );

  useEffect(() => {
    const key = `${selectedCollectionId ?? ""}|${selectedTagId ?? ""}`;
    if (prevFilterKey.current === key) return;
    prevFilterKey.current = key;
    if (replayQueryRef.current) return;
    if (pattern.trim()) triggerSearch(pattern, undefined, "user");
  }, [selectedCollectionId, selectedTagId]); // eslint-disable-line react-hooks/exhaustive-deps

  // Notify store when query presence changes
  useEffect(() => {
    setHasQuery(pattern.trim().length > 0);
  }, [pattern, setHasQuery]);

  // Debounce pattern changes
  useEffect(() => {
    if (replayQueryRef.current) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => triggerSearch(pattern, undefined, "user"), 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [pattern]); // eslint-disable-line react-hooks/exhaustive-deps

  // Replay updates several controls in one render. Clear the guard only after
  // their reactive effects have all observed it, preventing duplicate log rows.
  useEffect(() => {
    replayQueryRef.current = null;
  });

  // Re-trigger when externally-driven settings change (directory)
  useEffect(() => {
    if (!directory) {
      clearResults();
    } else if (pattern.trim()) {
      triggerSearch(pattern, undefined, "reactive");
    }
  }, [directory]); // eslint-disable-line react-hooks/exhaustive-deps

  // Auto-retry search once the index finishes building
  useEffect(() => {
    if (!prevSemanticReady.current && semanticReady && isSemanticMode && pattern.trim()) {
      triggerSearch(pattern);
    }
    prevSemanticReady.current = semanticReady;
  }, [semanticReady]); // eslint-disable-line react-hooks/exhaustive-deps

  const handleToggleRegex = () => {
    const next = !isRegex;
    setIsRegex(next);
    triggerSearch(pattern, { isRegex: next }, "user");
  };

  const handleToggleCaseSensitive = () => {
    const next = !caseSensitive;
    setCaseSensitive(next);
    triggerSearch(pattern, { caseSensitive: next }, "user");
  };

  const handleToggleSemantic = () => {
    const next = !isSemanticMode;
    setIsSemanticMode(next);
    setPreferSemantic(next);
    if (!next && semanticBuildRoot) {
      api.cancelEmbed().catch((e) => console.error("Cancel semantic index failed:", e));
    }
    if (!next || semanticReady) {
      triggerSearch(pattern, { isSemanticMode: next }, "user");
    } else if (pattern.trim()) {
      triggerSearch(pattern, { isSemanticMode: next }, "user");
    }
  };

  const handleToggleAll = () => {
    const next = !searchAll;
    setSearchAll(next);
    if (next) refreshGlobalStatus().catch(console.error);
    triggerSearch(pattern, { searchAll: next }, "user");
  };

  const handleResetSearch = () => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    setPattern("");
    clearResults();
    inputRef.current?.focus();
  };

  const replayHistory = (query: SearchQuery) => {
    const collectionId = collections.some((item) => item.id === query.collection_id)
      ? query.collection_id ?? null
      : null;
    const tagId = query.tag_ids?.find((id) => tags.some((tag) => tag.id === id)) ?? null;
    const replay = {
      ...query,
      collection_id: collectionId,
      tag_ids: tagId ? [tagId] : [],
    };
    replayQueryRef.current = replay;
    setPattern(query.pattern);
    setIsRegex(query.is_regex);
    setCaseSensitive(query.case_sensitive);
    setIsSemanticMode(query.mode === "Semantic");
    setSearchAll(query.scope.type === "all");
    setSelectedCollection(collectionId);
    setSelectedTag(tagId);
    search(replay);
    setHistoryOpen(false);
  };

  return (
    <div className="flex flex-col gap-2 p-3 border-b border-[var(--border-main)] bg-[var(--bg-app)]">
      {/* Top row: toggles + pattern */}
      <div className="flex items-center gap-2">
        <Toggle tooltip="Regular expression" active={isRegex} onToggle={handleToggleRegex}>
          <span className="font-mono text-[10px] w-4">.*</span>
        </Toggle>
        <Toggle tooltip="Case sensitive" active={caseSensitive} onToggle={handleToggleCaseSensitive}>
          <span className="text-[11px] font-bold tracking-tight">Aa</span>
        </Toggle>
        <Toggle
          tooltip={semanticReady ? "Semantic search" : "Set up semantic search in Settings"}
          active={isSemanticMode}
          onToggle={handleToggleSemantic}
          className="px-3 min-w-[100px]"
        >
          <div className="flex items-center gap-2">
            <div
              className={`w-3 h-3 rounded border flex items-center justify-center transition-colors ${
                isSemanticMode
                  ? "bg-white border-white text-[var(--accent-blue)]"
                  : "border-[var(--text-dim)]"
              }`}
            >
              {isSemanticMode && <Check size={10} strokeWidth={4} />}
            </div>
            <div className="flex items-center gap-1.5">
              <Database size={12} />
              <span className="text-[10px] font-bold uppercase tracking-wider">Semantic</span>
            </div>
          </div>
        </Toggle>

        {searching && (
          <span className="text-xs text-[var(--accent-blue)] animate-pulse flex items-center gap-1.5">
            <Search size={12} className="animate-spin" />
            <span>searching…</span>
          </span>
        )}

        <div className="flex flex-1 items-center rounded border border-[var(--border-main)] bg-[var(--bg-input)] transition-colors focus-within:ring-1 focus-within:ring-[var(--accent-blue)]">
          <div className="relative min-w-0 flex-shrink">
            <span
              aria-hidden="true"
              className="invisible block whitespace-pre py-1.5 pl-3 text-sm"
            >
              {pattern || "Search…"}
            </span>
            <input
              ref={inputRef}
              type="text"
              value={pattern}
              onChange={(e) => setPattern(e.target.value)}
              placeholder="Search…"
              className="absolute inset-0 min-w-0 w-full bg-transparent py-1.5 pl-3 text-sm text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
              spellCheck={false}
              autoFocus
            />
          </div>
          {pattern && (
            <Tooltip content="Clear search">
              <button
                type="button"
                onClick={handleResetSearch}
                aria-label="Clear search"
                className="inline-flex flex-shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
              >
                <X size={14} aria-hidden="true" />
              </button>
            </Tooltip>
          )}
          <div
            className="min-w-0 flex-1 self-stretch"
            aria-hidden="true"
            onMouseDown={(event) => {
              event.preventDefault();
              inputRef.current?.focus();
            }}
          />
          <Tooltip content={searchAll ? "Search current directory" : "Search all directories"}>
            <button
              type="button"
              onClick={handleToggleAll}
              aria-label={searchAll ? "Search current directory" : "Search all directories"}
              aria-pressed={searchAll}
              className={`mr-1 inline-flex flex-shrink-0 rounded p-1 transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] ${
                searchAll ? "bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]" : "text-[var(--text-dim)]"
              }`}
            >
              <Globe size={14} />
            </button>
          </Tooltip>
        </div>

        <div ref={historyRef} className="relative">
          <Tooltip content="Search history">
            <button
              type="button"
              aria-label="Search history"
              aria-expanded={historyOpen}
              onClick={() => {
                const next = !historyOpen;
                setHistoryOpen(next);
                if (next) loadHistory().catch(console.error);
              }}
              className={`rounded border p-2 transition-colors ${historyOpen ? "border-[var(--accent-blue)] text-[var(--accent-blue)]" : "border-[var(--border-main)] text-[var(--text-muted)] hover:text-[var(--text-main)]"}`}
            >
              <Clock size={14} />
            </button>
          </Tooltip>
          {historyOpen && (
            <div className="absolute right-0 top-full z-[900] mt-1 w-[min(520px,80vw)] overflow-hidden rounded-md border border-[var(--border-main)] bg-[var(--bg-app)] shadow-2xl">
              <div className="flex items-center border-b border-[var(--border-main)] px-3 py-2">
                <span className="flex-1 text-xs font-semibold text-[var(--text-main)]">Search history</span>
                {history.length > 0 && <button type="button" onClick={() => clearHistory().catch(console.error)} className="text-[10px] text-[var(--text-muted)] hover:text-[var(--text-main)]">Clear</button>}
              </div>
              <div className="max-h-80 overflow-auto p-1">
                {history.length === 0 && <p className="px-3 py-5 text-center text-xs text-[var(--text-dim)]">No searches yet</p>}
                {history.slice(0, 50).map((entry) => (
                  <div key={entry.id} className="group flex items-center gap-2 rounded px-2 py-1.5 hover:bg-[var(--bg-hover)]">
                    <button type="button" onClick={() => replayHistory(entry.query)} className="min-w-0 flex-1 text-left">
                      <div className="truncate text-xs text-[var(--text-main)]">{entry.query.pattern}</div>
                      <div className="truncate text-[10px] text-[var(--text-dim)]">{new Date(entry.started_at_ms).toLocaleString()} · {entry.query.mode} · {entry.result_count} matches · {entry.status}{entry.collection_name ? ` · ${entry.collection_name}` : ""}</div>
                    </button>
                    <button type="button" aria-label={`Delete search ${entry.query.pattern}`} onClick={() => deleteHistory(entry.id).catch(console.error)} className="p-1 text-[var(--text-dim)] opacity-0 hover:text-red-400 group-hover:opacity-100 focus:opacity-100"><Trash2 size={12} /></button>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        {settingsSlot}
      </div>

      {/* Bottom row: source controls */}
      <div className="flex items-center gap-2 flex-wrap">{sourceSlot}</div>
    </div>
  );
}

function Toggle({
  children,
  tooltip,
  active,
  disabled,
  onToggle,
  className = "min-w-[32px]",
}: {
  children: React.ReactNode;
  tooltip: string;
  active: boolean;
  disabled?: boolean;
  onToggle: () => void;
  className?: string;
}) {
  return (
    <Tooltip content={tooltip}>
      <button
        onClick={onToggle}
        disabled={disabled}
        className={`h-[32px] px-2 py-1 rounded text-xs font-mono font-semibold transition-all border flex items-center justify-center ${className} ${
          disabled
            ? "bg-[var(--bg-active)] text-[var(--text-dim)] border-transparent cursor-not-allowed"
            : active
              ? "bg-[var(--accent-blue)] text-white border-[var(--accent-blue)]"
              : "bg-[var(--bg-active)] text-[var(--text-muted)] border-[var(--border-main)] hover:text-[var(--text-main)] hover:border-[var(--border-strong)]"
        }`}
      >
        {children}
      </button>
    </Tooltip>
  );
}
