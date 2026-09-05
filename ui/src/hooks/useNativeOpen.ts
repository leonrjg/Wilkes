import { useEffect, useRef } from "react";
import { api } from "../services";
import type { NativeOpenRequest } from "../lib/types";

/**
 * Receive the open requests addressed to this window.
 *
 * A request from outside the application — a file the operating system handed
 * over, or a `wilkes://` link clicked elsewhere — can arrive before the
 * webview that has to show it exists, let alone before React has mounted. The
 * host queues those per window; this is the other half of that handshake, and
 * it is one hook rather than one per window so the ordering is written down
 * once: register the listener, *then* say the window is ready, then show what
 * had been waiting.
 *
 * `ready` gates the whole handshake, and is how a window says it can act on a
 * request rather than merely receive one. The main window cannot open a
 * document into a workspace it has not loaded yet, so it holds the queue shut
 * until it has — the host keeps the request, and nothing is lost by waiting.
 */
export function useNativeOpen(
  ready: boolean,
  open: (request: NativeOpenRequest) => void,
  reportProblem: (message: string) => void,
): void {
  // Held in refs so a re-rendered caller does not tear down the listener and
  // re-run the drain: the handshake belongs to the window's lifetime, not to
  // the identity of this render's callbacks.
  const openRef = useRef(open);
  openRef.current = open;
  const reportRef = useRef(reportProblem);
  reportRef.current = reportProblem;

  useEffect(() => {
    if (!ready) return;

    let disposed = false;
    let unlisten: (() => void) | undefined;

    const connect = async () => {
      if (!api.onNativeOpen || !api.nativeOpenReady) {
        reportRef.current("Native file opening is unavailable in this build");
        return;
      }
      const nextUnlisten = await api.onNativeOpen((request) => {
        if (!disposed) openRef.current(request);
      });
      if (disposed) {
        nextUnlisten();
        return;
      }
      unlisten = nextUnlisten;
      for (const request of await api.nativeOpenReady()) {
        if (disposed) return;
        openRef.current(request);
      }
    };

    connect().catch((error) => {
      console.error("Could not connect the external open bridge:", error);
      if (!disposed) {
        reportRef.current("Could not receive files from the operating system");
      }
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [ready]);
}
