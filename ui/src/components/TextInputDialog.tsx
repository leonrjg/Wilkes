import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

interface TextInputDialogProps {
  open: boolean;
  title: string;
  label: string;
  initialValue: string;
  confirmLabel: string;
  busy?: boolean;
  onCancel: () => void;
  onSubmit: (value: string) => void;
}

export function TextInputDialog({
  open,
  title,
  label,
  initialValue,
  confirmLabel,
  busy = false,
  onCancel,
  onSubmit,
}: TextInputDialogProps) {
  const titleId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(initialValue);

  useLayoutEffect(() => {
    if (!open) return;
    setValue(initialValue);
    const frame = requestAnimationFrame(() => inputRef.current?.select());
    return () => cancelAnimationFrame(frame);
  }, [initialValue, open]);

  if (!open) return null;

  const trimmedValue = value.trim();

  return createPortal(
    <div
      className="fixed inset-0 z-[1100] flex items-center justify-center bg-black/35 px-4"
      onMouseDown={(event) => {
        if (!busy && event.target === event.currentTarget) onCancel();
      }}
    >
      <form
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onSubmit={(event) => {
          event.preventDefault();
          if (trimmedValue && !busy) onSubmit(trimmedValue);
        }}
        className="w-full max-w-sm rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] p-3 shadow-2xl"
      >
        <div id={titleId} className="mb-2 text-sm font-semibold text-[var(--text-main)]">
          {title}
        </div>
        <input
          ref={inputRef}
          aria-label={label}
          value={value}
          disabled={busy}
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape" && !busy) {
              event.preventDefault();
              onCancel();
            }
          }}
          className="mb-3 h-8 w-full rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-2 text-sm text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)] disabled:opacity-50"
        />
        <div className="flex justify-end gap-2">
          <button
            type="button"
            disabled={busy}
            onClick={onCancel}
            className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={!trimmedValue || busy}
            className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
          >
            {busy ? `${confirmLabel}...` : confirmLabel}
          </button>
        </div>
      </form>
    </div>,
    document.body,
  );
}
