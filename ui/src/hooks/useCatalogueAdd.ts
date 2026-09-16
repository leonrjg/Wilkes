import { useCallback } from "react";
import { api, source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import type { CatalogueHit, LiteratureSearchResult } from "../lib/types";
import { type AddStage, hitKey, paperKey, useCatalogueStore } from "../stores/useCatalogueStore";
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
  const adding = useCatalogueStore((s) => s.adding);
  const acquired = useCatalogueStore((s) => s.acquired);
  const setAddStage = useCatalogueStore((s) => s.setAddStage);
  const noteAddError = useCatalogueStore((s) => s.noteAddError);

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

  /**
   * One add, from the click to the file being in the library or the reason it
   * is not.
   *
   * Every stage of it is marked here, because this is the only place that
   * knows there are three: the row asked to show progress "until the file is
   * on disk", and the download is only the middle of that. The stage is
   * cleared however the act ends, so a failure cannot leave a row spinning.
   */
  const runAdd = useCallback(
    async (key: string, act: () => Promise<string | null>): Promise<string | null> => {
      noteAddError(null);
      try {
        return await act();
      } catch (error: any) {
        // Every failing stage reaches the user the same way. A resolver that
        // refused and a move that failed used to reject into nothing at all.
        console.error("adding to the library failed:", error);
        noteAddError(error?.toString?.() ?? "Could not add that document");
        return null;
      } finally {
        setAddStage(key, null);
      }
    },
    [noteAddError, setAddStage],
  );

  const add = useCallback(
    async (hit: CatalogueHit): Promise<string | null> => {
      if (!canAdd) return null;
      const key = hitKey(hit);
      // A course is many files and a document describing them; a textbook is
      // one file. Which of the two this is was decided in core and travels on
      // the hit, so this does not test the provider id to find out.
      if (hit.acquisition === "course") {
        return runAdd(key, async () => {
          setAddStage(key, "fetching");
          const course = await acquireCourse(hit);
          if (course === null) return null;
          setAddStage(key, "importing");
          await install(course.paths, course.folder);
          return course.paths[0] ?? null;
        });
      }
      if (hit.pdf_url === null) return null;
      const url = hit.pdf_url;
      return runAdd(key, async () => {
        setAddStage(key, "fetching");
        const staged = await acquire({ key, url });
        setAddStage(key, "importing");
        await install([staged]);
        return staged;
      });
    },
    [canAdd, acquire, acquireCourse, install, runAdd, setAddStage],
  );

  /** Resolve only after selection, then use the same staging downloader as
   *  every other single document. A provider resolver may spend a credential;
   *  search itself never does that once per displayed row. */
  const addPaper = useCallback(
    async (provider: string, work: LiteratureSearchResult): Promise<string | null> => {
      const acquisition = work.acquisition ?? (work.pdf_url === null ? "none" : "direct");
      if (!canAdd || acquisition === "none") return null;
      const key = paperKey(provider, work);
      return runAdd(key, async () => {
        // Resolving is a request of its own, and a credentialed resolver is
        // not a fast one. It is named as its own stage rather than spent
        // before the row has said anything is happening.
        const direct = acquisition === "direct" && work.pdf_url !== null;
        setAddStage(key, direct ? "fetching" : "resolving");
        const resolved = direct
          ? { url: work.pdf_url as string, filename: null }
          : await api.literatureResolveDownload(provider, work);
        setAddStage(key, "fetching");
        const staged = await acquire({
          key,
          url: resolved.url,
          filename: resolved.filename ?? undefined,
        });
        setAddStage(key, "importing");
        await install([staged]);
        return staged;
      });
    },
    [canAdd, acquire, install, runAdd, setAddStage],
  );

  return {
    add,
    addPaper,
    canAdd,
    needsDirectory,
    readOnly,
    /** How far this row's add has got, or undefined when it is not adding.
     *  The key is built here so a row never has to know how one is spelled. */
    stageOf: (hit: CatalogueHit): AddStage | undefined => adding[hitKey(hit)],
    paperStageOf: (
      provider: string,
      work: LiteratureSearchResult,
    ): AddStage | undefined => adding[paperKey(provider, work)],
    isAdded: (hit: CatalogueHit) => hitKey(hit) in acquired,
    isPaperAdded: (provider: string, work: LiteratureSearchResult) =>
      paperKey(provider, work) in acquired,
  };
}
