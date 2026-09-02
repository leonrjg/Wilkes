import { useEffect } from "react";
import PreviewPane from "./components/PreviewPane";
import { useToasts } from "./components/Toast";
import { api } from "./services";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useViewerStore } from "./stores/useViewerStore";
import type { NativeOpenRequest } from "./lib/types";

/** The OS-opened document shell. It intentionally has no workspace picker,
 * root, search list, or workspace-owned companion panes. */
export default function DocumentApp() {
  const { addToast } = useToasts();

  useEffect(() => {
    let disposed = false;
    api.getGlobalSettings?.()
      .then((settings) => {
        if (!disposed) useSettingsStore.getState().replaceSettings(settings);
      })
      .catch((error) => {
        console.error("Could not load document-viewer preferences:", error);
        if (!disposed) addToast("Could not load viewer preferences", { type: "error" });
      });
    return () => {
      disposed = true;
    };
  }, [addToast]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    const openRequest = (request: NativeOpenRequest) => {
      if (disposed) return;
      for (const error of request.errors) {
        addToast(error, { type: "error" });
      }
      const viewer = useViewerStore.getState();
      for (const path of request.paths) viewer.openFile(path);
    };

    const connect = async () => {
      if (!api.onNativeOpen || !api.documentWindowReady) {
        addToast("Native file opening is unavailable in this build", { type: "error" });
        return;
      }
      const nextUnlisten = await api.onNativeOpen(openRequest);
      if (disposed) {
        nextUnlisten();
        return;
      }
      unlisten = nextUnlisten;
      const queued = await api.documentWindowReady();
      for (const request of queued) openRequest(request);
    };

    connect().catch((error) => {
      console.error("Could not connect the native file-open bridge:", error);
      if (!disposed) addToast("Could not receive files from the operating system", { type: "error" });
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [addToast]);

  return (
    <main className="h-screen min-h-0 overflow-hidden bg-[var(--bg-app)] text-[var(--text-main)]">
      <PreviewPane standalone />
    </main>
  );
}
