import { useEffect } from "react";
import { useToasts } from "../components/Toast";
import { api } from "../services";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useSemanticStore } from "../stores/useSemanticStore";
import { useActiveWorkspaceReadOnly } from "../stores/useWorkspaceStore";

function folderName(path: string): string {
  return path.replace(/[/\\]+$/, "").split(/[/\\]/).pop() || path;
}

/**
 * Offer to index the documents an indexed folder has gained while nobody was
 * watching it.
 *
 * The directory watcher follows only the active folder, and only while Wilkes
 * runs, so a folder becoming active — at launch, or by switching to it — is
 * exactly when it may hold files the index never saw. Each time that happens
 * this asks once. Indexing stays the user's act: the toast offers, the button
 * starts it, and a folder with no index of its own is never offered here.
 */
export function useMissingDocumentsOffer() {
  const { addToast, removeToast } = useToasts();
  const directory = useSettingsStore((state) => state.directory);
  const readOnly = useActiveWorkspaceReadOnly();

  useEffect(() => {
    if (!directory || readOnly) return;
    let current = true;
    let offerId: string | null = null;

    useSemanticStore
      .getState()
      .missingDocumentsToOffer(directory)
      .then((missing) => {
        if (!current || missing === 0) return;
        const documents = `${missing} document${missing === 1 ? "" : "s"}`;
        offerId = addToast(
          `${documents} in ${folderName(directory)} ${missing === 1 ? "isn't" : "aren't"} in the semantic index yet.`,
          {
            type: "info",
            duration: 0,
            action: {
              label: `Index ${missing === 1 ? "it" : "them"}`,
              onClick: () => {
                offerId = null;
                const { semantic } = useSettingsStore.getState();
                if (!semantic) {
                  addToast("Could not index the missing documents: no model is selected.", {
                    type: "error",
                  });
                  return;
                }
                api.indexMissingDocuments(directory, semantic.selected).catch((e: any) => {
                  console.error("indexMissingDocuments failed:", e);
                  addToast(`Could not index the missing documents: ${e?.toString?.() ?? e}`, {
                    type: "error",
                  });
                });
              },
            },
          },
        );
      })
      .catch((e) => console.error("Checking for documents missing from the index failed:", e));

    // The offer is about this folder; leaving it withdraws the question.
    return () => {
      current = false;
      if (offerId) removeToast(offerId);
    };
  }, [directory, readOnly, addToast, removeToast]);
}
