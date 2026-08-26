import { useEffect, useRef, useState } from "react";
import type { DocumentSelection } from "./preview/selection";
import type { SelectionSlotApi } from "./preview/slots";

interface SelectionActionsProps {
  selection: DocumentSelection;
  api: SelectionSlotApi;
  onAddBookmark?: (selection: DocumentSelection) => void;
  showChatActions?: boolean;
  onExplain?: (selection: DocumentSelection) => void;
  onAsk?: (selection: DocumentSelection, question: string) => void;
}

/**
 * Wilkes' own selection chrome, passed to a reader through the
 * `slots.selectionActions` slot.
 *
 * It lives outside `preview/` deliberately: "Bookmark", "Explain" and "Ask
 * about this" are this application's offer, not a reading affordance, and the
 * readers must not ship them to a host that has no chat and no bookmarks.
 *
 * It is no longer positioned here. Where a selection popover belongs is a fact
 * about the reader's geometry, which only the reader knows; what it offers is a
 * fact about the application, which only the host knows. The reader hands over
 * a positioned box and this fills it.
 */
export default function SelectionActions({
  selection,
  api,
  onAddBookmark,
  showChatActions = false,
  onExplain,
  onAsk,
}: SelectionActionsProps) {
  const [askDraft, setAskDraft] = useState("");
  const [isAskOpen, setIsAskOpen] = useState(false);
  const askInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setAskDraft("");
    setIsAskOpen(false);
  }, [selection]);

  useEffect(() => {
    if (isAskOpen) askInputRef.current?.focus();
  }, [isAskOpen]);

  // Focusing the question input collapses the document selection. Tell the
  // reader to hold the popover open across that, or typing a question destroys
  // the thing being typed into.
  useEffect(() => {
    api.setPinned(isAskOpen);
    return () => api.setPinned(false);
  }, [api, isAskOpen]);

  if (!onAddBookmark && !(showChatActions && (onExplain || onAsk))) return null;

  const finish = () => {
    setIsAskOpen(false);
    setAskDraft("");
    api.clear();
    api.dismiss();
  };

  return (
    <div
      onMouseDown={(event) => event.preventDefault()}
      className="rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-xs text-[var(--text-main)] shadow-lg"
    >
      {isAskOpen ? (
        <form
          className="flex items-center gap-1 p-1"
          onSubmit={(event) => {
            event.preventDefault();
            const question = askDraft.trim();
            if (!question || !onAsk) return;
            onAsk(selection, question);
            finish();
          }}
        >
          <input
            ref={askInputRef}
            value={askDraft}
            onChange={(event) => setAskDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault();
                setIsAskOpen(false);
                setAskDraft("");
              }
            }}
            placeholder="Ask about this…"
            className="w-48 bg-[var(--bg-input)] border border-[var(--border-main)] rounded px-1.5 py-0.5 text-xs outline-none focus:border-[var(--accent-blue)]"
          />
          <button type="submit" disabled={!askDraft.trim()} className="px-1.5 py-0.5 rounded bg-[var(--accent-blue)] text-white disabled:opacity-40">
            Send
          </button>
          <button type="button" onClick={() => { setIsAskOpen(false); setAskDraft(""); }} className="px-1.5 py-0.5 rounded hover:bg-[var(--bg-active)]">
            Cancel
          </button>
        </form>
      ) : (
        <div className="flex items-center">
          {onAddBookmark && (
            <button type="button" onClick={() => { onAddBookmark(selection); finish(); }} className="px-2 py-1 hover:bg-[var(--bg-active)]">
              Bookmark
            </button>
          )}
          {showChatActions && onExplain && (
            <button type="button" onClick={() => { onExplain(selection); finish(); }} className="px-2 py-1 border-l border-[var(--border-main)] hover:bg-[var(--bg-active)]">
              Explain
            </button>
          )}
          {showChatActions && onAsk && (
            <button type="button" onClick={() => setIsAskOpen(true)} className="px-2 py-1 border-l border-[var(--border-main)] hover:bg-[var(--bg-active)]">
              Ask about this
            </button>
          )}
        </div>
      )}
    </div>
  );
}
