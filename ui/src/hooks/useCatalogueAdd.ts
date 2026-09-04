import { useCallback } from "react";
import { source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import type { CatalogueHit } from "../lib/types";
import { hitKey, useCatalogueStore } from "../stores/useCatalogueStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useActiveWorkspaceReadOnly } from "../stores/useWorkspaceStore";

/** The directory a path sits in. The path was just handed to us by the
 *  backend, so it is well-formed; this only has to find its last separator. */
function parentDirectory(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut > 0 ? path.slice(0, cut) : path;
}

/**
 * Adding a catalogue candidate to the library.
 *
 * Two steps, deliberately: the fetch lands in Wilkes's own uploads directory,
 * and only then is the file imported into the library root. Fetching straight
 * into a root would be Wilkes writing into a directory whose contents the user
 * believes they control.
 *
 * On the web build the uploads directory *is* the root the server serves, so
 * the second step is a refresh rather than a move.
 */
export function useCatalogueAdd() {
  const directory = useSettingsStore((s) => s.directory);
  const setDirectory = useSettingsStore((s) => s.setDirectory);
  const refreshFileList = useSettingsStore((s) => s.refreshFileList);
  const readOnly = useActiveWorkspaceReadOnly();
  const acquire = useCatalogueStore((s) => s.acquire);
  const acquireCourse = useCatalogueStore((s) => s.acquireCourse);
  const acquiring = useCatalogueStore((s) => s.acquiring);
  const acquired = useCatalogueStore((s) => s.acquired);

  // A desktop import needs somewhere to import to. The web build has its root
  // by construction, so it can add before the user has chosen anything.
  const needsDirectory = source.type === "desktop" && !directory;
  const canAdd = !readOnly && !needsDirectory;

  /** The second step, shared by both kinds of candidate: whatever was staged
   *  in uploads is imported into the library root, or — on the web build,
   *  where uploads *is* the root — the root is simply pointed at it. */
  const install = useCallback(
    async (staged: string[]): Promise<void> => {
      if (source.type === "desktop") {
        await (source as DesktopSourceApi).importFiles(staged, directory, "move");
      } else if (!directory && staged.length > 0) {
        setDirectory(parentDirectory(staged[0]));
      }
      await refreshFileList();
    },
    [directory, setDirectory, refreshFileList],
  );

  const add = useCallback(
    async (hit: CatalogueHit): Promise<string | null> => {
      if (!canAdd) return null;
      // A course is many files and a document describing them; a textbook is
      // one file. Which of the two this is was decided in core and travels on
      // the hit, so this does not test the provider id to find out.
      if (hit.acquisition === "course") {
        const staged = await acquireCourse(hit);
        if (staged === null) return null;
        await install(staged);
        return staged[0] ?? null;
      }
      const staged = await acquire(hit);
      if (staged === null) return null;
      await install([staged]);
      return staged;
    },
    [canAdd, acquire, acquireCourse, install],
  );

  return {
    add,
    canAdd,
    needsDirectory,
    readOnly,
    isAdding: (hit: CatalogueHit) => acquiring === hitKey(hit),
    isAdded: (hit: CatalogueHit) => hitKey(hit) in acquired,
  };
}
