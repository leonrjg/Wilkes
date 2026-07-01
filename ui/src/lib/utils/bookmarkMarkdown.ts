import type { Bookmark } from "../types";

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

export function toMarkdown(bookmark: Bookmark): string {
  const page = "PdfPage" in bookmark.origin ? bookmark.origin.PdfPage.page : null;
  const pageSuffix = page === null ? "" : `, p.${page}`;
  return `> ${bookmark.quote}\n\n- [${fileName(bookmark.path)}](${bookmark.path})${pageSuffix}`;
}
