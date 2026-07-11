import { useCallback, useState, type RefObject } from "react";
import type { DocumentSelection, PositionedSelection } from "./SelectionActions";

interface Options {
  rootRef: RefObject<HTMLElement | null>;
  mapSelection: (range: Range, selection: Selection) => DocumentSelection | null;
}

export function useDomDocumentSelection({ rootRef, mapSelection }: Options) {
  const [positioned, setPositioned] = useState<PositionedSelection | null>(null);

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

  return {
    positioned,
    readSelection,
    dismiss: () => setPositioned(null),
    clearSelection: () => window.getSelection()?.removeAllRanges(),
  };
}
