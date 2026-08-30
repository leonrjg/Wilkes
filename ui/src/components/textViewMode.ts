import type { TextViewerMode } from "@leonrjg/wilkes-reader";

/**
 * Which viewer Wilkes last showed a document in, remembered for the session so
 * reopening a file lands in the same presentation. Both kinds of document that
 * have two presentations -- Markdown and HTML -- are remembered here: which one
 * a path is does not change what remembering it means.
 *
 * Host state, not reader state: no reader reads this, because no reader chooses
 * whether it is the one being mounted. PreviewPane does, so it lives here.
 */
const modes = new Map<string, TextViewerMode>();

export function saveTextViewMode(path: string, mode: TextViewerMode): void {
  modes.set(path, mode);
}

export function readTextViewMode(path: string): TextViewerMode {
  return modes.get(path) ?? "rendered";
}
