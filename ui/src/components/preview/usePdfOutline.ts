import { useEffect, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { PdfDestination } from "./pdfDestinations";

export interface PdfOutlineNode {
  title: string;
  dest: PdfDestination | null;
  url: string | null;
  items: PdfOutlineNode[];
}

/** pdf.js outline item shape (subset we consume). */
interface RawOutlineItem {
  title: string;
  dest?: PdfDestination | null;
  url?: string | null;
  items?: RawOutlineItem[];
}

function normalize(items: RawOutlineItem[]): PdfOutlineNode[] {
  return items.map((item) => ({
    title: item.title,
    dest: item.dest ?? null,
    url: item.url ?? null,
    items: item.items ? normalize(item.items) : [],
  }));
}

/**
 * Loads the document outline (the PDF's own table of contents / bookmarks tree)
 * via `pdf.getOutline()`. Returns `null` while loading or when the document has
 * no outline, letting callers hide the TOC affordance entirely.
 */
export function usePdfOutline(pdf: PDFDocumentProxy | null): PdfOutlineNode[] | null {
  const [outline, setOutline] = useState<PdfOutlineNode[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    setOutline(null);
    if (!pdf) return;

    pdf
      .getOutline()
      .then((items) => {
        if (cancelled) return;
        setOutline(items && items.length > 0 ? normalize(items as RawOutlineItem[]) : null);
      })
      .catch((e) => {
        if (!cancelled) console.error("Failed to load PDF outline:", e);
      });

    return () => {
      cancelled = true;
    };
  }, [pdf]);

  return outline;
}
