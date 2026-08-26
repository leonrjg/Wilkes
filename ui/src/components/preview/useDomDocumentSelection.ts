import { useCallback, useEffect, useState, type RefObject } from "react";
import type { DocumentSelection, PositionedSelection } from "./selection";
import { useSelectionSlot } from "./selectionSlot";

interface Options {
  rootRef: RefObject<HTMLElement | null>;
  mapSelection: (range: Range, selection: Selection) => DocumentSelection | null;
  /** Dismiss the selection chrome as soon as the document selection collapses.
   *  Suspended while the host has pinned itself. */
  dismissOnCollapsedSelection?: boolean;
}

export function useDomDocumentSelection({
  rootRef,
  mapSelection,
  dismissOnCollapsedSelection = false,
}: Options) {
  const [positioned, setPositioned] = useState<PositionedSelection | null>(null);

  const dismiss = useCallback(() => setPositioned(null), []);
  const clearSelection = useCallback(() => window.getSelection()?.removeAllRanges(), []);
  const { api, pinnedRef } = useSelectionSlot({ dismiss, clear: clearSelection });

  const readSelection = useCallback(() => {
    const root = rootRef.current;
    const selection = window.getSelection();
    if (!root || !selection || selection.isCollapsed || selection.rangeCount === 0) {
      setPositioned(null);
      return;
    }
    const range = selection.getRangeAt(0);
    if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) {
      setPositioned(null);
      return;
    }
    const mapped = mapSelection(range, selection);
    const rect = range.getBoundingClientRect();
    if (!mapped || !mapped.quote || rect.width <= 0 || rect.height <= 0) {
      setPositioned(null);
      return;
    }
    const clientRects = Array.from(range.getClientRects());
    const endRect = clientRects[clientRects.length - 1] ?? rect;
    const rootRect = root.getBoundingClientRect();
    setPositioned({
      selection: mapped,
      left: Math.min(Math.max(endRect.right - rootRect.left, 8), Math.max(rootRect.width - 128, 8)),
      top: Math.min(Math.max(endRect.bottom - rootRect.top + 3, 8), Math.max(rootRect.height - 40, 8)),
    });
  }, [mapSelection, rootRef]);

  useEffect(() => {
    if (!dismissOnCollapsedSelection) return;
    const handleSelectionChange = () => {
      if (pinnedRef.current) return;
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || selection.rangeCount === 0) dismiss();
    };
    window.document.addEventListener("selectionchange", handleSelectionChange);
    return () => window.document.removeEventListener("selectionchange", handleSelectionChange);
  }, [dismissOnCollapsedSelection, dismiss, pinnedRef]);

  return { positioned, readSelection, dismiss, clearSelection, slotApi: api };
}
