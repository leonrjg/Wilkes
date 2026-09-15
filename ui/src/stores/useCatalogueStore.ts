import { create } from "zustand";
import { api } from "../services";
import type {
  CatalogueCourse,
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  CatalogueGrain,
  CatalogueHit,
  LiteratureProviderAnswer,
  LiteratureSearchResult,
} from "../lib/types";

export const ALL_GRAINS: CatalogueGrain[] = ["textbook", "course", "reference"];

/**
 * What the pane's filter chips select between: the three grains the
 * catalogue mirror publishes at, and scholarly papers from the literature
 * providers the user has enabled.
 *
 * One selection rather than two because a person looking for material does
 * not first decide which *system* holds it. The two halves stay separate
 * underneath — a textbook record and a paper record answer different
 * questions and are never merged into one row type.
 */
export type SearchKind = CatalogueGrain | "paper";

export const ALL_KINDS: SearchKind[] = [...ALL_GRAINS, "paper"];

/**
 * One probe's answer, kept apart from the query that produced it.
 *
 * `terms` is what the query reduced to. An empty `hits` with empty `terms` is
 * "nothing in what you typed was searchable", which is a different sentence
 * from "the catalogues hold nothing on this" — and the only way to tell them
 * apart, since the filtering rules live in the store and are not repeated here.
 *
 * `grains` is what the probe was filtered to (empty for all of them), so a
 * chip toggle can tell whether this answer still describes the selection.
 */
export interface CatalogueAnswer {
  query: string;
  grains: CatalogueGrain[];
  terms: string[];
  hits: CatalogueHit[];
}

/** Every enabled literature provider's answer to one query. */
export interface LiteratureAnswer {
  query: string;
  providers: LiteratureProviderAnswer[];
}

/** The grains a kind selection passes to the mirror. Empty means all. */
function selectedGrains(kinds: SearchKind[]): CatalogueGrain[] {
  return kinds.filter((kind): kind is CatalogueGrain => kind !== "paper");
}

/** Whether a selection includes catalogue records at all. No selection means
 *  every kind; a selection of only papers means none of them. */
export function wantsCatalogue(kinds: SearchKind[]): boolean {
  return kinds.length === 0 || selectedGrains(kinds).length > 0;
}

export function wantsLiterature(kinds: SearchKind[]): boolean {
  return kinds.length === 0 || kinds.includes("paper");
}

function sameGrains(a: CatalogueGrain[], b: CatalogueGrain[]): boolean {
  return a.length === b.length && a.every((grain) => b.includes(grain));
}

/** Something the pane can add: its store key and the URL that fetches it. */
export interface Acquirable {
  key: string;
  url: string;
  filename?: string;
}

