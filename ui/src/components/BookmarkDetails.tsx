import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Trash2, X } from "react-feather";
import type { Bookmark } from "../lib/types";
import type { ElementAnchor } from "@leonrjg/wilkes-reader";

interface BookmarkDetailsProps {
  bookmark: Bookmark;
  anchor: ElementAnchor;
  deleting?: boolean;
  onClose: () => void;
  onDelete: () => void;
}

export default function BookmarkDetails({
  bookmark,
  anchor,
  deleting = false,
  onClose,
  onDelete,
}: BookmarkDetailsProps) {
  const cardRef = useRef<HTMLElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    const card = cardRef.current;
    if (!card) return;
    const margin = 12;
    const gap = 8;
    const { width, height } = card.getBoundingClientRect();
    const left = Math.min(
      Math.max(anchor.left, margin),
      Math.max(window.innerWidth - width - margin, margin),
    );
    const below = anchor.bottom + gap;
    const top = below + height <= window.innerHeight - margin
      ? below
      : Math.max(anchor.top - height - gap, margin);
    setPosition({ left, top });
  }, [anchor, bookmark.note, bookmark.quote]);

  useEffect(() => {
    const dismissOnOutsidePointer = (event: PointerEvent) => {
      if (event.target instanceof Node && !cardRef.current?.contains(event.target)) {
        onClose();
      }
    };
    const dismissOnScroll = () => onClose();
    document.addEventListener("pointerdown", dismissOnOutsidePointer);
    document.addEventListener("scroll", dismissOnScroll, true);
    return () => {
      document.removeEventListener("pointerdown", dismissOnOutsidePointer);
      document.removeEventListener("scroll", dismissOnScroll, true);
    };
  }, [onClose]);

  return (
    <aside
      ref={cardRef}
      aria-label="Bookmark details"
      style={{
        left: position?.left ?? anchor.left,
        top: position?.top ?? anchor.bottom + 8,
        visibility: position ? "visible" : "hidden",
      }}
      className="fixed z-40 max-h-[calc(100vh-1.5rem)] w-[min(22rem,calc(100vw-1.5rem))] overflow-auto rounded border border-[var(--border-strong)] bg-[var(--bg-sidebar)] p-3 shadow-lg"
    >
      <div className="flex items-start gap-2">
        <p className="min-w-0 flex-1 text-xs text-[var(--text-main)] line-clamp-3">
          {bookmark.quote}
        </p>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close bookmark details"
          className="flex-shrink-0 rounded p-1 text-[var(--text-dim)] hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
        >
          <X size={14} />
        </button>
      </div>

      <div className="mt-2 border-l-2 border-[var(--accent-blue)] pl-2 text-xs text-[var(--text-muted)] whitespace-pre-wrap">
        {bookmark.note?.trim() || <span className="italic text-[var(--text-dim)]">No note</span>}
      </div>

      <div className="mt-3 flex justify-end">
        <button
          type="button"
          onClick={onDelete}
          disabled={deleting}
          className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs text-[var(--text-error)] hover:bg-red-500/10 disabled:opacity-50"
        >
          <Trash2 size={13} />
          {deleting ? "Deleting…" : "Delete bookmark"}
        </button>
      </div>
    </aside>
  );
}
