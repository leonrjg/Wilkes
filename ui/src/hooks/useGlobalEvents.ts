import { useEffect, useRef } from "react";
import { useToasts } from "../components/Toast";
import { api } from "../services";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useBookmarksStore } from "../stores/useBookmarksStore";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useTopicsStore } from "../stores/useTopicsStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useCatalogueStore } from "../stores/useCatalogueStore";

export function useGlobalEvents() {
  const { addToast, removeToast } = useToasts();
  const reindexToastId = useRef<string | null>(null);

  useEffect(() => {
    let managerUnlisten: (() => void) | undefined;
    let fileListUnlisten: (() => void) | undefined;
    let metadataUnlisten: (() => void) | undefined;
    let clusterLabelUnlisten: (() => void) | undefined;
    let topicLabelUnlisten: (() => void) | undefined;
    let catalogueDownloadUnlisten: (() => void) | undefined;
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
        if (!reindexToastId.current) {
          reindexToastId.current = addToast(
            "Indexing... Semantic search is temporarily unavailable",
            { type: "info", duration: 0, startTime: Date.now(), shimmer: true },
          );
        }
      } else if (payload === "ReindexingDone") {
        closeReindexToast();
        void useSemanticStore.getState().handleIndexUpdated();
      } else if (payload === "ReindexingCancelled") {
        closeReindexToast();
        void useSemanticStore.getState().handleIndexTerminated();
      }
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        managerUnlisten = u;
      }
    });

    // Subscribed once here rather than per row: a download is reported by URL
    // and any number of candidate rows may be rendering at the time.
    api.onCatalogueDownloadProgress((progress) => {
      if (!mounted) return;
      useCatalogueStore.getState().noteDownloadProgress(progress);
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        catalogueDownloadUnlisten = u;
      }
    });

    api.onFileListChanged((payload) => {
      if (!mounted) return;
      const settings = useSettingsStore.getState();
      if (settings.directory === payload.root) {
        settings.refreshFileList();
      }
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        fileListUnlisten = u;
      }
    });

    api.onFileMetadataUpdated((updates) => {
      if (!mounted) return;
      useSettingsStore.getState().applyMetadataUpdates(updates);
      // This event fires right after the metadata cache is re-keyed for
      // renamed files, so it is the moment a bookmark's stale path can be
      // re-resolved. Reload so `list_bookmarks` re-points them in-session
      // instead of only on the next app start.
      void useBookmarksStore.getState().load();
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        metadataUnlisten = u;
      }
    });

    // Cluster labels arrive after `cluster_bookmarks` has already returned:
    // labelling one cluster takes ~370ms, so inlining it would add seconds to
    // a call that is otherwise pure compute.
    api.onBookmarkClusterLabelled((event) => {
      if (!mounted) return;
      useGenerationStore.getState().applyClusterLabel(event);
    }).then((u) => {
      if (!mounted) {
        u();
      } else {
        clusterLabelUnlisten = u;
      }
    });

    api.onChunkTopicLabelled((event) => {
      if (!mounted) return;
      useTopicsStore.getState().applyLabel(event);
      useSearchStore
        .getState()
        .updateTopicResultSubject(event.cluster_key, event.label);
    }).then((unlisten) => {
      if (!mounted) {
        unlisten();
      } else {
        topicLabelUnlisten = unlisten;
      }
    });

    void useGenerationStore.getState().refreshReady();

    return () => {
      mounted = false;
      if (managerUnlisten) managerUnlisten();
      if (fileListUnlisten) fileListUnlisten();
      if (metadataUnlisten) metadataUnlisten();
      if (clusterLabelUnlisten) clusterLabelUnlisten();
      if (topicLabelUnlisten) topicLabelUnlisten();
      if (catalogueDownloadUnlisten) catalogueDownloadUnlisten();
    };
  }, [addToast, removeToast]);
}