interface CatalogueStore {
  paneOpen: boolean;
  query: string;
  kinds: SearchKind[];
  loading: boolean;
  answer: CatalogueAnswer | null;
  error: string | null;
  literatureLoading: boolean;
  literature: LiteratureAnswer | null;
  /** The request as a whole failed. A single provider failing is not this:
   *  it is reported in that provider's own entry. */
  literatureError: string | null;
  /** Why the last add failed. Kept apart from the search errors so a failed
   *  download does not replace the results the user was choosing from. */
  acquireError: string | null;
  /** Path in the uploads directory, per candidate key, for what has been
   *  fetched in this session. Keyed so a row can say "added" without the pane
   *  having to re-read the directory. */
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
  toggleKind: (kind: SearchKind) => void;
  /** Runs the query afresh against every selected kind of source. */
  search: (query: string) => Promise<void>;
  /** Fetches one file into uploads. Returns the staged path. */
  acquire: (item: Acquirable) => Promise<string | null>;
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

/** The key of a paper. Prefixed because a literature provider's id and a
 *  catalogue provider's id live in different namespaces that could collide. */
export function paperKey(provider: string, work: LiteratureSearchResult): string {
  return `literature:${provider}:${work.id}`;
}

// Each half of a search records the request it belongs to. A literature
// provider can take seconds to answer, and a reply to a query the user has
// since replaced must not overwrite the answer to the new one.
let catalogueRequest = 0;
let literatureRequest = 0;

async function searchCatalogue(
  set: (partial: Partial<CatalogueStore>) => void,
  query: string,
  grains: CatalogueGrain[],
): Promise<void> {
  const request = ++catalogueRequest;
  set({ loading: true, error: null });
  try {
    const response = await api.catalogueSearch([
      { key: "pane", text: query, grains: grains.length ? grains : null },
    ]);
    if (request !== catalogueRequest) return;
    const result = response.results.find((r) => r.key === "pane");
    set({
      loading: false,
      answer: result
        ? { query, grains, terms: result.terms, hits: result.hits }
        : { query, grains, terms: [], hits: [] },
    });
  } catch (e: any) {
    if (request !== catalogueRequest) return;
    set({ loading: false, answer: null, error: e?.toString?.() ?? "Catalogue search failed" });
  }
}

async function searchLiterature(
  set: (partial: Partial<CatalogueStore>) => void,
  query: string,
): Promise<void> {
  const request = ++literatureRequest;
  set({ literatureLoading: true, literatureError: null });
  try {
    const response = await api.literatureSearch(query);
    if (request !== literatureRequest) return;
    set({
      literatureLoading: false,
      literature: { query: response.query, providers: response.providers },
    });
  } catch (e: any) {
    if (request !== literatureRequest) return;
    set({
      literatureLoading: false,
      literature: null,
      literatureError: e?.toString?.() ?? "Literature search failed",
    });
  }
}

export const useCatalogueStore = create<CatalogueStore>((set, get) => ({
  paneOpen: false,
  query: "",
  kinds: [],
  loading: false,
  answer: null,
  error: null,
  literatureLoading: false,
  literature: null,
  literatureError: null,
  acquireError: null,
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

  toggleKind: (kind) => {
    const kinds = get().kinds;
    const next = kinds.includes(kind) ? kinds.filter((k) => k !== kind) : [...kinds, kind];
    set({ kinds: next });
    const query = get().query.trim();
    if (!query) return;
    // A toggle re-asks only the half whose answer no longer describes the
    // selection. Flipping a textbook chip must not send the query to every
    // literature provider again: those are rate-limited services on the
    // other side of the network, and their answer has not changed.
    const { answer, literature } = get();
    const grains = selectedGrains(next);
    if (
      wantsCatalogue(next) &&
      (answer === null || answer.query !== query || !sameGrains(answer.grains, grains))
    ) {
      void searchCatalogue(set, query, grains);
    }
    if (wantsLiterature(next) && (literature === null || literature.query !== query)) {
      void searchLiterature(set, query);
    }
  },

  search: async (query) => {
    const trimmed = query.trim();
    if (!trimmed) {
      catalogueRequest++;
      literatureRequest++;
      set({
        answer: null,
        error: null,
        loading: false,
        literature: null,
        literatureError: null,
        literatureLoading: false,
      });
      return;
    }
    set({ query, acquireError: null });
    const kinds = get().kinds;
    // Both halves run at once and land independently: the mirror is local and
    // answers in milliseconds, and it should not wait on a remote index.
    await Promise.all([
      wantsCatalogue(kinds) ? searchCatalogue(set, trimmed, selectedGrains(kinds)) : null,
      wantsLiterature(kinds) ? searchLiterature(set, trimmed) : null,
    ]);
  },

  acquire: async ({ key, url, filename }) => {
    set({ acquiring: key, acquireError: null });
    try {
      const download =
        filename === undefined
          ? await api.catalogueAcquire(url)
          : await api.catalogueAcquire(url, filename);
      set((state) => {
        const { [url]: _finished, ...downloads } = state.downloads;
        return {
          acquiring: null,
          downloads,
          acquired: { ...state.acquired, [key]: download.path },
        };
      });
      return download.path;
    } catch (e: any) {
      set((state) => {
        const { [url]: _abandoned, ...downloads } = state.downloads;
        return {
          acquiring: null,
          downloads,
          acquireError: e?.toString?.() ?? "Could not fetch that document",
        };
      });
      return null;
    }
  },

  acquireCourse: async (hit) => {
    const courseUrl = hit.landing_url;
    if (hit.acquisition !== "course" || courseUrl === null) return null;
    const key = hitKey(hit);
    set({ acquiring: key, acquireError: null });
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
          acquireError: e?.toString?.() ?? "Could not fetch that course",
        };
      });
      return null;
    }
  },

  reset: () => {
    catalogueRequest++;
    literatureRequest++;
    set({
      query: "",
      answer: null,
      error: null,
      loading: false,
      literature: null,
      literatureError: null,
      literatureLoading: false,
      acquireError: null,
      acquired: {},
      acquiring: null,
      downloads: {},
      courseProgress: {},
      courses: {},
    });
  },
}));
