import { create } from "zustand";
import { api } from "../services";
import type {
  BrowseRequest,
  CatalogueCourse,
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  CatalogueGrain,
  LiteratureProviderAnswer,
  LiteratureProviderInfo,
  LiteratureSearchResult,
} from "../lib/types";

/** Where the pane's standing choices live. Still named for the filters it
 *  first held, so that upgrading does not silently reset one. */
const PREFERENCES_STORAGE_KEY = "wilkes.catalogue.filters";

/**
 * Which sources the pane asks.
 *
 * `null` is *every one of them, including any added later* — the default, and
 * not the same as a selection that happens to name them all: a provider the
 * user enables tomorrow, or a catalogue they fetch, joins a `null` selection
 * and would be missing from an enumerated one. An empty array is the other
 * end: nothing selected, so nothing is searched at all.
 *
 * One selection, because there is one kind of source. A learning catalogue is
 * a provider like any other, answered from a copy on this machine; the grain
 * filter this pane once had beside the provider filter named the same four
 * catalogues a second way.
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

/**
 * What the pane stored before a catalogue was a provider: a grain filter over
 * the mirror beside the provider filter, which then named no catalogue.
 *
 * Read once, to be turned into today's one selection when the provider list
 * arrives ([`migrated`]) — the list is what says which provider serves which
 * grain. Undefined when what was stored is already today's shape.
 */
type LegacyGrains = { grains: Selection<CatalogueGrain> } | undefined;

interface StoredPreferences {
  providers: Selection<string>;
  legacy: LegacyGrains;
}

const GRAINS: CatalogueGrain[] = ["textbook", "course", "reference"];

function readPreferences(): StoredPreferences {
  const empty: StoredPreferences = { providers: null, legacy: undefined };
  if (typeof localStorage === "undefined") return empty;
  try {
    const raw = localStorage.getItem(PREFERENCES_STORAGE_KEY);
    if (raw === null) return empty;
    const parsed = JSON.parse(raw) as unknown;
    if (typeof parsed !== "object" || parsed === null) return empty;
    const stored = parsed as { providers?: unknown; grains?: unknown };
    return {
      providers: Array.isArray(stored.providers)
        ? stored.providers.filter((id): id is string => typeof id === "string")
        : null,
      legacy:
        "grains" in stored
          ? {
              grains: Array.isArray(stored.grains)
                ? stored.grains.filter((grain): grain is CatalogueGrain =>
                    GRAINS.includes(grain as CatalogueGrain),
                  )
                : null,
            }
          : undefined,
    };
  } catch (error) {
    console.error("catalogue pane preferences could not be read from storage:", error);
    return empty;
  }
}

function persistPreferences(providers: Selection<string>): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(PREFERENCES_STORAGE_KEY, JSON.stringify({ providers }));
  } catch (error) {
    // The choice still holds for this session; only its memory is lost.
    console.error("catalogue pane preferences could not be stored:", error);
  }
}

/**
 * The one selection the two old filters amount to: the providers that were
 * selected, and the catalogues whose grain was. Null where that is every
 * source that is on, so a source added later still joins it.
 */
function migrated(
  providers: Selection<string>,
  grains: Selection<CatalogueGrain>,
  options: LiteratureProviderInfo[],
): Selection<string> {
  if (providers === null && grains === null) return null;
  const chosen = options
    .filter((option) =>
      option.mirror !== null
        ? selects(grains, option.mirror.grain)
        : selects(providers, option.provider),
    )
    .map((option) => option.provider);
  return enabledProviderIds(options).every((id) => chosen.includes(id)) ? null : chosen;
}

/**
 * What the providers answered to one query.
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
  /** How many works match in all, when the provider says. */
  total: number | null;
  /** The request this answer belongs to, so the next page asks the same one. */
  request: BrowseRequest;
}

/** What one provider is being filtered and sorted by.
 *
 *  Every filter's value is a list, whatever sets it: an on/off filter's is
 *  `["true"]`, a typed one's is what was typed, a multi-valued one's is each
 *  value chosen. An empty list is no filter and is never stored. `modes` is how
 *  a filter's values are combined where that is not the filter's first mode.
 *  `labels` is what a chosen value was called when it was chosen — an author's
 *  name for an author's id — kept because the request carries only the value
 *  and the chip has to say something a person can read. The sort is null for
 *  the provider's default. */
