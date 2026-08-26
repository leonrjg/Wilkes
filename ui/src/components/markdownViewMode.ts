import type { TextViewerMode } from "./preview/textScrollMemory";

/**
 * Which viewer Wilkes last showed a Markdown document in, remembered for the
 * session so reopening a file lands in the same presentation.
 *
 * Host state, not reader state: no reader reads this, because no reader chooses
 * whether it is the one being mounted. PreviewPane does, so it lives here.
 */
const markdownModes = new Map<string, TextViewerMode>();

export function saveMarkdownViewMode(path: string, mode: TextViewerMode): void {
  markdownModes.set(path, mode);
}

export function readMarkdownViewMode(path: string): TextViewerMode {
  return markdownModes.get(path) ?? "rendered";
}
