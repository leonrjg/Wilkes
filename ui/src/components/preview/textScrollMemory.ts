/** Session-scoped reader positions for text documents. Positions are keyed by
 * document path and presentation mode so Source and Rendered Markdown views do
 * not overwrite one another. A normalized ratio survives viewport resizing. */
export type TextViewerMode = "source" | "rendered";

const positions = new Map<string, number>();
const markdownModes = new Map<string, TextViewerMode>();

function key(path: string, mode: TextViewerMode): string {
  return `${path}\u0000${mode}`;
}

export function saveTextScrollPosition(path: string, mode: TextViewerMode, ratio: number): void {
  positions.set(key(path, mode), Math.min(Math.max(ratio, 0), 1));
}

export function readTextScrollPosition(path: string, mode: TextViewerMode): number | null {
  return positions.get(key(path, mode)) ?? null;
}

export function saveMarkdownViewMode(path: string, mode: TextViewerMode): void {
  markdownModes.set(path, mode);
}

export function readMarkdownViewMode(path: string): TextViewerMode {
  return markdownModes.get(path) ?? "source";
}