export interface Refinement {
  filters: Record<string, string[]>;
  modes: Record<string, string>;
  labels: Record<string, Record<string, string>>;
  sort: string | null;
}

/** A provider's listing: what it returned so far, whether a page is on its
 *  way, and why the last request failed. A failed *next page* leaves the pages
 *  already read where they are. */
export interface Listing {
  answer: BrowseAnswer | null;
  loading: boolean;
  error: string | null;
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
  /** Which providers are asked. Held by id; the names come from
   *  `providerOptions`, which is the registry's answer and not this store's. */
  providers: Selection<string>;
  /** The grain filter an older build stored, until the provider list arrives
   *  to say which catalogues it meant. */
  legacy: LegacyGrains;
  /** Every provider this installation knows — the learning catalogues and the
   *  remote indexes, on or off — as the backend registry orders them. Empty
   *  until `loadProviders` has answered. */
  providerOptions: LiteratureProviderInfo[];
  /** Why the provider list could not be read. The pane says so rather than
   *  showing an empty filter row that looks like "no providers configured". */
  providerOptionsError: string | null;
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
  /** What each provider is being filtered and sorted by, keyed by provider id.
   *  Held per provider because what can be filtered is that provider's own
   *  declaration, and several can be filtered at once. A provider with no
   *  entry, or an empty one, is searched by keyword like any other. */
  refinements: Record<string, Refinement>;
  /** What each filtered provider's listing returned, and how its request is
   *  going, keyed by provider id. */
  listings: Record<string, Listing>;
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
  /** What each acquired course produced, per result key: the directory, the
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
  toggleProvider: (provider: string) => void;
  setBrowseOpen: (open: boolean) => void;
  /** Sets one filter of one provider to these values; none is no filter.
   *  `labels` says what the values are called, for values a person cannot
   *  read. */
  setBrowseFilter: (
    provider: string,
    name: string,
    values: string[],
    labels?: Record<string, string>,
  ) => void;
  /** Chooses how one filter's values are combined. */
  setBrowseMode: (provider: string, name: string, mode: string) => void;
  /** Drops one filter of a provider, with its mode and its labels. */
  clearBrowseFilter: (provider: string, name: string) => void;
  setBrowseSort: (provider: string, sort: string | null) => void;
  /** Drops the filters and sort of one provider, or of every provider. */
  resetRefinement: (provider?: string) => void;
  /** Brings listings in line with the filters now set — one provider's, or all
   *  of them — with `keyword` beside the filters wherever a listing takes one.
   *  A provider left with none stops being a listing and is searched by
   *  keyword like the others. */
  refine: (keyword: string, provider?: string) => Promise<void>;
  /** The next page of one provider's listing. */
  browseMore: (provider: string) => Promise<void>;
  /** Reads the provider list and prunes any stored selection against it, so a
   *  filter that outlived the provider it named does not refuse every search. */
  loadProviders: () => Promise<void>;
  /** The next page of one provider's keyword answer, appended to what it has
   *  already said. */
  loadMoreLiterature: (provider: string) => Promise<void>;
  /** Runs the query afresh against every selected provider. */
  search: (query: string) => Promise<void>;
  /** Fetches one file into uploads. Returns the staged path, and raises if it
   *  could not be fetched — the caller drives the whole add and is the one
   *  place that says what became of it. */
  acquire: (item: Acquirable) => Promise<string>;
  /** Fetches a whole course from its landing page. Returns the folder it
   *  belongs in and the paths to import — the generated document first,
   *  because it is the thing that makes the rest a course. Raises if it could
   *  not be fetched. */
  acquireCourse: (
    key: string,
    courseUrl: string,
  ) => Promise<{ folder: string; paths: string[] }>;
  reset: () => void;
}

/** The key of a result: unique per provider, not across providers. */
export function paperKey(provider: string, work: LiteratureSearchResult): string {
  return `literature:${provider}:${work.id}`;
}

