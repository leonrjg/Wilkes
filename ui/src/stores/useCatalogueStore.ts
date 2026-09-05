import { create } from "zustand";
import { api } from "../services";
import type {
  CatalogueCourse,
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  CatalogueGrain,
  CatalogueHit,
} from "../lib/types";

export const ALL_GRAINS: CatalogueGrain[] = ["textbook", "course", "reference"];

/**
 * One probe's answer, kept apart from the query that produced it.
 *
 * `terms` is what the query reduced to. An empty `hits` with empty `terms` is
 * "nothing in what you typed was searchable", which is a different sentence
 * from "the catalogues hold nothing on this" — and the only way to tell them
 * apart, since the filtering rules live in the store and are not repeated here.
 */
export interface CatalogueAnswer {
  query: string;
  terms: string[];
  hits: CatalogueHit[];
}

interface CatalogueStore {
  paneOpen: boolean;
  query: string;
  grains: CatalogueGrain[];
  loading: boolean;
  answer: CatalogueAnswer | null;
  error: string | null;
  /** Path in the uploads directory, per external id, for what has been
   *  fetched in this session. Keyed by id so a row can say "added" without
   *  the pane having to re-read the directory. */
  acquired: Record<string, string>;
  acquiring: string | null;
  /** Bytes so far for each download in flight, keyed by the URL that was
   *  requested. Keyed rather than singular because nothing stops two rows from
   *  being added at once, and a single figure would be attributed to whichever
   *  row happened to render. */
  downloads: Record<string, CatalogueDownloadProgress>;
  /** How far each course acquisition has got, keyed by the course URL. A
   *  course is dozens of manifest reads and then dozens of downloads, and the
   *  byte stream above cannot say which of them it is reporting. */
  courseProgress: Record<string, CatalogueCourseProgress>;
  /** What each acquired course produced, per hit key: the directory, the
   *  generated document, and what was refused. Kept so a row can say "14
   *  documents, 4 videos skipped" instead of only "added". */
  courses: Record<string, CatalogueCourse>;
  noteDownloadProgress: (progress: CatalogueDownloadProgress) => void;
  noteCourseProgress: (progress: CatalogueCourseProgress) => void;
  openPane: () => void;
  closePane: () => void;
  setQuery: (query: string) => void;
  toggleGrain: (grain: CatalogueGrain) => void;
  search: (query: string) => Promise<void>;
  acquire: (hit: CatalogueHit) => Promise<string | null>;
  /** Fetches a whole course. Returns the folder it belongs in and the paths
   *  to import — the generated document first, because it is the thing that
   *  makes the rest a course. */
  acquireCourse: (
    hit: CatalogueHit,
  ) => Promise<{ folder: string; paths: string[] } | null>;
  reset: () => void;
}

/** The catalogue key of a hit: unique per provider, not across providers. */
export function hitKey(hit: CatalogueHit): string {
  return `${hit.provider}:${hit.external_id}`;
}

export const useCatalogueStore = create<CatalogueStore>((set, get) => ({
  paneOpen: false,
  query: "",
  grains: [],
  loading: false,
  answer: null,
  error: null,
  acquired: {},
  acquiring: null,
  downloads: {},
  courseProgress: {},
  courses: {},

  noteDownloadProgress: (progress) =>
    set((state) => ({
      downloads: { ...state.downloads, [progress.url]: progress },
    })),

  noteCourseProgress: (progress) =>
    set((state) => ({
      courseProgress: { ...state.courseProgress, [progress.course_url]: progress },
    })),

  openPane: () => set({ paneOpen: true }),
  closePane: () => set({ paneOpen: false }),
  setQuery: (query) => set({ query }),

  toggleGrain: (grain) => {
    const grains = get().grains;
    const next = grains.includes(grain)
      ? grains.filter((g) => g !== grain)
      : [...grains, grain];
    set({ grains: next });
    const query = get().query.trim();
    if (query) void get().search(query);
  },

  search: async (query) => {
    const trimmed = query.trim();
    if (!trimmed) {
      set({ answer: null, error: null, loading: false });
      return;
    }
    set({ loading: true, error: null, query });
    try {
      const grains = get().grains;
      const response = await api.catalogueSearch([
        { key: "pane", text: trimmed, grains: grains.length ? grains : null },
      ]);
      const result = response.results.find((r) => r.key === "pane");
      set({
        loading: false,
        answer: result
          ? { query: trimmed, terms: result.terms, hits: result.hits }
          : { query: trimmed, terms: [], hits: [] },
      });
    } catch (e: any) {
      set({ loading: false, error: e?.toString?.() ?? "Catalogue search failed" });
    }
  },

  acquire: async (hit) => {
    if (hit.pdf_url === null) return null;
    const key = hitKey(hit);
    set({ acquiring: key, error: null });
    try {
      const download = await api.catalogueAcquire(hit.pdf_url);
      set((state) => {
        const { [hit.pdf_url as string]: _finished, ...downloads } = state.downloads;
        return {
          acquiring: null,
          downloads,
          acquired: { ...state.acquired, [key]: download.path },
        };
      });
      return download.path;
    } catch (e: any) {
      set((state) => {
        const { [hit.pdf_url as string]: _abandoned, ...downloads } = state.downloads;
        return {
          acquiring: null,
          downloads,
          error: e?.toString?.() ?? "Could not fetch that document",
        };
      });
      return null;
    }
  },

  acquireCourse: async (hit) => {
    const courseUrl = hit.landing_url;
    if (hit.acquisition !== "course" || courseUrl === null) return null;
    const key = hitKey(hit);
    set({ acquiring: key, error: null });
    try {
      const course = await api.catalogueAcquireCourse(courseUrl);
      set((state) => {
        const { [courseUrl]: _finished, ...courseProgress } = state.courseProgress;
        return {
          acquiring: null,
          courseProgress,
          courses: { ...state.courses, [key]: course },
          acquired: { ...state.acquired, [key]: course.directory },
        };
      });
      // The generated document leads: it holds the syllabus and the reading
      // list, and it is what turns the rest of the list into a course.
      return {
        folder: course.folder,
        paths: [course.document, ...course.documents.map((d) => d.path)],
      };
    } catch (e: any) {
      set((state) => {
        const { [courseUrl]: _abandoned, ...courseProgress } = state.courseProgress;
        return {
          acquiring: null,
          courseProgress,
          error: e?.toString?.() ?? "Could not fetch that course",
        };
      });
      return null;
    }
  },

  reset: () =>
    set({
      query: "",
      answer: null,
      error: null,
      loading: false,
      acquired: {},
      acquiring: null,
      downloads: {},
      courseProgress: {},
      courses: {},
    }),
}));
