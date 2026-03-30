import { useState, useCallback, useEffect, useRef } from "react";
import ExtensionFilter from "./ExtensionFilter";
import type { FileEntry, SearchQuery } from "../lib/types";

interface Props {
  onSearch: (query: SearchQuery) => void;
  searching: boolean;
  sourceSlot: React.ReactNode;
  directory?: string;
  respectGitignore?: boolean;
  maxFileSize?: number;
  contextLines?: number;
  fileList?: FileEntry[];
  excluded?: Set<string>;
  onExcludedChange?: (excluded: Set<string>) => void;
  onQueryChange?: (hasQuery: boolean) => void;
}

export default function SearchBar({
  onSearch,
  searching,
  sourceSlot,
  directory = "",
  respectGitignore = true,
  maxFileSize = 10 * 1024 * 1024,
  contextLines = 2,
  fileList = [],
  excluded = new Set<string>(),
  onExcludedChange,
  onQueryChange,
}: Props) {
  const [pattern, setPattern] = useState("");
  const [isRegex, setIsRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const buildQuery = useCallback(
    (pat: string): SearchQuery => {
      const allExtensions = [...new Set(fileList.map((f) => f.extension))];
      const file_type_filters = excluded.size === 0
        ? []
        : allExtensions.filter((ext) => !excluded.has(ext));
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
      };
    },
    [isRegex, caseSensitive, directory, excluded, fileList, respectGitignore, maxFileSize, contextLines],
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
    debounceRef.current = setTimeout(() => triggerSearch(pattern), 200);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [pattern, triggerSearch]);

  // Re-trigger when options change (if there's already a pattern)
  useEffect(() => {
    if (pattern.trim()) triggerSearch(pattern);
  }, [isRegex, caseSensitive, directory, excluded, fileList]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div className="flex flex-col gap-2 p-3 border-b border-neutral-800 bg-neutral-900">
      {/* Top row: toggles + pattern */}
      <div className="flex items-center gap-2">
        <Toggle
          label=".*"
          title="Regular expression"
          active={isRegex}
          onToggle={() => setIsRegex((v) => !v)}
        />
        <Toggle
          label="Aa"
          title="Case sensitive"
          active={caseSensitive}
          onToggle={() => setCaseSensitive((v) => !v)}
        />

        {searching && (
          <span className="text-xs text-blue-400 animate-pulse">searching…</span>
        )}

        <input
          type="text"
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
          placeholder="Search…"
          className="flex-1 bg-neutral-800 rounded px-3 py-1.5 text-sm outline-none focus:ring-1 focus:ring-blue-500 placeholder:text-neutral-500"
          spellCheck={false}
          autoFocus
        />
      </div>

      {/* Bottom row: source slot + extension filter */}
      <div className="flex items-center gap-2 flex-wrap">
        {sourceSlot}
        <ExtensionFilter fileList={fileList} excluded={excluded} onChange={onExcludedChange ?? (() => {})} />
      </div>
    </div>
  );
}

function Toggle({
  label,
  title,
  active,
  onToggle,
}: {
  label: string;
  title: string;
  active: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      onClick={onToggle}
      title={title}
      className={`px-2 py-1 rounded text-xs font-mono font-semibold transition-colors ${
        active
          ? "bg-blue-600 text-white"
          : "bg-neutral-800 text-neutral-400 hover:text-neutral-100"
      }`}
    >
      {label}
    </button>
  );
}