// A search records the request it belongs to. A remote provider can take
// seconds to answer, and a reply to a query the user has since replaced must
// not overwrite the answer to the new one.
let literatureRequest = 0;

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

/** The ids of the providers that are on, in registry order: a remote one the
 *  user enabled, a catalogue the mirror holds. One that is off is never asked,
 *  so it is never part of a resolved selection. */
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

/** A refinement with one filter gone: its values, its mode and its labels. */
function withoutFilter(refinement: Refinement, name: string): Refinement {
  return {
    ...refinement,
    filters: omitKey(refinement.filters, name),
    modes: omitKey(refinement.modes, name),
    labels: omitKey(refinement.labels, name),
  };
}

function omitKey<T>(record: Record<string, T>, key: string): Record<string, T> {
  const { [key]: _removed, ...rest } = record;
  return rest;
}

/** In-flight listing requests per provider, so a stale answer is recognised. */
const browseRequests: Record<string, number> = {};

type StoreSet = (
  partial: Partial<CatalogueStore> | ((state: CatalogueStore) => Partial<CatalogueStore>),
) => void;

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

export const NO_REFINEMENT: Refinement = { filters: {}, modes: {}, labels: {}, sort: null };
const IDLE_LISTING: Listing = { answer: null, loading: false, error: null };

/** How many refinements one provider has set: its filters, plus a chosen
 *  sort. A filter with several values is one. */
export function refinementCount(refinement: Refinement | undefined): number {
  if (refinement === undefined) return 0;
  return Object.keys(refinement.filters).length + (refinement.sort !== null ? 1 : 0);
}

/**
 * The providers whose results are currently a filtered listing, in registry
 * order.
 *
 * A listing replaces that provider's keyword answer rather than sitting beside
 * it: both are the same provider's works for the same keyword, and two lists
 * of them would be two answers to one question. It applies only while the
 * provider is enabled and one the user has selected above, so a provider that
 * is off simply has no listing and nothing needs saying about it.
 */
export function refinedProviderIds(state: {
  refinements: Record<string, Refinement>;
  providerOptions: LiteratureProviderInfo[];
  providers: Selection<string>;
}): string[] {
  return browseProviderIds(state.providerOptions).filter(
    (id) => selects(state.providers, id) && refinementCount(state.refinements[id]) > 0,
  );
}

/** How many refinements are in force across every listed provider. */
export function totalRefinements(state: {
  refinements: Record<string, Refinement>;
  providerOptions: LiteratureProviderInfo[];
  providers: Selection<string>;
}): number {
  return refinedProviderIds(state).reduce(
    (total, id) => total + refinementCount(state.refinements[id]),
    0,
  );
}

function patchListing(set: StoreSet, provider: string, patch: Partial<Listing>): void {
  set((state) => ({
    listings: {
      ...state.listings,
      [provider]: { ...(state.listings[provider] ?? IDLE_LISTING), ...patch },
    },
  }));
}

/** Forgets a provider's listing and ignores any request still out for it. */
function dropListing(set: StoreSet, provider: string): void {
  browseRequests[provider] = (browseRequests[provider] ?? 0) + 1;
  set((state) => ({ listings: omitKey(state.listings, provider) }));
}

/** The first page of a provider's listing for what is set now. A keyword only
 *  goes to a provider whose listing takes one: sending it to another would be
 *  refused by name, and what was typed in the search box is not a filter that
 *  provider was asked to apply. */
function browseRequestFor(
  refinement: Refinement,
  info: LiteratureProviderInfo | undefined,
  keyword: string,
): BrowseRequest {
  return {
    query: info?.capabilities.browse?.keyword && keyword ? keyword : null,
    filters: refinement.filters,
    // Only for a filter that is set: a mode with no values is not a request.
    modes: Object.fromEntries(
      Object.entries(refinement.modes).filter(([name]) => name in refinement.filters),
    ),
    sort: refinement.sort,
    page: 1,
    limit: BROWSE_LIMIT,
  };
}

/** Asks for one page. `held` is the answer to extend, or null for a fresh
 *  listing that replaces whatever was shown. */
