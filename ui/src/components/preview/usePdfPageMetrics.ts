import { useEffect, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";

export interface PdfPageMetric {
  width: number;
  height: number;
}

export function getScaledPageHeight(metric: PdfPageMetric, renderedWidth: number) {
  return (metric.height / metric.width) * renderedWidth;
}

export function usePdfPageMetrics(pdf: PDFDocumentProxy | null, url: string) {
  const [pageMetrics, setPageMetrics] = useState<PdfPageMetric[]>([]);
  const [isLoadingPageMetrics, setIsLoadingPageMetrics] = useState(false);

  useEffect(() => {
    let cancelled = false;

    setPageMetrics([]);
    setIsLoadingPageMetrics(Boolean(pdf));

    if (!pdf) {
      return () => {
        cancelled = true;
      };
    }

    const loadMetrics = async () => {
      try {
        const metrics = await Promise.all(
          Array.from({ length: pdf.numPages }, async (_, index) => {
            const page = await pdf.getPage(index + 1);
            const viewport = page.getViewport({ scale: 1 });
            return { width: viewport.width, height: viewport.height };
          }),
        );

        if (!cancelled) {
          setPageMetrics(metrics);
        }
      } catch (e) {
        console.error("Failed to load PDF page metrics:", e);
      } finally {
        if (!cancelled) {
          setIsLoadingPageMetrics(false);
        }
      }
    };

    loadMetrics();

    return () => {
      cancelled = true;
    };
  }, [pdf, url]);

  return {
    pageMetrics,
    isLoadingPageMetrics,
    hasPageMetrics: pdf !== null && pageMetrics.length === pdf.numPages,
  };
}
