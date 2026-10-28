// Finding in the browser pane's page, with the platform's own find: a strip
// under the pane's header rather than a floating bar, since anything drawn
// over the page would be hidden under its native view. Opened by Cmd/Ctrl+F
// while the pane has focus; on Windows WebView2's own find bar answers that.
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Search, X } from "react-feather";
import { api } from "../services";
import type { BrowserFound } from "../services/api";

/** What a find turned up, as the strip says it. A platform that only says
 *  whether it found something leaves the highlighted match to say the rest. */
export function findStatus(found: BrowserFound | null): string {
  if (!found) return "";
  if (!found.found) return "No matches";
  if (found.matches === null) return "";
  return `${found.matches} match${found.matches === 1 ? "" : "es"}`;
}

const iconButton =
  "inline-flex shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-40 disabled:hover:bg-transparent";

export default function BrowserFindStrip({ requests, onClose }: { requests: number; onClose: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<BrowserFound | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Only the latest find may report: typing starts one per keystroke.
  const latest = useRef(0);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, [requests]);

  const find = async (text: string, backwards: boolean) => {
    const asked = ++latest.current;
    if (text === "") {
      setFound(null);
      setError(null);
      return;
    }
    try {
      const result = (await api.browserPaneFind?.(text, backwards)) ?? null;
      if (asked !== latest.current) return;
      setFound(result);
      setError(null);
    } catch (e) {
      console.error("browser pane: find failed", e);
      if (asked === latest.current) setError(String(e));
    }
  };

  const status = error ?? findStatus(found);

  return (
    <form
      className="flex items-center gap-1.5 border-b border-[var(--border-main)] px-2 py-1.5 text-[11px]"
      onSubmit={(event) => event.preventDefault()}
    >
      <Search size={12} className="shrink-0 text-[var(--text-dim)]" />
      <input
        ref={input}
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          void find(event.target.value, false);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            void find(query, event.shiftKey);
          } else if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
        placeholder="Find in page"
        aria-label="Find in page"
        spellCheck={false}
        className="min-w-0 flex-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]"
      />
      {status && (
        <span role="status" className={error ? "text-[var(--accent-red)]" : "text-[var(--text-muted)]"}>
          {status}
        </span>
      )}
      <button
        type="button"
        aria-label="Previous match"
        disabled={query === ""}
        onClick={() => void find(query, true)}
        className={iconButton}
      >
        <ChevronUp size={12} />
      </button>
      <button
        type="button"
        aria-label="Next match"
        disabled={query === ""}
        onClick={() => void find(query, false)}
        className={iconButton}
      >
        <ChevronDown size={12} />
      </button>
      <button type="button" aria-label="Close find" onClick={onClose} className={iconButton}>
        <X size={12} />
      </button>
    </form>
  );
}
