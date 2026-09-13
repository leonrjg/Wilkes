import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { fileName } from "../components/DocumentEntryRow";
import { isTauri } from "../services";

const APP_NAME = "Wilkes";
const SEPARATOR = " — ";

export interface WindowTitleParts {
  workspace?: string | null;
  /** The active library root, as a path; the title names its last segment. */
  root?: string | null;
  /** The reader's active document, as a path; the title names its file. */
  document?: string | null;
}

/**
 * The window title, most specific first.
 *
 * The title is what a screen reader announces on focus and what the window
 * switcher lists, and both truncate from the end. So the part that changes
 * most often — the document — leads, and the application's own name, which
 * never changes, trails.
 */
export function composeWindowTitle({ workspace, root, document }: WindowTitleParts): string {
  const rootName = root ? fileName(root.replace(/[/\\]+$/, "")) || root : null;
  return [document ? fileName(document) : null, rootName, workspace || null, APP_NAME]
    .filter((part): part is string => Boolean(part))
    .join(SEPARATOR);
}

/**
 * Keeps this window's title on what it is showing.
 *
 * `document.title` names the browser tab in the web build; the desktop shell
 * does not follow it, so the native window is titled explicitly as well.
 */
export function useWindowTitle(parts: WindowTitleParts): void {
  const title = composeWindowTitle(parts);

  useEffect(() => {
    document.title = title;
    if (!isTauri) return;
    getCurrentWindow()
      .setTitle(title)
      .catch((error) => console.error("Could not set the window title:", error));
  }, [title]);
}
