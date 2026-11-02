import { api } from "../services";

/** The list's picture: a row draws it 40px tall, on a screen that may be 2x. */
export const THUMBNAIL_EDGE = 96;

/** The picture a click opens. Larger than a window needs for most screens, so
 *  the window is what limits it, and one size means one stored copy. */
export const FULL_COVER_EDGE = 2048;

/** Thumbnails being fetched at once. Each may be a first render on the host, so
 *  a library scrolled quickly should not start one per row it passes. */
const MAX_THUMBNAIL_LOADS = 4;

let thumbnailLoads = 0;
const waitingThumbnails: Array<() => void> = [];

/** Wait for a turn. Newest first, because the row asked for last is the one
 *  still on screen; a row that has since scrolled away leaves the queue. */
function takeTurn(signal: AbortSignal): Promise<void> {
  if (thumbnailLoads < MAX_THUMBNAIL_LOADS) {
    thumbnailLoads += 1;
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const turn = () => {
      signal.removeEventListener("abort", leave);
      resolve();
    };
    const leave = () => {
      waitingThumbnails.splice(waitingThumbnails.indexOf(turn), 1);
      reject(signal.reason);
    };
    signal.addEventListener("abort", leave, { once: true });
    waitingThumbnails.push(turn);
  });
}

function endTurn() {
  const next = waitingThumbnails.pop();
  if (next) next();
  else thumbnailLoads -= 1;
}

/** An object URL for one cover, which the caller must revoke. */
export async function loadFullCover(path: string, signal: AbortSignal): Promise<string> {
  return objectUrl(await api.fileCover(path, FULL_COVER_EDGE, signal), signal);
}

/** An object URL for one row's cover, which the caller must revoke. */
export async function loadThumbnail(path: string, signal: AbortSignal): Promise<string> {
  await takeTurn(signal);
  try {
    return objectUrl(await api.fileCover(path, THUMBNAIL_EDGE, signal), signal);
  } finally {
    endTurn();
  }
}

function objectUrl(bytes: ArrayBuffer, signal: AbortSignal): string {
  signal.throwIfAborted();
  return URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
}

const visibilityListeners = new Map<Element, (visible: boolean) => void>();
let visibilityObserver: IntersectionObserver | null = null;

/** Be told whether `element` is on screen, until the returned function is
 *  called. One observer for the whole list, which can run to thousands of rows. */
export function watchVisibility(
  element: Element,
  listener: (visible: boolean) => void,
): () => void {
  visibilityObserver ??= new IntersectionObserver((entries) => {
    for (const entry of entries) {
      visibilityListeners.get(entry.target)?.(entry.isIntersecting);
    }
  });
  const observer = visibilityObserver;
  visibilityListeners.set(element, listener);
  observer.observe(element);
  return () => {
    visibilityListeners.delete(element);
    observer.unobserve(element);
  };
}
