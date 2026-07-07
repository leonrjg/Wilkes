import { useEffect, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { PdfDestination } from "./pdfDestinations";
import { Tooltip } from "../Tooltip";
import "./pdfLinkLayer.css";

interface LinkRect {
  key: string;
  left: number;
  top: number;
  width: number;
  height: number;
  dest: PdfDestination | null;
  url: string | null;
}

interface Props {
  pdf: PDFDocumentProxy;
  pageNumber: number;
  /** CSS pixels per PDF unit, i.e. renderedWidth / unscaledPageWidth. */
  scale: number;
  /** Navigate to an in-document GoTo destination. */
  onNavigateToDestination: (dest: PdfDestination) => void;
  /** Open an external URL (http/https) referenced by a link annotation. */
  onOpenExternal: (url: string) => void;
}

/**
 * Renders clickable overlays for a page's Link annotations — the within-document
 * links (table-of-contents entries, cross-references) and external URLs that OS
 * readers make navigable. Positioned above the text layer so links win the click;
 * everything else stays selectable.
 *
 * Mirrors PdfTextLayer's lifecycle: annotations are fetched per page and the
 * overlay boxes are derived from the annotation rects via the page viewport, so
 * coordinates already match the rendered scale.
 */
export default function PdfLinkLayer({
  pdf,
  pageNumber,
  scale,
  onNavigateToDestination,
  onOpenExternal,
}: Props) {
  const [links, setLinks] = useState<LinkRect[]>([]);

  useEffect(() => {
    let cancelled = false;

    pdf
      .getPage(pageNumber)
      .then(async (page) => {
        const annotations = await page.getAnnotations();
        if (cancelled) return;
        const viewport = page.getViewport({ scale });

        const rects: LinkRect[] = [];
        for (const [index, annotation] of annotations.entries()) {
          if (annotation.subtype !== "Link") continue;
          const dest = (annotation.dest ?? null) as PdfDestination | null;
          const url = (annotation.url ?? null) as string | null;
          // Only annotations that actually navigate somewhere are clickable.
          if (!dest && !url) continue;

          // convertToViewportRectangle maps the PDF-space rect (bottom-left
          // origin) to top-left CSS pixels at the render scale; corners may come
          // back unordered, so normalise to left/top/width/height.
          const [x1, y1, x2, y2] = viewport.convertToViewportRectangle(annotation.rect);
          rects.push({
            key: `${index}`,
            left: Math.min(x1, x2),
            top: Math.min(y1, y2),
            width: Math.abs(x2 - x1),
            height: Math.abs(y2 - y1),
            dest,
            url,
          });
        }

        if (!cancelled) setLinks(rects);
      })
      .catch((e) => {
        if (!cancelled) console.error(`PDF link layer (page ${pageNumber}) failed:`, e);
      });

    return () => {
      cancelled = true;
    };
  }, [pdf, pageNumber, scale]);

  return (
    <>
      {links.map((link) => (
        <Tooltip key={link.key} content={link.url} className="break-all">
          <a
            data-testid="pdf-link"
            href={link.url ?? "#"}
            onClick={(event) => {
              event.preventDefault();
              if (link.url) onOpenExternal(link.url);
              else if (link.dest) onNavigateToDestination(link.dest);
            }}
            style={{
              position: "absolute",
              left: `${link.left}px`,
              top: `${link.top}px`,
              width: `${Math.max(link.width, 4)}px`,
              height: `${Math.max(link.height, 4)}px`,
              cursor: "pointer",
              // Transparent hit target; a faint tint appears on hover via CSS below.
            }}
            className="pdf-link-overlay"
          />
        </Tooltip>
      ))}
    </>
  );
}
