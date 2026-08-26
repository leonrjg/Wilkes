/** Session-scoped reader positions for text documents. Positions are keyed by
 * document path and presentation mode so Source and Rendered Markdown views do
 * not overwrite one another. A normalized ratio survives viewport resizing. */
export type TextViewerMode = "source" | "rendered";

const positions = new Map<string, number>();
const markdownZooms = new Map<string, number>();

export const MARKDOWN_MIN_ZOOM = 0.6;
export const MARKDOWN_MAX_ZOOM = 2.5;

function key(path: string, mode: TextViewerMode): string {
  return `${path}\u0000${mode}`;
}

export function saveTextScrollPosition(path: string, mode: TextViewerMode, ratio: number): void {
  positions.set(key(path, mode), Math.min(Math.max(ratio, 0), 1));
}

export function readTextScrollPosition(path: string, mode: TextViewerMode): number | null {
  return positions.get(key(path, mode)) ?? null;
}

export function saveMarkdownZoom(path: string, zoom: number): void {
  markdownZooms.set(path, Math.min(Math.max(zoom, MARKDOWN_MIN_ZOOM), MARKDOWN_MAX_ZOOM));
}

export function readMarkdownZoom(path: string): number {
  return markdownZooms.get(path) ?? 1;
}
