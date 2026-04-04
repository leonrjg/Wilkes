import { useState, useCallback, useEffect, useRef } from "react";
import { Search, Database, Check } from "react-feather";
import type { FileEntry, SearchQuery } from "../lib/types";

interface Props {
  onSearch: (query: SearchQuery) => void;
  searching: boolean;
  sourceSlot: React.ReactNode;
  settingsSlot?: React.ReactNode;
  semanticReady?: boolean;
  directory?: string;
  respectGitignore?: boolean;
  maxFileSize?: number;
  contextLines?: number;
  supportedExtensions?: string[];
  fileList?: FileEntry[];
  excluded?: Set<string>;
  onExcludedChange?: (excluded: Set<string>) => void;
  onQueryChange?: (hasQuery: boolean) => void;
  initialSemanticMode?: boolean;
  onSemanticModeChange?: (active: boolean) => void;
}

export default function SearchBar({
  onSearch,
  searching,
  sourceSlot,
  settingsSlot,
  semanticReady = false,
  directory = "",
  respectGitignore = true,
  maxFileSize = 10 * 1024 * 1024,
  contextLines = 2,
  supportedExtensions = [],
  fileList = [],
  excluded = new Set<string>(),
  onQueryChange,
  initialSemanticMode = false,
  onSemanticModeChange,
}: Props) {
  const [pattern, setPattern] = useState("");
  const [isRegex, setIsRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [isSemanticMode, setIsSemanticMode] = useState(initialSemanticMode);

  // Sync state if initial prop changes (e.g. on load)
  useEffect(() => {
    setIsSemanticMode(initialSemanticMode);
  }, [initialSemanticMode]);

  const handleToggleSemantic = () => {
    const next = !isSemanticMode;
    setIsSemanticMode(next);
    onSemanticModeChange?.(next);
  };
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const buildQuery = useCallback(
    (pat: string): SearchQuery => {
      const allExtensions = [...new Set(fileList.map((f) => f.extension))];
      const file_type_filters =
        excluded.size === 0 ? [] : allExtensions.filter((ext) => !excluded.has(ext));
      return {
        pattern: pat,
        is_regex: isRegex,
        case_sensitive: caseSensitive,
        root: directory || ".",
        file_type_filters,
        max_results: 0,
        respect_gitignore: respectGitignore,
        max_file_size: maxFileSize,
        context_lines: contextLines,
        mode: isSemanticMode ? "Semantic" : "Grep",
        supported_extensions: supportedExtensions,
      };
    },
    [isRegex, caseSensitive, directory, excluded, fileList, respectGitignore, maxFileSize, contextLines, isSemanticMode, supportedExtensions],
  );

  const triggerSearch = useCallback(
    (pat: string) => {
      if (!pat.trim()) return;
      onSearch(buildQuery(pat));
    },
    [onSearch, buildQuery],
  );

  // Notify parent when query presence changes
  useEffect(() => {
    onQueryChange?.(pattern.trim().length > 0);
  }, [pattern]); // eslint-disable-line react-hooks/exhaustive-deps

  // Debounce pattern changes
  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => triggerSearch(pattern), 300);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [pattern, triggerSearch]);

  // Re-trigger when options change (if there's already a pattern)
  useEffect(() => {
    if (pattern.trim()) triggerSearch(pattern);
  }, [isRegex, caseSensitive, directory, excluded, isSemanticMode]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="flex flex-col gap-2 p-3 border-b border-[var(--border-main)] bg-[var(--bg-app)]">
      {/* Top row: toggles + pattern */}
      <div className="flex items-center gap-2">
        <Toggle
          title="Regular expression"
          active={isRegex}
          onToggle={() => setIsRegex((v) => !v)}
        >
          <span className="font-mono text-[10px] w-4">.*</span>
        </Toggle>
        <Toggle
          title="Case sensitive"
          active={caseSensitive}
          onToggle={() => setCaseSensitive((v) => !v)}
        >
          <span className="text-[11px] font-bold tracking-tight">Aa</span>
        </Toggle>
        <Toggle
          title={semanticReady ? "Semantic search" : "Set up semantic search in Settings"}
          active={isSemanticMode}
          onToggle={handleToggleSemantic}
          className="px-3 min-w-[100px]"
        >
          <div className="flex items-center gap-2">
            <div className={`w-3 h-3 rounded border flex items-center justify-center transition-colors ${
              isSemanticMode ? "bg-white border-white text-[var(--accent-blue)]" : "border-[var(--text-dim)]"
            }`}>
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

        <input
          type="text"
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
          placeholder="Search…"
          className="flex-1 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-3 py-1.5 text-sm outline-none focus:ring-1 focus:ring-[var(--accent-blue)] placeholder:text-[var(--text-dim)] text-[var(--text-main)] transition-colors"
          spellCheck={false}
          autoFocus
        />

        {settingsSlot}
      </div>

      {/* Bottom row: source slot */}
      <div className="flex items-center gap-2 flex-wrap">
        {sourceSlot}
      </div>
    </div>
  );
}

function Toggle({
  children,
  title,
  active,
  disabled,
  onToggle,
  className = "min-w-[32px]",
}: {
  children: React.ReactNode;
  title: string;
  active: boolean;
  disabled?: boolean;
  onToggle: () => void;
  className?: string;
}) {
  return (
    <button
      onClick={onToggle}
      title={title}
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
  );
}
