import { useEffect, useRef } from "react";
import { useToasts } from "../components/Toast";
import { api } from "../services";
import { useSearchStore } from "../stores/useSearchStore";

export function useGlobalEvents() {
  const { addToast, removeToast } = useToasts();
  const reindexToastId = useRef<string | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;

    api.onManagerEvent((payload) => {
      if (!mounted) return;
      if (payload === "WorkerStarting") {
        addToast("Starting worker... Next queries will be faster", { type: "info" });
      } else if (payload === "Reindexing") {
        if (!reindexToastId.current) {
          reindexToastId.current = addToast(
            "Reindexing... Semantic search is temporarily unavailable",
            { type: "info", duration: 0, startTime: Date.now(), shimmer: true },
          );
        }
      } else if (payload === "ReindexingDone") {
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

    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [addToast, removeToast]);
}
