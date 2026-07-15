export interface BookmarkAnchor {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

export type BookmarkOpenHandler = (bookmarkId: string, anchor: BookmarkAnchor) => void;

export function bookmarkAnchorFor(element: Element): BookmarkAnchor {
  const { left, top, right, bottom } = element.getBoundingClientRect();
  return { left, top, right, bottom };
}
