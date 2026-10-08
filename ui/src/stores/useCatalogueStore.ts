import { create } from "zustand";
import { api } from "../services";
import type {
  BrowseRequest,
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
  /** How far each provider has been read: the last page fetched, and whether
   *  it was the last there is. Keyed by provider id. */
  pages: Record<string, LiteraturePage>;
}

export interface LiteraturePage {
  page: number;
  exhausted: boolean;
}

/** Works per provider per page of a keyword search. Sent explicitly, because
 *  a page shorter than the limit asked for is how the last one is known. */
export const LITERATURE_LIMIT = 10;

/** Works per page of a browse. A page is a list of things to consider, like
 *  the rest of this pane, so it is short; the next is one press away. */
export const BROWSE_LIMIT = 10;

/**
 * What one provider's listing returned, kept apart from the search answers.
 *
 * A browse is a different question from a search — filters and a sort rather
 * than a query — asked of one provider at a time, and its pages accumulate as
 * the user reads on. Folding it into `LiteratureAnswer` would have meant a
 * search that held some other question's results.
 */
export interface BrowseAnswer {
  provider: string;
  name: string;
  /** Every page so far, in the order the provider returned them. */
  results: LiteratureSearchResult[];
  /** The last page fetched, 1-based. */
  page: number;
  /** The last page was short, so there is no next one. */
  exhausted: boolean;
  /** The request this answer belongs to, so the next page asks the same one. */
  request: BrowseRequest;
}

/**
 * How far one row's add has got.
 *
 * Adding is three calls, not one: a provider may have to be asked where the
 * file is, the file has to be fetched, and the fetched file has to be moved
 * into the library root. Only the middle one reports bytes, so a row that
 * showed only the byte stream said nothing at all for the first and last —
 * and for the gap between the request going out and the first byte arriving.
 *
 * `useCatalogueAdd` is the only writer: it is the only thing that knows the
 * whole act. The store holds the stage so that a row can render it without
 * the pane threading it down.
 */