async function runBrowse(
  set: StoreSet,
  provider: string,
  name: string,
  request: BrowseRequest,
  held: BrowseAnswer | null,
): Promise<void> {
  const token = (browseRequests[provider] = (browseRequests[provider] ?? 0) + 1);
  patchListing(set, provider, { loading: true, error: null });
  try {
    const answer = await api.literatureBrowse(provider, request);
    if (token !== browseRequests[provider]) return;
    if (answer.error !== null || answer.results === null) {
      // The provider answered, and refused: a request its manifest does not
      // allow, or a service that is down. Pages already read stay.
      patchListing(set, provider, {
        loading: false,
        error: answer.error ?? "the answer carried neither results nor an error",
        answer: held,
      });
      return;
    }
    patchListing(set, provider, {
      loading: false,
      answer: {
        provider,
        name,
        results: [...(held?.results ?? []), ...unseen(held?.results ?? [], answer.results)],
        page: request.page ?? 1,
        exhausted: answer.results.length < (request.limit ?? BROWSE_LIMIT),
        total: answer.total ?? null,
        request,
      },
    });
  } catch (e: any) {
    if (token !== browseRequests[provider]) return;
    patchListing(set, provider, {
      loading: false,
      error: e?.toString?.() ?? "Browse failed",
      answer: held,
    });
  }
}

/**
 * Puts each named provider in the state its filters call for: asked for its
 * listing if it has filters set and is selected, otherwise with no listing.
 * Returns the requests started and the providers that are not listings.
 */
function applyListings(
  set: StoreSet,
  get: () => CatalogueStore,
  keyword: string,
  ids: string[],
): { requests: Promise<void>[]; stopped: string[] } {
  const listed = refinedProviderIds(get());
  const requests: Promise<void>[] = [];
  const stopped: string[] = [];
  for (const id of ids) {
    if (listed.includes(id)) requests.push(listProvider(set, get(), id, keyword));
    else {
      dropListing(set, id);
      stopped.push(id);
    }
  }
  return { requests, stopped };
}

/** Asks one provider for its listing under what is set now. */
function listProvider(
  set: StoreSet,
  state: CatalogueStore,
  provider: string,
  keyword: string,
): Promise<void> {
  const info = state.providerOptions.find((option) => option.provider === provider);
  return runBrowse(
    set,
    provider,
    info?.name ?? provider,
    browseRequestFor(state.refinements[provider] ?? NO_REFINEMENT, info, keyword),
    null,
  );
}

