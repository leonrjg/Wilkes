import { useEffect, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { BoundingBox } from "../../lib/types";

export interface InnerMatch {
  page: number;
  bbox: BoundingBox;
}

/**
 * Computes the PDF-specific match set for in-document find: it scans page text
 * for `query` and returns page-anchored bounding boxes. Find-bar state and match
 * navigation are owned by the shared {@link useDocumentFind} controller.
 */
export function usePdfInnerSearch(pdf: PDFDocumentProxy | null, query: string, isEnabled: boolean) {
  const [matches, setMatches] = useState<InnerMatch[]>([]);
  const [isSearching, setIsSearching] = useState(false);

  useEffect(() => {
    if (!isEnabled || !query.trim() || !pdf) {
      setMatches([]);
      setIsSearching(false);
      return;
    }

    const abort = new AbortController();

    const search = async () => {
      setIsSearching(true);
      const found: InnerMatch[] = [];
      const needle = query.toLowerCase();

      try {
        for (let i = 1; i <= pdf.numPages; i++) {
          if (abort.signal.aborted) return;
          const p = await pdf.getPage(i);
          const textContent = await p.getTextContent();

          for (const item of textContent.items) {
            if ("str" in item) {
              const text = item.str.toLowerCase();
              if (text.includes(needle)) {
                const [scX, _skY, _skX, scY, tx, ty] = item.transform;
                const vp = p.getViewport({ scale: 1 });
                found.push({
                  page: i,
                  bbox: {
                    x: tx,
                    y: vp.height - ty - scY,
                    width: item.width || text.length * scX * 0.6,
                    height: Math.abs(scY),
                  },
                });
              }
            }
          }
        }

        if (!abort.signal.aborted) setMatches(found);
      } catch (e) {
        console.error("PDF inner search failed:", e);
      } finally {
        if (!abort.signal.aborted) setIsSearching(false);
      }
    };

    const timeout = setTimeout(search, 300);
    return () => {
      abort.abort();
      clearTimeout(timeout);
    };
  }, [query, isEnabled, pdf]);

  return { matches, isSearching };
}
