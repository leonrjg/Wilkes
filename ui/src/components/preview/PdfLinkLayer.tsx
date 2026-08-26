import { useCallback, useEffect, useRef, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { PdfDestination } from "./pdfDestinations";
import {
  getPdfLinkPreview,
  type PdfLinkPreview,
} from "./pdfLinkPreview";
import { Tooltip } from "./Tooltip";

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

type PreviewState =
  | { status: "idle" | "loading" | "unavailable" | "failed" }
  | { status: "ready"; preview: PdfLinkPreview };

function PdfLinkOverlay({
  pdf,
  link,
  onNavigateToDestination,
  onOpenExternal,
}: {
  pdf: PDFDocumentProxy;
  link: LinkRect;
  onNavigateToDestination: (dest: PdfDestination) => void;
  onOpenExternal: (url: string) => void;
}) {
  const [previewState, setPreviewState] = useState<PreviewState>({
    status: "idle",
  });
  const requestGenerationRef = useRef(0);

  useEffect(
    () => () => {
      requestGenerationRef.current += 1;
    },
    [],
  );

  const loadPreview = useCallback(() => {
    if (!link.dest) return;
    if (
      previewState.status === "loading" ||
      previewState.status === "ready" ||
      previewState.status === "unavailable"
    ) {
      return;
    }

    const generation = ++requestGenerationRef.current;
    setPreviewState({ status: "loading" });
    getPdfLinkPreview(pdf, link.dest).then(
      (preview) => {
        if (requestGenerationRef.current !== generation) return;
        setPreviewState(
          preview
            ? { status: "ready", preview }
            : { status: "unavailable" },
        );
      },
      (error) => {
        if (requestGenerationRef.current !== generation) return;
        console.error("PDF link preview failed:", error);
        setPreviewState({ status: "failed" });
      },
    );
  }, [link.dest, pdf, previewState.status]);

  let tooltipContent;
  if (link.url) {
    tooltipContent = link.url;
  } else if (previewState.status === "ready") {
    tooltipContent = (
      <div className="space-y-1">
        <div className="font-medium text-[var(--text-dim)]">
          Page {previewState.preview.pageNumber}
        </div>
        <div className="whitespace-pre-line">
          {previewState.preview.text}
        </div>
      </div>
    );
  } else if (previewState.status === "unavailable") {
    tooltipContent = "No reliable text preview at this destination";
  } else if (previewState.status === "failed") {
    tooltipContent = "Link preview unavailable";
  } else {
    tooltipContent = "Loading link preview…";
  }

  const ariaLabel = link.url
    ? `Open ${link.url}`
    : previewState.status === "ready"
      ? `Go to page ${previewState.preview.pageNumber}: ${previewState.preview.text}`
      : "Follow internal PDF link";

  return (
    <Tooltip
      content={tooltipContent}
      className={link.url ? "break-all" : "text-left"}
      delayMs={link.dest ? 300 : 0}
      interactive={Boolean(link.dest)}
      size={link.dest ? "wide" : "default"}
    >
      <a
        data-testid="pdf-link"
        aria-label={ariaLabel}
        href={link.url ?? "#"}
        onMouseEnter={loadPreview}
        onFocus={loadPreview}
        onClick={(event) => {
          event.preventDefault();
          if (link.url) onOpenExternal(link.url);
          else if (link.dest) onNavigateToDestination(link.dest);
        }}
        style={{
          position: "absolute",
          // Links must remain the hit target when a persisted bookmark
          // highlight covers the same text (bookmark highlights use z-index 1).
          zIndex: 2,
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
  );
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
    setLinks([]);

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

          // Map the PDF-space rect (bottom-left origin) to top-left CSS pixels
          // at the render scale. pdf.js 6 dropped `convertToViewportRectangle`;
          // it was exactly this -- the same affine transform applied to the two
          // corners. They may come back unordered, so normalise below.
          const [x1, y1] = viewport.convertToViewportPoint(
            annotation.rect[0],
            annotation.rect[1],
          );
          const [x2, y2] = viewport.convertToViewportPoint(
            annotation.rect[2],
            annotation.rect[3],
          );
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
        <PdfLinkOverlay
          key={link.key}
          pdf={pdf}
          link={link}
          onNavigateToDestination={onNavigateToDestination}
          onOpenExternal={onOpenExternal}
        />
      ))}
    </>
  );
}
