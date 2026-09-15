import { useCallback } from "react";
import { api, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import type { CatalogueHit, LiteratureSearchResult } from "../lib/types";
import { hitKey, paperKey, useCatalogueStore } from "../stores/useCatalogueStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useActiveWorkspaceReadOnly } from "../stores/useWorkspaceStore";

/** The directory a path sits in. The path was just handed to us by the
 *  backend, so it is well-formed; this only has to find its last separator. */
function parentDirectory(path: string): string {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return cut > 0 ? path.slice(0, cut) : path;
}

/**
 * Adding a catalogue candidate, or a paper a literature provider found, to the
 * library.
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
   *  where uploads *is* the root — the root is simply pointed at it.
   *
   *  `folder` is passed for a course and omitted for a single document, so a
   *  course keeps its folder across the move instead of arriving as forty
   *  loose files the root cannot attribute to it. */
  const install = useCallback(
    async (staged: string[], folder?: string): Promise<void> => {
      if (source.type === "desktop") {
        await (source as DesktopSourceApi).importFiles(staged, directory, "move", folder);
      } else if (!directory && staged.length > 0) {
        // On the web build the files are already in their course folder under
        // the served root, so the folder is where the root should point.
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
        const course = await acquireCourse(hit);
        if (course === null) return null;
        await install(course.paths, course.folder);
        return course.paths[0] ?? null;
      }
      if (hit.pdf_url === null) return null;
      const staged = await acquire({ key: hitKey(hit), url: hit.pdf_url });
      if (staged === null) return null;
      await install([staged]);
      return staged;
    },
    [canAdd, acquire, acquireCourse, install],
  );

  /** Resolve only after selection, then use the same staging downloader as
   *  every other single document. A provider resolver may spend a credential;
   *  search itself never does that once per displayed row. */
  const addPaper = useCallback(
    async (provider: string, work: LiteratureSearchResult): Promise<string | null> => {
      const acquisition = work.acquisition ?? (work.pdf_url === null ? "none" : "direct");
      if (!canAdd || acquisition === "none") return null;
      const resolved =
        acquisition === "direct" && work.pdf_url !== null
          ? { url: work.pdf_url, filename: null }
          : await api.literatureResolveDownload(provider, work);
      const staged = await acquire({
        key: paperKey(provider, work),
        url: resolved.url,
        filename: resolved.filename ?? undefined,
      });
      if (staged === null) return null;
      await install([staged]);
      return staged;
    },
    [canAdd, acquire, install],
  );

  return {
    add,
    addPaper,
    canAdd,
    needsDirectory,
    readOnly,
    isAdding: (hit: CatalogueHit) => acquiring === hitKey(hit),
    isAdded: (hit: CatalogueHit) => hitKey(hit) in acquired,
    isPaperAdding: (provider: string, work: LiteratureSearchResult) =>
      acquiring === paperKey(provider, work),
    isPaperAdded: (provider: string, work: LiteratureSearchResult) =>
      paperKey(provider, work) in acquired,
  };
}
