import { useEffect, useRef, useState } from "react";
import type { BoundingBox, ByteRange, SourceOrigin } from "../../lib/types";

export interface DocumentSelection {
  quote: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
  rects: BoundingBox[];
}

export interface PositionedSelection {
  selection: DocumentSelection;
  left: number;
  top: number;
}

interface SelectionActionsProps {
  positioned: PositionedSelection | null;
  onAddBookmark?: (selection: DocumentSelection) => void;
  showChatActions?: boolean;
  onExplain?: (selection: DocumentSelection) => void;
  onAsk?: (selection: DocumentSelection, question: string) => void;
  onDismiss: () => void;
  onClearSelection: () => void;
  dismissOnCollapsedDomSelection?: boolean;
}

export default function SelectionActions({
  positioned,
  onAddBookmark,
  showChatActions = false,
  onExplain,
  onAsk,
  onDismiss,
  onClearSelection,
  dismissOnCollapsedDomSelection = false,
}: SelectionActionsProps) {
  const [askDraft, setAskDraft] = useState("");
  const [isAskOpen, setIsAskOpen] = useState(false);
  const askInputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setAskDraft("");
    setIsAskOpen(false);
  }, [positioned?.selection]);

  useEffect(() => {
    if (isAskOpen) askInputRef.current?.focus();
  }, [isAskOpen]);

  useEffect(() => {
    if (!dismissOnCollapsedDomSelection) return;
    const handleSelectionChange = () => {
      if (isAskOpen) return;
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) onDismiss();
    };
    window.document.addEventListener("selectionchange", handleSelectionChange);
    return () => window.document.removeEventListener("selectionchange", handleSelectionChange);
  }, [dismissOnCollapsedDomSelection, isAskOpen, onDismiss]);

  if (!positioned) return null;
  if (!onAddBookmark && !(showChatActions && (onExplain || onAsk))) return null;

  const finish = () => {
    onDismiss();
    onClearSelection();
    setIsAskOpen(false);
    setAskDraft("");
  };

  return (
    <div
      onMouseDown={(event) => event.preventDefault()}
      className="absolute z-40 rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-xs text-[var(--text-main)] shadow-lg"
      style={{ left: positioned.left, top: positioned.top }}
    >
      {isAskOpen ? (
        <form
          className="flex items-center gap-1 p-1"
          onSubmit={(event) => {
            event.preventDefault();
            const question = askDraft.trim();
            if (!question || !onAsk) return;
            onAsk(positioned.selection, question);
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
            <button type="button" onClick={() => { onAddBookmark(positioned.selection); finish(); }} className="px-2 py-1 hover:bg-[var(--bg-active)]">
              Bookmark
            </button>
          )}
          {showChatActions && onExplain && (
            <button type="button" onClick={() => { onExplain(positioned.selection); finish(); }} className="px-2 py-1 border-l border-[var(--border-main)] hover:bg-[var(--bg-active)]">
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
