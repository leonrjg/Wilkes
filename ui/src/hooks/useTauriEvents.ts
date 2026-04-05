import { useEffect, useRef } from "react";
import { useToasts } from "../components/Toast";
import { isTauri } from "../services";
import { useSearchStore } from "../stores/useSearchStore";

export function useTauriEvents() {
  const { addToast, removeToast } = useToasts();
  const reindexToastId = useRef<string | null>(null);

  useEffect(() => {
    if (!isTauri) return;

    let unlisten: (() => void) | undefined;
    let mounted = true;

    import("@tauri-apps/api/event").then(({ listen }) => {
      if (!mounted) return;
      listen<string>("manager-event", (event) => {
        if (event.payload === "WorkerStarting") {
          addToast("Starting worker... Next queries will be faster", { type: "info" });
        } else if (event.payload === "Reindexing") {
          if (!reindexToastId.current) {
            reindexToastId.current = addToast(
              "Reindexing... Semantic search is temporarily unavailable",
              { type: "info", duration: 0, startTime: Date.now(), shimmer: true },
            );
          }
        } else if (event.payload === "ReindexingDone") {
          if (reindexToastId.current) {
            removeToast(reindexToastId.current);
            reindexToastId.current = null;
          }
          useSearchStore.getState().replaySearch();
        }
      }).then((u) => {
        if (!mounted) {
          u();
        } else {
          unlisten = u;
        }
      });
    });

    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [addToast, removeToast]);
}
