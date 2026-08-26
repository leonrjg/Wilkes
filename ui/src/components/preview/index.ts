/**
 * The readers' public surface.
 *
 * Everything re-exported here is API: it is what a host is allowed to depend
 * on, and what cannot be changed without changing the host. Everything else in
 * this directory is internal and may be reshaped freely -- the text and link
 * layers, the find bar, the zoom cluster, the scroll memories, the WebKit
 * selection workaround, the markdown source map, the selection plumbing.
 *
 * Two tiers, because there are two ways to use these readers:
 *
 *   1. **Composed.** Mount a reader and drive it through the contract:
 *      `decorations` for host-owned marks, `slots` for host chrome, a `ref` for
 *      navigation, and `ReaderHostProvider` for the capabilities the readers
 *      need from the application around them.
 *
 *   2. **Headless.** Take the hooks and build your own surface. This is for a
 *      host whose reading surface is genuinely not a reader -- an annotated
 *      single-page stage, a thumbnail strip -- and which would otherwise
 *      reimplement document loading, page metrics and text location badly. It
 *      shares the engine without pretending to share the shell.
 */

// ── Tier 1: composed readers ────────────────────────────────────────────────

export { default as PdfViewer } from "./PdfViewer";
export type { PdfViewerProps, PdfReaderHandle, PdfSelection } from "./PdfViewer";

export { default as MarkdownViewer } from "./MarkdownViewer";
export type { MarkdownViewerProps, MarkdownReaderHandle } from "./MarkdownViewer";

export { default as CodeViewer, getLanguageExtension } from "./CodeViewer";
export type { CodeViewerProps, CodeReaderHandle } from "./CodeViewer";

// ── The contract ────────────────────────────────────────────────────────────

export { elementAnchor, rectDecorationsForPage, rangeDecorations, unionBox } from "./decorations";
export type {
  Decoration,
  DecorationAnchor,
  DecorationRenderContext,
  DecorationActivateHandler,
  ElementAnchor,
} from "./decorations";

export type {
  ReaderSlots,
  PdfReaderSlots,
  SelectionActionsSlot,
  PageGutterSlot,
  SelectionSlotApi,
} from "./slots";

export type { ReaderHandle, FindableReaderHandle, ZoomableReaderHandle } from "./readerHandle";

export type { DocumentSelection, PositionedSelection } from "./selection";

export { ReaderHostProvider, useReaderHost } from "./ReaderHost";
export type { ReaderHostServices } from "./ReaderHost";

// ── Tier 2: headless ────────────────────────────────────────────────────────

export { usePdfDocument, loadPdfDocument, peekCachedPdfDocument } from "./pdfDocumentCache";
export { usePdfPageMetrics, getScaledPageHeight } from "./usePdfPageMetrics";
export type { PdfPageMetric } from "./usePdfPageMetrics";
export { usePdfOutline } from "./usePdfOutline";
export type { PdfOutlineNode } from "./usePdfOutline";
export { usePdfInnerSearch } from "./usePdfInnerSearch";
export type { InnerMatch } from "./usePdfInnerSearch";
export { useDocumentFind } from "./useDocumentFind";
export type { DocumentFind } from "./useDocumentFind";
export { resolveDestination } from "./pdfDestinations";
export type { PdfDestination, ResolvedDestination } from "./pdfDestinations";
export {
  findAllPdfTextMatches,
  findPdfTextMatchesOnPage,
  locatePdfSearchResult,
} from "./pdfTextLocator";
export type { PdfSearchLocator, PdfTextMatch } from "./pdfTextLocator";
export {
  utf8ByteOffsetToUtf16Offset,
  utf8ByteRangeToUtf16Range,
  textSelectionFromUtf16Range,
} from "./textOffsets";
