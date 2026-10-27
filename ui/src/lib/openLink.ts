import { api, isTauri } from "../services";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useSettingsStore } from "../stores/useSettingsStore";

export function isWebAddress(target: string): boolean {
  return /^https?:\/\//i.test(target);
}

/** Where a link goes. A web address opens in this window's browser pane when
 *  the desktop app has that turned on, and in the system browser otherwise;
 *  anything else -- a file a document points at -- is the operating system's
 *  to open. Every link the app follows comes through here. */
export async function openLink(target: string): Promise<void> {
  if (isWebAddress(target) && isTauri && useSettingsStore.getState().settings?.open_links_in_app) {
    useBrowserStore.getState().show(target);
    return;
  }
  return api.openPath(target);
}

/** Send every click on a web link in the page through `openLink`. Left alone,
 *  a plain anchor does nothing in the desktop webview, which opens no new
 *  windows, and replaces the app when it has no target. A click a component
 *  has already handled (its default prevented) is that component's. */
export function routeLinkClicks(root: Document = document): () => void {
  const onClick = (event: MouseEvent) => {
    if (event.defaultPrevented || event.button !== 0) return;
    if (!(event.target instanceof Element)) return;
    const anchor = event.target.closest("a[href]");
    // The attribute, not `anchor.href`: a relative link resolves against the
    // app's own origin, which is not a page to open.
    const href = anchor?.getAttribute("href");
    if (!href || !isWebAddress(href)) return;
    event.preventDefault();
    openLink(href).catch((e) => console.error("Open link failed:", e));
  };
  root.addEventListener("click", onClick);
  return () => root.removeEventListener("click", onClick);
}
