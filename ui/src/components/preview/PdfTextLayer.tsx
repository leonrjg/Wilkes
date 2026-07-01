import { useEffect, useRef } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { TextLayerBuilder } from "pdfjs-dist/web/pdf_viewer.mjs";
import { attachWebkitMarginSelection } from "./pdfWebkitSelection";
import "./pdfTextLayer.css";

// pdf.js' viewer-components build (`web/pdf_viewer.mjs`) reads the core library
// off `globalThis.pdfjsLib` at module-evaluation time. We must publish it there
// before that module is ever evaluated, then load the bundle lazily so the
// assignment is guaranteed to run first.
(globalThis as Record<string, unknown>).pdfjsLib ??= pdfjsLib;

let textLayerBuilderPromise: Promise<typeof TextLayerBuilder> | null = null;
function loadTextLayerBuilder(): Promise<typeof TextLayerBuilder> {
  textLayerBuilderPromise ??= import("pdfjs-dist/web/pdf_viewer.mjs").then(
    (m) => m.TextLayerBuilder,
  );
  return textLayerBuilderPromise;
}

interface Props {
  pdf: PDFDocumentProxy;
  pageNumber: number;
  /** CSS pixels per PDF unit, i.e. renderedWidth / unscaledPageWidth. */
  scale: number;
}

/**
 * Renders the selectable text overlay for a single page using pdf.js' own
 * `TextLayerBuilder` — the exact component the pdf.js viewer (and Zotero) use.
 *
 * We deliberately do NOT use react-pdf's `renderTextLayer`: react-pdf only
 * reimplements a stub of the viewer's text-layer glue and omits the
 * `selectionchange`-driven `endOfContent` management that keeps selection from
 * ballooning to the whole paragraph/page. `TextLayerBuilder` owns that logic
 * (its static global selection listener spans all mounted pages, including
 * virtualized ones) and is maintained upstream, so there is nothing for us to
 * hand-port. The canvas is still rendered by react-pdf's `<Page>`.
 */
export default function PdfTextLayer({ pdf, pageNumber, scale }: Props) {
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const wrapper = wrapperRef.current;
    if (!wrapper) return;

    let cancelled = false;
    let builder: TextLayerBuilder | null = null;
    let detachWebkitFix: (() => void) | null = null;

    Promise.all([loadTextLayerBuilder(), pdf.getPage(pageNumber)])
      .then(async ([TextLayerBuilderCtor, page]) => {
        if (cancelled) return;
        const viewport = page.getViewport({ scale });
        builder = new TextLayerBuilderCtor({ pdfPage: page });
        // pdf.js 5.x positions every span via calc(var(--total-scale-factor) * …px)
        // (4.x used --scale-factor); the viewer normally sets these on the page
        // div, so we set both here. --user-unit defaults to 1, so total == scale.
        builder.div.style.setProperty("--scale-factor", String(scale));
        builder.div.style.setProperty("--total-scale-factor", String(scale));
        await builder.render({ viewport });
        if (cancelled) {
          builder.cancel();
          return;
        }
        wrapper.append(builder.div);
        detachWebkitFix = attachWebkitMarginSelection(builder.div);
      })
      .catch((e) => {
        if (!cancelled) console.error(`PDF text layer (page ${pageNumber}) failed:`, e);
      });

    return () => {
      cancelled = true;
      detachWebkitFix?.();
      builder?.cancel();
      builder?.div.remove();
    };
  }, [pdf, pageNumber, scale]);

  return <div ref={wrapperRef} className="absolute inset-0" />;
}
