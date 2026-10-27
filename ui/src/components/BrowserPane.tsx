// The docked browser. The page only lays out a placeholder (the stage); the
// desktop shell keeps a native webview over its rectangle. That webview sits
// above everything the page draws -- a menu or a dialog that overlaps the pane
// is hidden under it -- which is accepted rather than worked around.
import { useEffect, useRef, useState } from "react";
import { ArrowLeft, ArrowRight, ExternalLink, Globe, Loader, Moon, RotateCw, Trash2, X } from "react-feather";
import { Tooltip } from "@leonrjg/wilkes-reader";
import { api } from "../services";
import type { BrowserStep, PaneBounds } from "../services/api";
import { useBrowserStore } from "../stores/useBrowserStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { confirmDialog } from "../lib/utils/dialog";

function boundsOf(element: HTMLElement): PaneBounds {
  const rect = element.getBoundingClientRect();
  return { x: rect.left, y: rect.top, width: rect.width, height: rect.height };
}

function sameBounds(a: PaneBounds, b: PaneBounds) {
  return a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
}

/** What someone types into the address bar, as an address: a bare host is
 *  taken to mean its https site. */
export function addressFrom(input: string): string {
  const trimmed = input.trim();
  return /^[a-z][a-z0-9+.-]*:/i.test(trimmed) ? trimmed : `https://${trimmed}`;
}

const iconButton =
  "inline-flex shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] disabled:opacity-40 disabled:hover:bg-transparent";

