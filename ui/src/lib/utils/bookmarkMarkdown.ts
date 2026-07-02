import type { Bookmark } from "../types";

function fileName(path: string) {
  return path.split(/[/\\]/).pop() || path;
}

export function toMarkdown(bookmark: Bookmark): string {
  const page = "PdfPage" in bookmark.origin ? bookmark.origin.PdfPage.page : null;
  const pageSuffix = page === null ? "" : `, p.${page}`;
  const note = bookmark.note?.trim();
  const noteBlock = note ? `\n\n${note}` : "";
  return `> ${bookmark.quote}${noteBlock}\n\n- [${fileName(bookmark.path)}](${bookmark.path})${pageSuffix}`;
}
