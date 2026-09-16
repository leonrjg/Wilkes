import { create } from "zustand";
import { api } from "../services";
import type {
  CatalogueCourse,
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  CatalogueGrain,
  CatalogueHit,
  LiteratureProviderAnswer,
  LiteratureProviderInfo,
  LiteratureSearchResult,
} from "../lib/types";

export const ALL_GRAINS: CatalogueGrain[] = ["textbook", "course", "reference"];

/** Records per catalogue probe. Ten, because the pane is a list of things to
 *  consider acquiring and not a second search result page: a strip long enough
 *  to scroll past is one nobody reads to the end of. */
export const CATALOGUE_LIMIT = 10;

/** Where the pane's standing choices live. Still named for the filters it
 *  first held, so that upgrading does not silently reset one. */
const PREFERENCES_STORAGE_KEY = "wilkes.catalogue.filters";

/** The two halves of the pane, named so a preference can say which it means. */
export type PaneSection = "catalogues" | "literature";

export const ALL_SECTIONS: PaneSection[] = ["catalogues", "literature"];

/**
 * What each half of the pane is filtered to.
 *
 * `null` is *every one of them, including any added later* — the default, and
 * not the same as a selection that happens to name them all: a literature
 * provider the user enables tomorrow joins a `null` selection and would be
 * missing from an enumerated one. An empty array is the other end: nothing
 * selected, so that half is not searched at all.
 *
 * Two selections rather than one, because they filter different systems. The
 * grains are the mirror's own vocabulary and mean nothing to a literature
 * provider; the providers are services the user configured and have no grain.
 * The single merged list this replaced put a "Papers" chip beside "Textbooks"
 * and so had to call every live source a paper, which several of them are not.
 */
export type Selection<T> = T[] | null;

/** Whether a filter admits something. */
export function selects<T>(selection: Selection<T>, item: T): boolean {
  return selection === null || selection.includes(item);
}

/** Whether a filter admits anything at all. */
export function selectsAny<T>(selection: Selection<T>): boolean {
  return selection === null || selection.length > 0;
}

/** Toggling one member of a filter. Turning one off while everything is
 *  selected enumerates the rest; turning the last one back off leaves an empty
 *  selection, which is "search nothing here" and is allowed to be said. */
export function toggled<T>(selection: Selection<T>, all: T[], item: T): Selection<T> {
  const current = selection ?? all;
  return current.includes(item)
    ? current.filter((member) => member !== item)
    : all.filter((member) => current.includes(member) || member === item);
}

function sameSelection<T>(a: Selection<T>, b: Selection<T>): boolean {
  if (a === null || b === null) return a === b;
  return a.length === b.length && a.every((member) => b.includes(member));
}

/**
 * Everything about the pane that outlives one question.
 *
 * Which sources to ask and which half to look at are both standing choices
 * about how this pane is used, so they are one stored object and one pair of
 * helpers rather than a second storage mechanism beside the first.
 *
 * They are not the same kind of choice, though, and the store never conflates
 * them: a filter decides what is *asked*, and collapsing decides what is
 * *shown*. A collapsed section is still searched, so opening it shows the
 * answer to the question that was asked rather than a blank that needs a
 * re-run.
 */
interface PanePreferences {
  grains: Selection<CatalogueGrain>;
  providers: Selection<string>;
  collapsed: PaneSection[];
}

function readPreferences(): PanePreferences {
  const empty: PanePreferences = { grains: null, providers: null, collapsed: [] };
  if (typeof localStorage === "undefined") return empty;
  try {
    const raw = localStorage.getItem(PREFERENCES_STORAGE_KEY);
    if (raw === null) return empty;
    const parsed = JSON.parse(raw) as unknown;
    if (typeof parsed !== "object" || parsed === null) return empty;
    const stored = parsed as Partial<PanePreferences>;
    return {
      grains: Array.isArray(stored.grains)
        ? stored.grains.filter((grain): grain is CatalogueGrain =>
            ALL_GRAINS.includes(grain as CatalogueGrain),
          )
        : null,
      providers: Array.isArray(stored.providers)
        ? stored.providers.filter((id): id is string => typeof id === "string")
        : null,
      collapsed: Array.isArray(stored.collapsed)
        ? stored.collapsed.filter((section): section is PaneSection =>
            ALL_SECTIONS.includes(section as PaneSection),
          )
        : [],
    };
  } catch (error) {
    console.error("catalogue pane preferences could not be read from storage:", error);
    return empty;
  }
}

function persistPreferences(preferences: PanePreferences): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(PREFERENCES_STORAGE_KEY, JSON.stringify(preferences));
  } catch (error) {
    // The choice still holds for this session; only its memory is lost.
    console.error("catalogue pane preferences could not be stored:", error);
  }
}

