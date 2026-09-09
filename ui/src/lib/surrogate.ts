import type { PdfOutlineNode, PageLinks } from "@leonrjg/wilkes-reader";
import type { SurrogateOutline, SurrogatePageLinks } from "./types";

/**
 * A book's outline and links, in the vocabulary the reader speaks.
 *
 * The backend resolved both against the document *it* laid out, and the reader
 * is drawing a rendering of that document — so every destination is already a
 * page number, and none of them can be a pdf.js destination, which names its
 * page by a reference into a document pdf.js parsed. `HostDestination` is the
 * reader's shape for exactly this case.
 *
 * The conversion is here rather than on the wire because the two sides name
 * things differently by convention — `offset_y` in Rust, `offsetY` in the
 * reader — and one place to cross that is better than a serde attribute that
 * makes the Rust type read like TypeScript.
 */

export function readerOutline(entries: SurrogateOutline[]): PdfOutlineNode[] {
  return entries.map((entry) => ({
    title: entry.title,
    dest: entry.page != null ? { page: entry.page, offsetY: entry.offset_y ?? null } : null,
    url: entry.url ?? null,
    items: readerOutline(entry.items),
  }));
}

export function readerLinks(pages: SurrogatePageLinks[]): PageLinks[] {
  return pages.map((page) => ({
    page: page.page,
    links: page.links.map((link) => ({
      bbox: link.bbox,
      page: link.page ?? null,
      offsetY: link.offset_y ?? null,
      url: link.url ?? null,
    })),
  }));
}