export type AddStage = "resolving" | "fetching" | "importing";

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
  /** The providers whose next page is on its way. */
  literatureMore: string[];
  /** Why a provider's next page failed, by provider id. The pages already
   *  read stay where they are. */
  literatureMoreError: Record<string, string>;
  literature: LiteratureAnswer | null;
  /** The request as a whole failed. A single provider failing is not this:
   *  it is reported in that provider's own entry. */
  literatureError: string | null;
  /** Whether the browse panel is open. A display choice: closing it hides the
   *  controls and keeps what they were set to. */
  browseOpen: boolean;
  /** The provider being browsed, by id. */
  browseProvider: string | null;
  /** Filter values by the names the provider's manifest gives them. A blank
   *  value is no filter and is never stored. */
  browseFilters: Record<string, string>;
  /** The sort chosen, or null for the provider's own default. */
  browseSort: string | null;
  browseLoading: boolean;
  browseAnswer: BrowseAnswer | null;
  /** Why the last browse failed. A failed *next page* leaves the pages already
   *  read where they are. */
  browseError: string | null;
  /** Why the last add failed, whichever of its three calls failed. Kept apart
   *  from the search errors so a failed download does not replace the results
   *  the user was choosing from. */
  acquireError: string | null;
  /** Path in the uploads directory, per candidate key, for what has been
   *  fetched in this session. Keyed so a row can say "added" without the pane
   *  having to re-read the directory. */
  acquired: Record<string, string>;
  /** How far each add in flight has got, by candidate key. Keyed like the two
   *  progress maps below, and for the same reason: nothing stops two rows from
   *  being added at once, and a single key meant the second add to start
   *  erased the first row's indicator. */
  adding: Record<string, AddStage>;
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
  /** Marks how far one row's add has got, or clears it when the act is over.
   *  Written by `useCatalogueAdd` and nothing else. */
  setAddStage: (key: string, stage: AddStage | null) => void;
  /** Why an add failed, or null when one begins. Also `useCatalogueAdd`'s:
   *  one reporter for the whole act, so a resolve that failed and a move that
   *  failed reach the user by the same route a download does. */
  noteAddError: (message: string | null) => void;
  openPane: () => void;
  closePane: () => void;
  setQuery: (query: string) => void;
  toggleGrain: (grain: CatalogueGrain) => void;
  toggleProvider: (provider: string) => void;
  toggleSection: (section: PaneSection) => void;
  setBrowseOpen: (open: boolean) => void;
  /** Switches the provider being browsed. Filters belong to a provider's
   *  manifest, so a different provider starts from none. */
  selectBrowseProvider: (provider: string) => void;
  setBrowseFilter: (name: string, value: string) => void;
  /** Drops one declared filter, both ends of a range included. */
  clearBrowseFilter: (name: string) => void;
  setBrowseSort: (sort: string | null) => void;
  /** Drops every filter and the sort, back to plain search. */
  resetRefinement: () => void;
  /** Brings the provider's listing in line with the filters and sort now set,
   *  with `keyword` beside them when its listing takes one. With none set the
   *  listing goes and the provider is searched by keyword like the others. */
  refine: (keyword: string) => Promise<void>;
  /** The next page of the listing already held. */
  browseMore: () => Promise<void>;
  /** Reads the provider list and prunes any stored selection against it, so a
   *  filter that outlived the provider it named does not refuse every search. */
  loadProviders: () => Promise<void>;
  /** The next page of one provider's keyword answer, appended to what it has
   *  already said. */
  loadMoreLiterature: (provider: string) => Promise<void>;
  /** Runs the query afresh against both halves, each as its filter says. */
  search: (query: string) => Promise<void>;
  /** Fetches one file into uploads. Returns the staged path, and raises if it
   *  could not be fetched — the caller drives the whole add and is the one
   *  place that says what became of it. */
  acquire: (item: Acquirable) => Promise<string>;
  /** Fetches a whole course. Returns the folder it belongs in and the paths
   *  to import — the generated document first, because it is the thing that
   *  makes the rest a course. Null means the hit is not a course at all,
   *  which is a caller's mistake rather than a failed fetch; a failed fetch
   *  raises. */
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
      LITERATURE_LIMIT,
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
    const pages: Record<string, LiteraturePage> =
      held !== null && held.query === query ? { ...held.pages } : {};
    for (const entry of response.providers) {
      if (entry.results !== null) {
        pages[entry.provider] = {
          page: 1,
          exhausted: entry.results.length < LITERATURE_LIMIT,
        };
      }
    }
    set({
      literatureLoading: false,
      literatureMoreError: {},
      literature: {
        query: response.query,
        asked,
        pages,
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

/** The works of a later page not already on the list. A service that ranks by
 *  a score with ties can return the same work on two pages, and a row listed
 *  twice is two rows with one key. */
function unseen(
  held: LiteratureSearchResult[],
  fetched: LiteratureSearchResult[],
): LiteratureSearchResult[] {
  const known = new Set(held.map((work) => work.id));
  return fetched.filter((work) => !known.has(work.id));
}

function omitKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  const { [key]: _removed, ...rest } = record;
  return rest;
}

let browseRequest = 0;

/** The provider the panel is browsing: the one chosen, else the first enabled
 *  one that offers a listing. Null when none does. */
function browseProviderOf(state: CatalogueStore): string | null {
  const offered = browseProviderIds(state.providerOptions);
  if (state.browseProvider !== null && offered.includes(state.browseProvider)) {
    return state.browseProvider;
  }
  return offered[0] ?? null;
}

/** The ids of the enabled providers whose listing can be refined — one that
 *  declares at least a sort or a filter — in registry order. A listing with
 *  nothing to choose is a search with no query, and offers nothing the keyword
 *  box does not. */
export function browseProviderIds(options: LiteratureProviderInfo[]): string[] {
  return options
    .filter((option) => {
      const browse = option.capabilities.browse;
      return (
        option.enabled &&
        browse !== null &&
        (browse.sort !== null || browse.filters.length > 0)
      );
    })
    .map((option) => option.provider);
}

/** The declared filter a stored value belongs to: a range stores its two ends
 *  as `name.min` and `name.max`. */
function filterNameOf(key: string): string {
  return key.replace(/\.(min|max)$/, "");
}

/** How many refinements are set: filters by declared name, plus a chosen sort.
 *  A range with both ends is one. */
export function refinementCount(state: {
  browseFilters: Record<string, string>;
  browseSort: string | null;
}): number {
  const names = new Set(Object.keys(state.browseFilters).map(filterNameOf));
  return names.size + (state.browseSort !== null ? 1 : 0);
}

/**
 * The provider whose results are currently a filtered listing, or null when
 * the pane is in plain search.
 *
 * A listing replaces that provider's keyword answer rather than sitting beside
 * it: both are the same provider's works for the same keyword, and two lists
 * of them would be two answers to one question. It applies only while the
 * provider is one the user has selected above.
 */
export function refinedProvider(state: CatalogueStore): string | null {
  if (refinementCount(state) === 0) return null;
  const provider = browseProviderOf(state);
  if (provider === null || !selects(state.providers, provider)) return null;
  return provider;
}

/** The first page of the listing for what is set now. A keyword only goes to a
 *  provider whose listing takes one: sending it to another would be refused by
 *  name, and what was typed in the search box is not a filter that provider was
 *  asked to apply. */
function browseRequestFor(
  state: CatalogueStore,
  info: LiteratureProviderInfo | undefined,
  keyword: string,
): BrowseRequest {
  return {
    query: info?.capabilities.browse?.keyword && keyword ? keyword : null,
    filters: state.browseFilters,
    sort: state.browseSort,
    page: 1,
    limit: BROWSE_LIMIT,
  };
}

/** Asks for one page. `held` is the answer to extend, or null for a fresh
 *  listing that replaces whatever was shown. */
async function runBrowse(
  set: (partial: Partial<CatalogueStore>) => void,
  provider: string,
  name: string,
  request: BrowseRequest,
  held: BrowseAnswer | null,
): Promise<void> {
  const token = ++browseRequest;
  set({ browseLoading: true, browseError: null });
  try {
    const answer = await api.literatureBrowse(provider, request);
    if (token !== browseRequest) return;
    if (answer.error !== null || answer.results === null) {
      // The provider answered, and refused: a request its manifest does not
      // allow, or a service that is down. Pages already read stay.
      set({
        browseLoading: false,
        browseError: answer.error ?? "the answer carried neither results nor an error",
        browseAnswer: held,
      });
      return;
    }
    set({
      browseLoading: false,
      browseAnswer: {
        provider,
        name,
        results: [...(held?.results ?? []), ...unseen(held?.results ?? [], answer.results)],
        page: request.page ?? 1,
        exhausted: answer.results.length < (request.limit ?? BROWSE_LIMIT),
        request,
      },
    });
  } catch (e: any) {
    if (token !== browseRequest) return;
    set({
      browseLoading: false,
      browseError: e?.toString?.() ?? "Browse failed",
      browseAnswer: held,
    });
  }
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
  literatureMore: [],
  literatureMoreError: {},
  literature: null,
  literatureError: null,
  browseOpen: false,
  browseProvider: null,
  browseFilters: {},
  browseSort: null,
  browseLoading: false,
  browseAnswer: null,
  browseError: null,
  acquireError: null,
  acquired: {},
  adding: {},
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

  setAddStage: (key, stage) =>
    set((state) => {
      if (stage !== null) return { adding: { ...state.adding, [key]: stage } };
      const { [key]: _done, ...adding } = state.adding;
      return { adding };
    }),

  noteAddError: (message) => set({ acquireError: message }),

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
    // The listing follows its provider's chip: switching the provider off
    // hides it and switching it on asks for it, as it would a search.
    if (provider === browseProviderOf(get())) void get().refine(get().query);
    const query = get().query.trim();
    if (!query) return;
    if (!selectsAny(next)) {
      literatureRequest++;
      set({ literature: null, literatureError: null, literatureLoading: false });
      return;
    }
    // Narrowing is free: the answers already held cover it, and the pane
    // shows the subset. Only a provider that has not been asked this query
    // costs a request, and then only that provider is asked. A provider shown
    // as a filtered listing is not one of them.
    const listed = refinedProvider(get());
    const missing = unasked(get().literature, query, next, enabled).filter(
      (id) => id !== listed,
    );
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

  setBrowseOpen: (open) => set({ browseOpen: open }),

  selectBrowseProvider: (provider) => {
    if (provider === get().browseProvider) return;
    browseRequest++;
    set({
      browseProvider: provider,
      browseFilters: {},
      browseSort: null,
      browseAnswer: null,
      browseError: null,
      browseLoading: false,
    });
  },

  setBrowseFilter: (name, value) =>
    set((state) => {
      const { [name]: _removed, ...rest } = state.browseFilters;
      return { browseFilters: value === "" ? rest : { ...rest, [name]: value } };
    }),

  clearBrowseFilter: (name) =>
    set((state) => ({
      browseFilters: Object.fromEntries(
        Object.entries(state.browseFilters).filter(([key]) => filterNameOf(key) !== name),
      ),
    })),

  setBrowseSort: (sort) => set({ browseSort: sort }),

  resetRefinement: () => set({ browseFilters: {}, browseSort: null }),

  refine: async (keyword) => {
    const trimmed = keyword.trim();
    set({ query: keyword });
    const provider = refinedProvider(get());
    if (provider === null) {
      browseRequest++;
      set({ browseAnswer: null, browseError: null, browseLoading: false });
      // The provider goes back to the keyword search it was left out of.
      if (!trimmed) return;
      const enabled = enabledProviderIds(get().providerOptions);
      const missing = unasked(get().literature, trimmed, get().providers, enabled);
      if (missing.length > 0) await searchLiterature(set, get, trimmed, missing, true);
      return;
    }
    const info = get().providerOptions.find((option) => option.provider === provider);
    await runBrowse(set, provider, info?.name ?? provider, browseRequestFor(get(), info, trimmed), null);
  },

  browseMore: async () => {
    const held = get().browseAnswer;
    if (held === null || held.exhausted) return;
    await runBrowse(
      set,
      held.provider,
      held.name,
      { ...held.request, page: held.page + 1 },
      held,
    );
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

  loadMoreLiterature: async (provider) => {
    const held = get().literature;
    const progress = held?.pages[provider];
    if (held === null || progress === undefined || progress.exhausted) return;
    if (get().literatureMore.includes(provider)) return;
    const request = literatureRequest;
    const next = progress.page + 1;
    set((state) => ({
      literatureMore: [...state.literatureMore, provider],
      literatureMoreError: omitKey(state.literatureMoreError, provider),
    }));
    const settle = (partial: Partial<CatalogueStore>) =>
      set((state) => ({
        ...partial,
        literatureMore: state.literatureMore.filter((id) => id !== provider),
      }));
    try {
      const response = await api.literatureSearch(held.query, LITERATURE_LIMIT, [provider], next);
      // A new question was asked while this one was out: its answer belongs to
      // the old one.
      if (request !== literatureRequest) return settle({});
      const entry = response.providers.find((candidate) => candidate.provider === provider);
      if (entry === undefined || entry.results === null) {
        return settle({
          literatureMoreError: {
            ...get().literatureMoreError,
            [provider]: entry?.error ?? "the answer carried neither results nor an error",
          },
        });
      }
      const fetched = entry.results;
      const current = get().literature;
      if (current === null || current.query !== held.query) return settle({});
      settle({
        literature: {
          ...current,
          pages: {
            ...current.pages,
            [provider]: { page: next, exhausted: fetched.length < LITERATURE_LIMIT },
          },
          providers: current.providers.map((existing) =>
            existing.provider === provider
              ? {
                  ...existing,
                  results: [...(existing.results ?? []), ...unseen(existing.results ?? [], fetched)],
                }
              : existing,
          ),
        },
      });
    } catch (e: any) {
      if (request !== literatureRequest) return settle({});
      settle({
        literatureMoreError: {
          ...get().literatureMoreError,
          [provider]: e?.toString?.() ?? "The next page could not be read",
        },
      });
    }
  },

  search: async (query) => {
    const trimmed = query.trim();
    const listed = refinedProvider(get());
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
      // Filters need no keyword: that is what browsing is.
      if (listed !== null) await get().refine(query);
      else {
        browseRequest++;
        set({ browseAnswer: null, browseError: null, browseLoading: false });
      }
      return;
    }
    set({ query, acquireError: null });
    const { grains, providers, providerOptions } = get();
    // The provider shown as a filtered listing is asked by the listing, with
    // the keyword beside the filters, and left out of the keyword search.
    const searched =
      listed === null
        ? providers
        : (providers ?? enabledProviderIds(providerOptions)).filter((id) => id !== listed);
    const searchesLiterature = selectsAny(searched);
    // Both halves run at once and land independently: the mirror is local and
    // answers in milliseconds, and it should not wait on a remote index. A
    // fresh query replaces what the providers said rather than merging into
    // it — the held answers belong to the question that was asked before.
    await Promise.all([
      selectsAny(grains) ? searchCatalogue(set, trimmed, grains) : null,
      searchesLiterature ? searchLiterature(set, get, trimmed, searched, false) : null,
      listed !== null ? get().refine(query) : null,
    ]);
    if (listed === null) {
      browseRequest++;
      set({ browseAnswer: null, browseError: null, browseLoading: false });
    }
    if (!selectsAny(grains)) set({ answer: null, error: null, loading: false });
    if (!searchesLiterature) {
      set({ literature: null, literatureError: null, literatureLoading: false });
    }
  },

  acquire: async ({ key, url, filename }) => {
    try {
      const download =
        filename === undefined
          ? await api.catalogueAcquire(url)
          : await api.catalogueAcquire(url, filename);
      set((state) => {
        const { [url]: _finished, ...downloads } = state.downloads;
        return {
          downloads,
          acquired: { ...state.acquired, [key]: download.path },
        };
      });
      return download.path;
    } catch (error) {
      // The byte counter is this call's own and is dropped whichever way the
      // call ends. Saying what went wrong is the caller's: it owns the stage
      // this failed in, and there are two other stages that can fail too.
      set((state) => {
        const { [url]: _abandoned, ...downloads } = state.downloads;
        return { downloads };
      });
      throw error;
    }
  },

  acquireCourse: async (hit) => {
    const courseUrl = hit.landing_url;
    if (hit.acquisition !== "course" || courseUrl === null) return null;
    const key = hitKey(hit);
    try {
      const course = await api.catalogueAcquireCourse(courseUrl);
      set((state) => {
        const { [courseUrl]: _finished, ...courseProgress } = state.courseProgress;
        return {
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
    } catch (error) {
      set((state) => {
        const { [courseUrl]: _abandoned, ...courseProgress } = state.courseProgress;
        return { courseProgress };
      });
      throw error;
    }
  },

  /** Clears the question and everything it produced. The filters are not
   *  touched: they are the user's standing choice about which sources to ask,
   *  which is why they outlive a session at all. */
  reset: () => {
    catalogueRequest++;
    literatureRequest++;
    browseRequest++;
    set({
      query: "",
      answer: null,
      error: null,
      loading: false,
      literature: null,
      literatureError: null,
      literatureLoading: false,
      literatureMore: [],
      literatureMoreError: {},
      browseAnswer: null,
      browseError: null,
      browseLoading: false,
      acquireError: null,
      acquired: {},
      adding: {},
      downloads: {},
      courseProgress: {},
      courses: {},
    });
  },
}));