export const useCatalogueStore = create<CatalogueStore>((set, get) => ({
  paneOpen: false,
  query: "",
  ...readPreferences(),
  providerOptions: [],
  providerOptionsError: null,
  literatureLoading: false,
  literatureMore: [],
  literatureMoreError: {},
  literature: null,
  literatureError: null,
  browseOpen: false,
  refinements: {},
  listings: {},
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

  toggleProvider: (provider) => {
    const enabled = enabledProviderIds(get().providerOptions);
    const next = toggled(get().providers, enabled, provider);
    set({ providers: next });
    persistPreferences(next);
    // A filtered listing follows its provider's chip: switching the provider
    // off removes it and switching it on asks for it, as it would a search.
    if (browseProviderIds(get().providerOptions).includes(provider)) {
      void get().refine(get().query, provider);
    }
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
    const listed = refinedProviderIds(get());
    const missing = unasked(get().literature, query, next, enabled).filter(
      (id) => !listed.includes(id),
    );
    if (missing.length > 0) void searchLiterature(set, get, query, missing, true);
  },

  setBrowseOpen: (open) => set({ browseOpen: open }),

  setBrowseFilter: (provider, name, values, labels) =>
    set((state) => {
      const current = state.refinements[provider] ?? NO_REFINEMENT;
      const kept = values.filter((value) => value.trim() !== "");
      if (kept.length === 0) {
        return { refinements: { ...state.refinements, [provider]: withoutFilter(current, name) } };
      }
      // Labels are kept only for values still chosen, and a label given now
      // replaces one remembered.
      const known = { ...(current.labels[name] ?? {}), ...(labels ?? {}) };
      const named = Object.fromEntries(
        kept.filter((value) => value in known).map((value) => [value, known[value]]),
      );
      return {
        refinements: {
          ...state.refinements,
          [provider]: {
            ...current,
            filters: { ...current.filters, [name]: kept },
            labels: { ...current.labels, [name]: named },
          },
        },
      };
    }),

  setBrowseMode: (provider, name, mode) =>
    set((state) => {
      const current = state.refinements[provider] ?? NO_REFINEMENT;
      return {
        refinements: {
          ...state.refinements,
          [provider]: { ...current, modes: { ...current.modes, [name]: mode } },
        },
      };
    }),

  clearBrowseFilter: (provider, name) =>
    set((state) => ({
      refinements: {
        ...state.refinements,
        [provider]: withoutFilter(state.refinements[provider] ?? NO_REFINEMENT, name),
      },
    })),

  setBrowseSort: (provider, sort) =>
    set((state) => ({
      refinements: {
        ...state.refinements,
        [provider]: { ...(state.refinements[provider] ?? NO_REFINEMENT), sort },
      },
    })),

  resetRefinement: (provider) =>
    set((state) => ({
      refinements:
        provider === undefined ? {} : omitKey(state.refinements, provider),
    })),

  refine: async (keyword, provider) => {
    const trimmed = keyword.trim();
    set({ query: keyword });
    const considered =
      provider === undefined ? browseProviderIds(get().providerOptions) : [provider];
    const { requests, stopped } = applyListings(set, get, trimmed, considered);
    const asked = [...requests];
    // A provider that stopped being a listing goes back to the keyword search
    // it was left out of; the others' answers to it are already held.
    if (trimmed && stopped.length > 0) {
      const enabled = enabledProviderIds(get().providerOptions);
      const missing = unasked(get().literature, trimmed, get().providers, enabled).filter(
        (id) => stopped.includes(id),
      );
      if (missing.length > 0) asked.push(searchLiterature(set, get, trimmed, missing, true));
    }
    await Promise.all(asked);
  },

  browseMore: async (provider) => {
    const held = get().listings[provider]?.answer ?? null;
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
      const legacy = get().legacy;
      const stored =
        legacy === undefined ? get().providers : migrated(get().providers, legacy.grains, options);
      // A selection naming a provider that no longer exists would be refused
      // by the backend on every search, so it is pruned here rather than sent.
      const pruned = stored === null ? null : stored.filter((id) => known.includes(id));
      set({
        providerOptions: options,
        providerOptionsError: null,
        providers: pruned,
        legacy: undefined,
      });
      if (
        legacy !== undefined ||
        (stored !== null && pruned !== null && pruned.length !== stored.length)
      ) {
        persistPreferences(pruned);
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
    const listed = refinedProviderIds(get());
    if (!trimmed) {
      literatureRequest++;
      set({
        literature: null,
        literatureError: null,
        literatureLoading: false,
      });
      // Filters need no keyword: that is what browsing is.
      set({ query });
      await Promise.all(applyListings(set, get, "", browseProviderIds(get().providerOptions)).requests);
      return;
    }
    set({ query, acquireError: null });
    const { providers, providerOptions } = get();
    // A provider shown as a filtered listing is asked by the listing, with the
    // keyword beside its filters, and left out of the keyword search.
    const searched =
      listed.length === 0
        ? providers
        : (providers ?? enabledProviderIds(providerOptions)).filter(
            (id) => !listed.includes(id),
          );
    const searchesLiterature = selectsAny(searched);
    // A fresh query replaces what the providers said rather than merging into
    // it — the held answers belong to the question that was asked before.
    await Promise.all([
      searchesLiterature ? searchLiterature(set, get, trimmed, searched, false) : null,
      ...applyListings(set, get, trimmed, browseProviderIds(providerOptions)).requests,
    ]);
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

  acquireCourse: async (key, courseUrl) => {
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
    literatureRequest++;
    for (const provider of Object.keys(browseRequests)) browseRequests[provider]++;
    set({
      query: "",
      literature: null,
      literatureError: null,
      literatureLoading: false,
      literatureMore: [],
      literatureMoreError: {},
      listings: {},
      acquireError: null,
      acquired: {},
      adding: {},
      downloads: {},
      courseProgress: {},
      courses: {},
    });
  },
}));
