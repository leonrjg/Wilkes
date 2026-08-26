import type { ReactNode } from "react";
import type { DocumentSelection } from "./selection";

/** What the reader hands a selection slot so the host's chrome can behave. */
export interface SelectionSlotApi {
  /** Hide the chrome, leaving the text selected. */
  dismiss: () => void;
  /** Hide the chrome and clear the underlying selection. */
  clear: () => void;
  /** While pinned, the reader will not dismiss the chrome when the document
   *  selection collapses. Host chrome that takes focus (a text input, a
   *  submenu) must pin itself, or the act of focusing it destroys it. */
  setPinned: (pinned: boolean) => void;
}

/** Host chrome for the current selection. The reader positions it against the
 *  end of the selection and owns dismissal; the host owns the content. */
export type SelectionActionsSlot = (
  selection: DocumentSelection,
  api: SelectionSlotApi,
) => ReactNode;

/** Host content laid out beside one page, aligned to that page's top-left.
 *  The reader reserves no horizontal space for it: the column is positioned at
 *  the page's right edge and the host sizes its own content. */
export type PageGutterSlot = (
  page: number,
  ctx: { scale: number; width: number; height: number },
) => ReactNode;

export interface ReaderSlots {
  selectionActions?: SelectionActionsSlot;
  /** Extra controls, rendered inside the reader's floating control cluster
   *  ahead of the zoom controls. */
  toolbar?: ReactNode;
}

export interface PdfReaderSlots extends ReaderSlots {
  pageGutter?: PageGutterSlot;
}
