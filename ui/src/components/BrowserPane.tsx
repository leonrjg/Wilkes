// PROTOTYPE: a docked browser. The page only lays out a placeholder; the
// desktop shell keeps a native webview over its rectangle. That webview sits
// above everything the page draws, which is what this prototype demonstrates.
import { useEffect, useRef, useState } from "react";
import { Globe, X } from "react-feather";
import { api } from "../services";
import type { PaneBounds } from "../services/api";

const START_URL = "https://en.wikipedia.org/wiki/Maurice_Wilkes";

function boundsOf(element: HTMLElement): PaneBounds {
  const rect = element.getBoundingClientRect();
  return { x: rect.left, y: rect.top, width: rect.width, height: rect.height };
}

function sameBounds(a: PaneBounds | null, b: PaneBounds) {
  return a !== null && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
}

export default function BrowserPane({ onClose }: { onClose: () => void }) {
  const stage = useRef<HTMLDivElement>(null);
  const [draft, setDraft] = useState(START_URL);

  // Show the webview over the stage, then follow the stage wherever the layout
  // moves it. A frame loop rather than a ResizeObserver, because neighbouring
  // panes move the stage without resizing it.
  useEffect(() => {
    const element = stage.current;
    if (!element) return;
    let last: PaneBounds | null = boundsOf(element);
    api.browserPaneShow?.(START_URL, last).catch((e) => console.error("browser pane: show failed", e));
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
      api.browserPaneClose?.().catch((e) => console.error("browser pane: close failed", e));
    };
  }, []);

  const go = () => {
    const element = stage.current;
    if (!element) return;
    api.browserPaneShow?.(draft, boundsOf(element)).catch((e) => console.error("browser pane: navigate failed", e));
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-sidebar)]">
      <form
        className="flex items-center gap-1.5 border-b border-[var(--border-main)] p-2"
        onSubmit={(event) => {
          event.preventDefault();
          go();
        }}
      >
        <div className="flex min-w-0 flex-1 items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] pl-2 pr-1">
          <Globe size={12} className="shrink-0 text-[var(--text-dim)]" />
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            aria-label="Address"
            className="min-w-0 flex-1 bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none"
          />
        </div>
        <button
          type="button"
          aria-label="Close browser pane"
          onClick={onClose}
          className="inline-flex shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
        >
          <X size={12} />
        </button>
      </form>
      <div ref={stage} className="min-h-0 flex-1" />
    </div>
  );
}
