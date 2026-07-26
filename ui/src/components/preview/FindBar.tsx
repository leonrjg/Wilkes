import { Search as SearchIcon, ChevronUp, ChevronDown, X } from "react-feather";
import type { DocumentFind } from "./useDocumentFind";

interface FindBarProps {
  find: DocumentFind;
  matchCount: number;
  /** Optional spinner shown while an async matcher (e.g. PDF) is still scanning. */
  isSearching?: boolean;
}

/**
 * Presentational find bar shared by every viewer that offers in-document find.
 * All state lives in the {@link DocumentFind} controller; this only renders it.
 */
export default function FindBar({ find, matchCount, isSearching = false }: FindBarProps) {
  return (
    <div className="bg-[var(--bg-app)] border border-[var(--border-main)] rounded-lg shadow-xl flex items-center p-1.5 gap-1.5 animate-in fade-in slide-in-from-top-2 duration-200">
      <div className="relative flex items-center pl-2 text-[var(--text-dim)]">
        <SearchIcon size={14} />
        <input
          ref={find.inputRef}
          type="text"
          placeholder="Find in document..."
          value={find.query}
          onChange={(e) => find.setQuery(e.target.value)}
          onKeyDown={find.onInputKeyDown}
          className="bg-transparent border-none outline-none px-2.5 py-1.5 text-sm text-[var(--text-main)] placeholder-[var(--text-dim)] w-56"
        />
      </div>
      {matchCount > 0 && (
        <span className="text-xs text-[var(--text-muted)] font-mono px-1">
          {find.currentIdx + 1}/{matchCount}
        </span>
      )}
      {isSearching && (
        <div className="w-3.5 h-3.5 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin mx-1" />
      )}
      <div className="flex border-l border-[var(--border-main)] ml-1 pl-1">
        <button
          onClick={find.prev}
          disabled={matchCount === 0}
          className="p-1.5 hover:bg-[var(--bg-active)] rounded disabled:opacity-30"
        >
          <ChevronUp size={16} />
        </button>
        <button
          onClick={find.next}
          disabled={matchCount === 0}
          className="p-1.5 hover:bg-[var(--bg-active)] rounded disabled:opacity-30"
        >
          <ChevronDown size={16} />
        </button>
        <button
          onClick={find.close}
          className="p-1.5 hover:bg-[var(--bg-active)] rounded text-[var(--text-dim)] hover:text-[var(--accent-red)]"
        >
          <X size={16} />
        </button>
      </div>
    </div>
  );
}
