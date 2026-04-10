import { useEffect, useRef } from "react";
import { useToasts } from "../components/Toast";
import { api } from "../services";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";

export function useGlobalEvents() {
  const { addToast, removeToast } = useToasts();
  const reindexToastId = useRef<string | null>(null);

  useEffect(() => {
    let managerUnlisten: (() => void) | undefined;
    let mounted = true;

    const closeReindexToast = () => {
      if (reindexToastId.current) {
        removeToast(reindexToastId.current);
        reindexToastId.current = null;
      }
    };

    api.onManagerEvent((payload) => {
      if (!mounted) return;
      if (payload === "WorkerStarting") {
        addToast("Starting worker... Next queries will be faster", { type: "info" });
      } else if (payload === "Reindexing") {
        useSettingsStore.getState().refreshFileList();
        if (!reindexToastId.current) {
          reindexToastId.current = addToast(
            "Indexing... Semantic search is temporarily unavailable",
            { type: "info", duration: 0, startTime: Date.now(), shimmer: true },
          );
        }
      } else if (payload === "ReindexingDone") {
        closeReindexToast();
        void useSearchStore.getState().replaySearch();
      } else if (payload === "ReindexingCancelled") {
        closeReindexToast();
      }
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        managerUnlisten = u;
      }
    });

    return () => {
      mounted = false;
      if (managerUnlisten) managerUnlisten();
    };
  }, [addToast, removeToast]);
}
