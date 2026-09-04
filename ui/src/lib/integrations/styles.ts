/**
 * The control styles the integration panels share.
 *
 * Named here rather than repeated per panel because two components now render
 * the same kinds of control — a built-in provider's form and the custom
 * manifest editor — and a checkbox that looks different in one of them would
 * read as a different kind of switch.
 */

export const INPUT_CLASS =
  "w-full bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-2.5 py-1.5 text-xs text-[var(--text-main)] focus:outline-none focus:border-[var(--accent-blue)] transition-colors";

export const BUTTON_CLASS =
  "px-3 py-1.5 bg-[var(--accent-blue)] hover:bg-[var(--accent-blue-hover)] text-white text-[10px] font-bold uppercase tracking-wider rounded transition-colors disabled:opacity-50";

export const GHOST_BUTTON_CLASS =
  "px-2.5 py-1 border border-[var(--border-main)] text-[10px] uppercase tracking-wider rounded text-[var(--text-muted)] hover:text-[var(--text-main)] transition-colors disabled:opacity-50";

export const CHECKBOX_CLASS =
  "w-3.5 h-3.5 rounded border-[var(--border-strong)] bg-[var(--bg-input)] text-[var(--accent-blue)] focus:ring-[var(--accent-blue)] focus:ring-offset-[var(--bg-app)]";

export const FIELD_LABEL_CLASS = "text-xs text-[var(--text-muted)]";

export const ERROR_TEXT_CLASS = "text-xs text-[var(--accent-red,#f87171)]";
