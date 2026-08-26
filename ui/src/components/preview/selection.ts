import type { BoundingBox, ByteRange, SourceOrigin } from "./documentCoordinates";

/** A run of text the reader resolved back to document coordinates.
 *
 *  Produced by every reader from its own substrate -- a DOM range over the PDF
 *  text layer, a CodeMirror range, a source-mapped markdown span -- and handed
 *  to the host, which decides what a selection is *for*. */
export interface DocumentSelection {
  quote: string;
  origin: SourceOrigin;
  text_range?: ByteRange;
  /** Page-relative rectangles, one per visual line. Empty for text readers,
   *  which have no page geometry to report. */
  rects: BoundingBox[];
}

/** A selection plus where the reader wants chrome for it drawn, in coordinates
 *  relative to the reader's own root element. */
export interface PositionedSelection {
  selection: DocumentSelection;
  left: number;
  top: number;
}