export default function BrowserPane() {
  const stage = useRef<HTMLDivElement>(null);
  // Whether the native webview exists yet: it is made by the first address
  // the pane is asked to show, not by the pane opening.
  const created = useRef(false);
  const request = useBrowserStore((s) => s.request);
  const url = useBrowserStore((s) => s.url);
  const title = useBrowserStore((s) => s.title);
  const loading = useBrowserStore((s) => s.loading);
  const held = useBrowserStore((s) => s.held);
  const closePane = useBrowserStore((s) => s.closePane);
  const invertLightPages = useBrowserStore((s) => s.invertLightPages);
  const toggleInversion = useBrowserStore((s) => s.toggleInversion);
  const dark = useSettingsStore((s) => s.colorScheme === "dark");
  const inversionPercent = useSettingsStore((s) => s.settings?.dark_mode_inversion ?? 90);
  // The reader's inversion, at the reader's strength.
  const inversion = dark && invertLightPages ? inversionPercent / 100 : null;
  const [draft, setDraft] = useState(request?.url ?? "");
  const [editing, setEditing] = useState(false);

  useEffect(() => {
    if (!editing && url) setDraft(url);
  }, [url, editing]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    api
      .onBrowserPanePage?.(useBrowserStore.getState().applyPageEvent)
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((e) => console.error("browser pane: could not listen to its page", e));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  // Follow the stage wherever the layout moves it. A frame loop rather than a
  // ResizeObserver, because a neighbouring pane moves the stage without
  // resizing it. The webview closes with the pane.
  useEffect(() => {
    const element = stage.current;
    if (!element) return;
    let last = boundsOf(element);
    let frame = requestAnimationFrame(function follow() {
      const next = boundsOf(element);
      if (!sameBounds(last, next)) {
        last = next;
        api.browserPaneSetBounds?.(next).catch((e) => console.error("browser pane: move failed", e));
      }
      frame = requestAnimationFrame(follow);
    });
    return () => {
      cancelAnimationFrame(frame);
      created.current = false;
      api.browserPaneClose?.().catch((e) => console.error("browser pane: close failed", e));
    };
  }, []);

  useEffect(() => {
    const element = stage.current;
    if (!request || !element) return;
    setDraft(request.url);
    api
      .browserPaneShow?.(request.url, boundsOf(element))
      .then(() => {
        created.current = true;
      })
      .catch((e) => console.error("browser pane: could not open", request.url, e));
  }, [request]);

  // Held by the shell for every page the pane loads, so it is sent whether or
  // not the webview exists yet.
  useEffect(() => {
    api.browserPaneSetInversion?.(inversion).catch((e) => console.error("browser pane: could not set inversion", e));
  }, [inversion]);

  useEffect(() => {
    if (!created.current) return;
    api.browserPaneSetVisible?.(!held).catch((e) => console.error("browser pane: could not step aside", e));
  }, [held]);

  const step = (to: BrowserStep) =>
    api.browserPaneStep?.(to).catch((e) => console.error(`browser pane: ${to} failed`, e));

  const clearData = async () => {
    const confirmed = await confirmDialog(
      "Clear the browser pane's cookies and site data? This signs you out of every site you signed in to there.",
    );
    if (!confirmed) return;
    await api.browserPaneClearData?.().catch((e) => console.error("browser pane: clearing data failed", e));
  };

  const hasPage = url !== null && request !== null;

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-sidebar)]">
      <form
        className="flex items-center gap-1 border-b border-[var(--border-main)] p-2"
        onSubmit={(event) => {
          event.preventDefault();
          if (draft.trim() === "") return;
          useBrowserStore.getState().show(addressFrom(draft));
          (document.activeElement as HTMLElement | null)?.blur();
        }}
      >
        <Tooltip content="Back">
          <button type="button" aria-label="Back" disabled={!hasPage} onClick={() => step("back")} className={iconButton}>
            <ArrowLeft size={12} />
          </button>
        </Tooltip>
        <Tooltip content="Forward">
          <button type="button" aria-label="Forward" disabled={!hasPage} onClick={() => step("forward")} className={iconButton}>
            <ArrowRight size={12} />
          </button>
        </Tooltip>
        <Tooltip content="Reload">
          <button type="button" aria-label="Reload" disabled={!hasPage} onClick={() => step("reload")} className={iconButton}>
            {loading ? <Loader size={12} className="animate-spin" /> : <RotateCw size={12} />}
          </button>
        </Tooltip>
        <div className="flex min-w-0 flex-1 items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] pl-2 pr-1">
          <Globe size={12} className="shrink-0 text-[var(--text-dim)]" />
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onFocus={(e) => {
              setEditing(true);
              e.target.select();
            }}
            onBlur={() => setEditing(false)}
            placeholder="Enter an address"
            aria-label="Address"
            title={title ?? undefined}
            spellCheck={false}
            className="min-w-0 flex-1 bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
          />
        </div>
        <Tooltip content="Open in your browser">
          <button
            type="button"
            aria-label="Open in your browser"
            disabled={!url}
            onClick={() => {
              if (url) api.openPath(url).catch((e) => console.error("browser pane: open in browser failed", e));
            }}
            className={iconButton}
          >
            <ExternalLink size={12} />
          </button>
        </Tooltip>
        {dark && (
          <Tooltip content={invertLightPages ? "Stop inverting light pages" : "Invert light pages"}>
            <button
              type="button"
              aria-label="Invert light pages"
              aria-pressed={invertLightPages}
              onClick={toggleInversion}
              className={`${iconButton} ${invertLightPages ? "text-[var(--accent-blue)]" : ""}`}
            >
              <Moon size={12} />
            </button>
          </Tooltip>
        )}
        <Tooltip content="Clear cookies and site data">
          <button
            type="button"
            aria-label="Clear cookies and site data"
            disabled={!hasPage}
            onClick={() => void clearData()}
            className={iconButton}
          >
            <Trash2 size={12} />
          </button>
        </Tooltip>
        <Tooltip content="Close">
          <button type="button" aria-label="Close browser pane" onClick={closePane} className={iconButton}>
            <X size={12} />
          </button>
        </Tooltip>
      </form>
      <div ref={stage} className="relative min-h-0 flex-1">
        {!request && (
          <p className="p-4 text-center text-[11px] text-[var(--text-dim)]">
            Links you follow open here.
          </p>
        )}
      </div>
    </div>
  );
}
