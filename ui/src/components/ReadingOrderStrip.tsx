import { useEffect, useState } from "react";
import { RefreshCw, X } from "react-feather";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { readingOrderCounts } from "../lib/readingOrder";
import { useReadingOrderStore } from "../stores/useReadingOrderStore";

/** Subjects offered as one-click themes. More than a line or two of them is a
 *  list to read, and any other phrase can be typed. */
const OFFERED_SUBJECTS = 8;

/**
 * What the file list says about itself while it is in reading order: how many
 * documents were ordered and how many were not, and a theme to rank them by
 * instead.
 *
 * A theme is one of the folder's own subjects or any short phrase. With one,
 * the list puts the documents that discuss it most first, and each row still
 * says what it builds on, so the foundations a theme does not need can be
 * told from the ones it does.
 */
export default function ReadingOrderStrip() {
  const order = useReadingOrderStore((s) => s.order);
  const loading = useReadingOrderStore((s) => s.loading);
  const error = useReadingOrderStore((s) => s.error);
  const theme = useReadingOrderStore((s) => s.theme);
  const setTheme = useReadingOrderStore((s) => s.setTheme);
  const refresh = useReadingOrderStore((s) => s.refresh);
  const [draft, setDraft] = useState(theme);

  useEffect(() => setDraft(theme), [theme]);

  const status = (() => {
    if (error) return error;
    if (!order) return loading ? "Working out the reading order…" : "";
    const counts = readingOrderCounts(order);
    if (order.theme) {
      return `${counts.discussing} of ${order.documents.length} discuss ${order.theme}`;
    }
    if (counts.ordered === 0) {
      return "No document here names another's subject, so there is no order to give.";
    }
    return [
      `${counts.ordered} in order`,
      counts.any_time > 0 ? `${counts.any_time} at any time` : null,
      counts.not_placed > 0 ? `${counts.not_placed} not placed` : null,
    ].filter(Boolean).join(" · ");
  })();

  const subjects = order?.subjects.slice(0, OFFERED_SUBJECTS) ?? [];
  const activeTheme = theme.trim().toLowerCase();

  return (
    <div
      className="flex flex-shrink-0 flex-col gap-1 border-b border-[var(--border-main)] px-2 py-1.5 text-[11px] text-[var(--text-muted)]"
      aria-label="Reading order"
    >
      <div className="flex min-w-0 items-center gap-1">
        <span
          className={`min-w-0 flex-1 truncate ${error ? "text-[var(--text-error)]" : ""}`}
          role={error ? "alert" : "status"}
        >
          {status}
        </span>
        <Tooltip content="Work out the order again">
          <button
            type="button"
            aria-label="Work out the reading order again"
            disabled={loading}
            onClick={() => void refresh()}
            className="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)] disabled:opacity-50"
          >
            <RefreshCw size={11} aria-hidden="true" className={loading ? "animate-spin" : ""} />
          </button>
        </Tooltip>
      </div>
      <form
        className="flex min-w-0 items-center gap-1"
        onSubmit={(event) => {
          event.preventDefault();
          void setTheme(draft.trim());
        }}
      >
        <input
          type="text"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder="Rank by a theme…"
          aria-label="Theme to rank the documents by"
          className="min-w-0 flex-1 rounded border border-[var(--border-main)] bg-transparent px-1.5 py-0.5 text-[11px] text-[var(--text-main)] outline-none placeholder-[var(--text-dim)] focus:border-[var(--accent-blue)]"
        />
        {theme && (
          <button
            type="button"
            aria-label="Clear theme"
            onClick={() => void setTheme("")}
            className="flex h-5 w-5 flex-shrink-0 items-center justify-center rounded text-[var(--text-dim)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)]"
          >
            <X size={12} aria-hidden="true" />
          </button>
        )}
      </form>
      {subjects.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {subjects.map((subject) => {
            const active = subject.name === activeTheme;
            return (
              <Tooltip
                key={subject.name}
                content={`${subject.documents} document${subject.documents === 1 ? "" : "s"} here ${subject.documents === 1 ? "has or names" : "have or name"} it`}
              >
                <button
                  type="button"
                  aria-label={`Rank by ${subject.name}`}
                  aria-pressed={active}
                  onClick={() => void setTheme(active ? "" : subject.name)}
                  className={`rounded-full border px-1.5 py-0.5 text-[10px] ${
                    active
                      ? "border-[var(--accent-blue)] bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]"
                      : "border-[var(--border-main)] text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)]"
                  }`}
                >
                  {subject.name}
                </button>
              </Tooltip>
            );
          })}
        </div>
      )}
    </div>
  );
}
