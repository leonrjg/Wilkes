import { useEffect, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import {
  locatePdfSearchResult,
  type PdfSearchLocator,
  type PdfTextMatch,
} from "./pdfTextLocator";

/** Resolve a coarse indexed PDF origin to rendered PDF.js page geometry. */
export function usePdfSearchResult(
  pdf: PDFDocumentProxy | null,
  coarsePage: number,
  locator: PdfSearchLocator | null | undefined,
) {
  const [match, setMatch] = useState<PdfTextMatch | null>(null);

  useEffect(() => {
    setMatch(null);
    if (!pdf || !locator?.matched_text.trim()) {
      return;
    }

    const abort = new AbortController();
    locatePdfSearchResult(pdf, coarsePage, locator, abort.signal)
      .then((located) => {
        if (!abort.signal.aborted) setMatch(located);
      })
      .catch((error) => {
        if (!abort.signal.aborted) {
          console.error("PDF search result localization failed:", error);
        }
      });

    return () => abort.abort();
  }, [
    pdf,
    coarsePage,
    locator?.matched_text,
    locator?.context_before,
    locator?.context_after,
  ]);

  return match;
}
