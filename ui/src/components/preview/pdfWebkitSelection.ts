// WebKit-only fix for pdf.js text selection started in the page margin.
//
// pdf.js makes selection smooth via a non-selectable `endOfContent` sentinel:
// clicking whitespace should "fall through" to the nearest line. Chromium and
// Gecko honor that; WebKit (Safari, and Tauri's macOS WKWebView) does NOT — a
// whitespace/margin mousedown anchors the selection at a DOM extreme, so the
// drag balloons to everything before/after the line. This is an engine
// limitation with no upstream fix (reproduced on pdfjs-dist 5.x too).
//
// We take over selection ONLY when a drag STARTS on whitespace (not on a text
// span). Text-started selections keep WebKit's native path, which works — so
// this augments pdf.js' mechanism for the one case WebKit breaks, rather than
// replacing it. No-op on every non-WebKit engine.

const IS_WEBKIT =
  typeof navigator !== "undefined" &&
  /AppleWebKit/.test(navigator.userAgent) &&
  !/Chrome|Chromium|Edg|OPR/.test(navigator.userAgent);

type Caret = { node: Text; offset: number };

function textNodeOf(span: Element): Text | null {
  const n = span.firstChild;
  return n && n.nodeType === Node.TEXT_NODE ? (n as Text) : null;
}

/** Nearest caret to a point, snapping to a real text span (never endOfContent). */
function caretAtPoint(x: number, y: number, spans: Element[]): Caret | null {
  // Native hit-test is accurate when the point is over an actual glyph.
  const range = document.caretRangeFromPoint?.(x, y);
  if (
    range &&
    range.startContainer.nodeType === Node.TEXT_NODE &&
    (range.startContainer.parentElement?.closest(".textLayer"))
  ) {
    return { node: range.startContainer as Text, offset: range.startOffset };
  }
  // Whitespace/margin: snap to the nearest span by squared edge-distance.
  let best: Element | null = null;
  let bestDist = Infinity;
  for (const span of spans) {
    const r = span.getBoundingClientRect();
    const dx = x < r.left ? r.left - x : x > r.right ? x - r.right : 0;
    const dy = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0;
    const dist = dx * dx + dy * dy;
    if (dist < bestDist) {
      bestDist = dist;
      best = span;
    }
  }
  if (!best) return null;
  const node = textNodeOf(best);
  if (!node) return null;
  const r = best.getBoundingClientRect();
  // Left of the span's middle anchors at its start, otherwise its end.
  const atStart = x < r.left + r.width / 2;
  return { node, offset: atStart ? 0 : node.length };
}

/**
 * Attach the margin-selection fix to one text-layer div. Returns a cleanup fn.
 * Safe (and cheap) to call on every engine — it only binds on WebKit.
 */
export function attachWebkitMarginSelection(textLayer: HTMLElement): () => void {
  if (!IS_WEBKIT) return () => {};

  let anchor: Caret | null = null;
  let spans: Element[] = [];

  const onMove = (e: MouseEvent) => {
    if (!anchor) return;
    const focus = caretAtPoint(e.clientX, e.clientY, spans);
    if (!focus) return;
    window.getSelection()?.setBaseAndExtent(anchor.node, anchor.offset, focus.node, focus.offset);
    e.preventDefault();
  };

  const end = () => {
    if (!anchor) return;
    anchor = null;
    spans = [];
    window.removeEventListener("mousemove", onMove, true);
    window.removeEventListener("mouseup", end, true);
  };

  const onDown = (e: MouseEvent) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    // Only intercept whitespace/margin clicks. A click that lands directly on a
    // text span (the common case) anchors correctly in WebKit — leave it native.
    if (target !== textLayer && target.tagName === "SPAN" && textNodeOf(target)) return;

    // Cache spans from every mounted text layer so cross-page drags can snap.
    spans = [...document.querySelectorAll(".textLayer span")].filter(textNodeOf);
    const a = caretAtPoint(e.clientX, e.clientY, spans);
    if (!a) return;

    e.preventDefault(); // suppress WebKit's broken whitespace anchoring
    anchor = a;
    window.getSelection()?.setBaseAndExtent(a.node, a.offset, a.node, a.offset);
    window.addEventListener("mousemove", onMove, true);
    window.addEventListener("mouseup", end, true);
  };

  textLayer.addEventListener("mousedown", onDown, true);
  return () => {
    textLayer.removeEventListener("mousedown", onDown, true);
    end();
  };
}
