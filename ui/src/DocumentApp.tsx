import { useCallback, useEffect } from "react";
import PreviewPane from "./components/PreviewPane";
import BrowserPane from "./components/BrowserPane";
import { useToasts } from "./components/Toast";
import { api } from "./services";
import { useNativeOpen } from "./hooks/useNativeOpen";
import { useWindowTitle } from "./hooks/useWindowTitle";
import { useSettingsStore } from "./stores/useSettingsStore";
import { useBrowserStore } from "./stores/useBrowserStore";
import { activeViewerTab, useViewerStore } from "./stores/useViewerStore";
import type { NativeOpenRequest } from "./lib/types";

/** The OS-opened document shell. It intentionally has no workspace picker,
 * root, search list, or workspace-owned companion panes. The browser pane is
 * none of those: a link followed here opens beside the document it was in. */
export default function DocumentApp() {
  const { addToast } = useToasts();
  const activePath = useViewerStore((state) => activeViewerTab(state)?.path ?? null);
  const browserPaneOpen = useBrowserStore((state) => state.paneOpen);

  // This window belongs to no workspace or root, so its title is only the
  // document it is showing.
  useWindowTitle({ document: activePath });

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

  // Nothing here needs loading before a document can be shown, so this
  // window is ready as soon as it exists.
  useNativeOpen(
    true,
    useCallback(
      (request: NativeOpenRequest) => {
        for (const error of request.errors) addToast(error, { type: "error" });
        const viewer = useViewerStore.getState();
        // The origin belongs to the first path and only to it: a link names
        // one document, and a multi-file open from the operating system names
        // no place inside any of them.
        request.paths.forEach((path, index) =>
          viewer.openFile(path, index === 0 ? request.origin : null),
        );
      },
      [addToast],
    ),
    useCallback((message: string) => addToast(message, { type: "error" }), [addToast]),
  );

  return (
    <main className="flex h-screen min-h-0 overflow-hidden bg-[var(--bg-app)] text-[var(--text-main)]">
      <div className="min-h-0 min-w-0 flex-1">
        <PreviewPane standalone />
      </div>
      {browserPaneOpen && (
        <div className="w-[45%] min-w-[320px] flex-shrink-0 border-l border-[var(--border-main)]">
          <BrowserPane />
        </div>
      )}
    </main>
  );
}