/**
 * One probe's answer, kept apart from the query that produced it.
 *
 * `terms` is what the query reduced to. An empty `hits` with empty `terms` is
 * "nothing in what you typed was searchable", which is a different sentence
 * from "the catalogues hold nothing on this" — and the only way to tell them
 * apart, since the filtering rules live in the store and are not repeated here.
 *
 * `grains` is what the probe was filtered to, so a chip toggle can tell
 * whether this answer still describes the selection.
 */
export interface CatalogueAnswer {
  query: string;
  grains: Selection<CatalogueGrain>;
  terms: string[];
  hits: CatalogueHit[];
}

/**
 * What the literature providers answered to one query.
 *
 * `asked` is which providers the entries actually cover. It is not derivable
 * from `providers`, because narrowing the filter and then widening it again
 * merges a second request's entries into the first's — and a provider that
 * answered with an error is still one that was asked and must not be asked
 * again by the act of re-selecting a sibling.
 */
export interface LiteratureAnswer {
  query: string;
  asked: string[];
  providers: LiteratureProviderAnswer[];
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
  /** Which catalogue grains the mirror is asked for. */
  grains: Selection<CatalogueGrain>;
  /** Which literature providers are asked. Held by id; the names come from
   *  `providerOptions`, which is the registry's answer and not this store's. */
  providers: Selection<string>;
  /** Which halves of the pane are rolled up. A display choice, not a filter:
   *  a collapsed section is still searched, so opening it shows the answer to
   *  the question that was asked rather than a blank. */
  collapsed: PaneSection[];
  /** Every literature provider this installation knows, enabled ones included
   *  and disabled ones too, as the backend registry orders them. Empty until
   *  `loadProviders` has answered. */
  providerOptions: LiteratureProviderInfo[];
  /** Why the provider list could not be read. The pane says so rather than
   *  showing an empty filter row that looks like "no providers configured". */
  providerOptionsError: string | null;
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
  toggleGrain: (grain: CatalogueGrain) => void;
  toggleProvider: (provider: string) => void;
  toggleSection: (section: PaneSection) => void;
  /** Reads the provider list and prunes any stored selection against it, so a
   *  filter that outlived the provider it named does not refuse every search. */
  loadProviders: () => Promise<void>;
  /** Runs the query afresh against both halves, each as its filter says. */
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
  grains: Selection<CatalogueGrain>,
): Promise<void> {
  const request = ++catalogueRequest;
  set({ loading: true, error: null });
  try {
    const response = await api.catalogueSearch(
      [{ key: "pane", text: query, grains }],
      CATALOGUE_LIMIT,
    );
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

/**
 * Asks the named providers and folds their answers into what is already held.
 *
 * `merge` is what makes widening a filter cheap. Re-selecting a provider that
 * was turned off asks that provider alone; the siblings that already answered
 * keep their entries, because they are rate-limited services on the other side
 * of the network and their answer to this query has not changed.
 */
async function searchLiterature(
  set: (partial: Partial<CatalogueStore>) => void,
  get: () => CatalogueStore,
  query: string,
  providers: Selection<string>,
  merge: boolean,
): Promise<void> {
  const request = ++literatureRequest;
  set({ literatureLoading: true, literatureError: null });
  try {
    const response = await api.literatureSearch(
      query,
      undefined,
      providers === null ? undefined : providers,
    );
    if (request !== literatureRequest) return;
    const held = merge ? get().literature : null;
    const carried =
      held !== null && held.query === query
        ? held.providers.filter(
            (entry) => !response.providers.some((fresh) => fresh.provider === entry.provider),
          )
        : [];
    const askedBefore = held !== null && held.query === query ? held.asked : [];
    // A null filter asked every enabled provider, so what came back *is* what
    // was asked. A named one asked exactly what it named, including a provider
    // that turned out to be disabled and so has no entry — recording it stops
    // a re-selection from asking for it again on every toggle.
    const asked =
      providers === null
        ? response.providers.map((entry) => entry.provider)
        : [...new Set([...askedBefore, ...providers])];
    set({
      literatureLoading: false,
      literature: {
        query: response.query,
        asked,
        // Registry order, not arrival order and not merge order: which
        // provider a reply came from and when it was asked are accidents of
        // the network, and a list that reordered itself as filters were
        // toggled would be a different pane each time.
        providers: inRegistryOrder([...carried, ...response.providers], get().providerOptions),
      },
    });
  } catch (e: any) {
    if (request !== literatureRequest) return;
    set({
      literatureLoading: false,
      // A widening that failed leaves what the other providers already said.
      // Dropping it would take results the user is reading away because a
      // provider they just re-selected could not be reached — and `asked` is
      // not advanced, so re-selecting asks that provider again.
      literature: merge ? get().literature : null,
      literatureError: e?.toString?.() ?? "Literature search failed",
    });
  }
}

/** Provider answers in the order the registry names them, with any provider
 *  the list does not know kept, at the end, rather than dropped. */
function inRegistryOrder(
  answers: LiteratureProviderAnswer[],
  options: LiteratureProviderInfo[],
): LiteratureProviderAnswer[] {
  const rank = (entry: LiteratureProviderAnswer) => {
    const index = options.findIndex((option) => option.provider === entry.provider);
    return index === -1 ? options.length : index;
  };
  return [...answers].sort((a, b) => rank(a) - rank(b));
}

/** The stored half of the store's state, for a writer that is changing one
 *  field of it and must not drop the others. */
function preferences(state: CatalogueStore): PanePreferences {
  return { grains: state.grains, providers: state.providers, collapsed: state.collapsed };
}

/** The ids of the providers the user has switched on, in registry order. A
 *  disabled one is never asked, so it is never part of a resolved selection. */
export function enabledProviderIds(options: LiteratureProviderInfo[]): string[] {
  return options.filter((option) => option.enabled).map((option) => option.provider);
}

/** The providers a selection names that the held answer does not already
 *  cover. Empty means the selection can be honoured without a request. */
function unasked(
  literature: LiteratureAnswer | null,
  query: string,
  providers: Selection<string>,
  enabled: string[],
): string[] {
  if (literature === null || literature.query !== query) return providers ?? enabled;
  const wanted = providers ?? enabled;
  return wanted.filter((id) => !literature.asked.includes(id));
}

export const useCatalogueStore = create<CatalogueStore>((set, get) => ({
  paneOpen: false,
  query: "",
  ...readPreferences(),
  providerOptions: [],
  providerOptionsError: null,
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

  toggleGrain: (grain) => {
    const next = toggled(get().grains, ALL_GRAINS, grain);
    set({ grains: next });
    persistPreferences({ ...preferences(get()), grains: next });
    const query = get().query.trim();
    if (!query) return;
    // A grain toggle touches the mirror alone. It must not send the query to
    // every literature provider again: those are rate-limited services on the
    // other side of the network, and their answer has not changed.
    if (!selectsAny(next)) {
      catalogueRequest++;
      set({ answer: null, error: null, loading: false });
      return;
    }
    const answer = get().answer;
    if (answer === null || answer.query !== query || !sameSelection(answer.grains, next)) {
      void searchCatalogue(set, query, next);
    }
  },

  toggleProvider: (provider) => {
    const enabled = enabledProviderIds(get().providerOptions);
    const next = toggled(get().providers, enabled, provider);
    set({ providers: next });
    persistPreferences({ ...preferences(get()), providers: next });
    const query = get().query.trim();
    if (!query) return;
    if (!selectsAny(next)) {
      literatureRequest++;
      set({ literature: null, literatureError: null, literatureLoading: false });
      return;
    }
    // Narrowing is free: the answers already held cover it, and the pane
    // shows the subset. Only a provider that has not been asked this query
    // costs a request, and then only that provider is asked.
    const missing = unasked(get().literature, query, next, enabled);
    if (missing.length > 0) void searchLiterature(set, get, query, missing, true);
  },

  toggleSection: (section) => {
    const collapsed = get().collapsed;
    const next = collapsed.includes(section)
      ? collapsed.filter((member) => member !== section)
      : [...collapsed, section];
    set({ collapsed: next });
    persistPreferences({ ...preferences(get()), collapsed: next });
  },

  loadProviders: async () => {
    try {
      const options = await api.literatureProviders();
      const known = options.map((option) => option.provider);
      const stored = get().providers;
      // A selection naming a provider that no longer exists would be refused
      // by the backend on every search, so it is pruned here rather than sent.
      const pruned = stored === null ? null : stored.filter((id) => known.includes(id));
      set({ providerOptions: options, providerOptionsError: null, providers: pruned });
      if (stored !== null && pruned !== null && pruned.length !== stored.length) {
        persistPreferences({ ...preferences(get()), providers: pruned });
      }
    } catch (e: any) {
      const message = e?.toString?.() ?? "Literature providers could not be listed";
      console.error("literature providers could not be listed:", e);
      set({ providerOptionsError: message });
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
    const { grains, providers } = get();
    // Both halves run at once and land independently: the mirror is local and
    // answers in milliseconds, and it should not wait on a remote index. A
    // fresh query replaces what the providers said rather than merging into
    // it — the held answers belong to the question that was asked before.
    await Promise.all([
      selectsAny(grains) ? searchCatalogue(set, trimmed, grains) : null,
      selectsAny(providers)
        ? searchLiterature(set, get, trimmed, providers, false)
        : null,
    ]);
    if (!selectsAny(grains)) set({ answer: null, error: null, loading: false });
    if (!selectsAny(providers)) {
      set({ literature: null, literatureError: null, literatureLoading: false });
    }
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

  /** Clears the question and everything it produced. The filters are not
   *  touched: they are the user's standing choice about which sources to ask,
   *  which is why they outlive a session at all. */
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
