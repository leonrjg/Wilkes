import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Search, Sliders, Square, X, Zap } from "react-feather";
import type {
  BrowseControls,
  CatalogueGrain,
  ControlOption,
  FilterControl,
  FilterInput,
  FilterSuggestion,
  LiteratureProviderAnswer,
  LiteratureProviderInfo,
} from "../lib/types";
import {
  browseProviderIds,
  enabledProviderIds,
  type LiteratureAnswer,
  NO_REFINEMENT,
  type Refinement,
  refinedProviderIds,
  refinementCount,
  totalRefinements,
  type Selection,
  selects,
  selectsAny,
  useCatalogueStore,
} from "../stores/useCatalogueStore";
import { useGenerationStore } from "../stores/useGenerationStore";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import { api } from "../services";
import CatalogueCandidate from "./CatalogueCandidate";

/** What a learning catalogue holds, said beside its name: the one thing a
 *  catalogue's name does not say about it. */
const GRAIN_LABELS: Record<CatalogueGrain, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
};

/**
 * Looking for something to add: every source that is on, asked one question at
 * once.
 *
 * Sits with the other ways documents enter a library rather than with the
 * search results: looking for material to acquire is an acquisition, and the
 * pane it opens beside is the file list it will change.
 *
 * The open learning catalogues and the remote indexes are one list of
 * providers here, because they are one kind of thing: each is asked the query,
 * answers under its own name, and is never ranked against another. One filter
 * chooses which of them are asked. What a catalogue has that an index does not
 * — it is answered from this machine, and holds one kind of thing — is said
 * beside its name rather than by a second section with a second filter.
 */
export default function CataloguePane() {
  const closePane = useCatalogueStore((s) => s.closePane);
  const providers = useCatalogueStore((s) => s.providers);
  const providerOptions = useCatalogueStore((s) => s.providerOptions);
  const providerOptionsError = useCatalogueStore((s) => s.providerOptionsError);
  const toggleProvider = useCatalogueStore((s) => s.toggleProvider);
  const loadProviders = useCatalogueStore((s) => s.loadProviders);
  const runSearch = useCatalogueStore((s) => s.search);
  const literatureLoading = useCatalogueStore((s) => s.literatureLoading);
  const literature = useCatalogueStore((s) => s.literature);
  const literatureError = useCatalogueStore((s) => s.literatureError);
  const acquireError = useCatalogueStore((s) => s.acquireError);
  const refinements = useCatalogueStore((s) => s.refinements);
  const listings = useCatalogueStore((s) => s.listings);
  const listed = useMemo(
    () => refinedProviderIds({ refinements, providerOptions, providers }),
    [refinements, providerOptions, providers],
  );
  const setBrowseOpen = useCatalogueStore((s) => s.setBrowseOpen);
  const compose = useCatalogueStore((s) => s.compose);
  const generationReady = useGenerationStore((s) => s.ready);
  const { readOnly, needsDirectory } = useCatalogueAdd();
  const query = useCatalogueStore((s) => s.query);
  const composedQuery = useCatalogueStore((s) => s.composedQuery);
  // The box holds what was written. Where that was a request filters were
  // composed from, the sources are asked its subject, not the sentence.
  const [draft, setDraft] = useState(() => {
    const held = useCatalogueStore.getState();
    return held.composedQuery !== null && held.composedQuery.keyword === held.query
      ? held.composedQuery.text
      : held.query;
  });
  const [composeMode, setComposeMode] = useState(false);

  // The filter is offered before anything has been asked, so the list it is
  // built from is read when the pane opens rather than from a search.
  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  const enabled = providerOptions.filter((option) => option.enabled);
  const listedAsked = listed.some((id) => {
    const listing = listings[id];
    return listing !== undefined && (listing.loading || listing.error !== null || listing.answer !== null);
  });
  const asked =
    (selectsAny(providers) &&
      (literatureLoading || literatureError !== null || literature !== null)) ||
    listedAsked;
  const browsable = browseProviderIds(providerOptions);
  const keyword =
    composedQuery !== null && composedQuery.text === draft && composedQuery.keyword === query
      ? composedQuery.keyword
      : draft;
  // Composing is offered only where it can do something — the generation
  // model is ready and a provider declares filters — and is in force only
  // while it is offered.
  const composable = generationReady && browsable.length > 0;
  const composeInForce = composeMode && composable;

  return (
    <div className="flex h-full flex-col border-l border-[var(--border-main)] bg-[var(--bg-sidebar)]">
      <div className="flex items-center justify-between border-b border-[var(--border-main)] px-3 py-1.5">
        <h2 className="text-[11px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
          Catalogues & literature
        </h2>
        <button
          type="button"
          onClick={closePane}
          aria-label="Close catalogue"
          className="flex h-[22px] w-[22px] items-center justify-center rounded text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)]"
        >
          <X size={13} />
        </button>
      </div>

      <form
        className="px-3 py-1.5"
        onSubmit={(e) => {
          e.preventDefault();
          if (composeInForce && draft.trim() !== "") void compose(draft);
          else void runSearch(draft);
        }}
      >
        <div className="flex items-stretch gap-1.5">
          <div className="flex min-w-0 flex-1 items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] pl-2 pr-1">
            <Search size={12} className="shrink-0 text-[var(--text-dim)]" />
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder="What do you want to learn or read?"
              aria-label="Search catalogues and literature"
              className="w-full bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
            />
            {composable && (
              <ComposeToggle active={composeMode} onToggle={() => setComposeMode(!composeMode)} />
            )}
          </div>
          <FiltersButton />
        </div>
      </form>
      <RefineBar keyword={keyword} options={providerOptions} />
      <ComposeStatus />

      {(readOnly || needsDirectory) && (
        <p className="border-b border-[var(--border-main)] px-3 py-1.5 text-[10px] leading-snug text-[var(--text-dim)]">
          {readOnly
            ? "This workspace is read-only. You can search and open what the sources hold, but nothing can be added to it."
            : "Choose a directory before adding documents."}
        </p>
      )}

      <div className="flex-1 overflow-y-auto border-t border-[var(--border-main)] px-3 pb-2">
        {acquireError !== null && (
          <p className="py-1.5 text-[10px] leading-snug text-red-400">{acquireError}</p>
        )}

        {providerOptionsError !== null ? (
          <p className="py-1 text-[10px] leading-snug text-red-400">
            The providers could not be listed: {providerOptionsError}
          </p>
        ) : (
          <FilterChips
            selection={providers}
            all={enabledProviderIds(providerOptions)}
            label={(id) => enabled.find((option) => option.provider === id)?.name ?? id}
            onToggle={toggleProvider}
          />
        )}
        {!selectsAny(providers) ? (
          <Note>No source selected, so nothing was searched.</Note>
        ) : (
          <>
            {literatureLoading && <Spinner />}
            {/* The error and the answer are shown together when both exist.
                A failed request for a provider the user has just re-selected
                says nothing about what the others already answered, and
                hiding their results would take away what is being read. */}
            {literatureError !== null && !literatureLoading && (
              <p className="py-2 text-[10px] leading-snug text-red-400">{literatureError}</p>
            )}
            <ProviderResults
              answer={literatureLoading ? null : literature}
              listed={listed}
              providerOptions={providerOptions}
              selection={providers}
            />
          </>
        )}

        {!asked && (
          <>
            <Note>
              Describe what you are trying to understand. Every source that is on
              is asked: the open catalogues of textbooks, courses and
              documentation you have fetched, and the indexes you have enabled,
              both in Settings › Integrations.
            </Note>
            {browsable.length > 0 && (
              <Note>
                No keyword needed to explore:{" "}
                <button
                  type="button"
                  onClick={() => setBrowseOpen(true)}
                  className="font-bold text-[var(--accent-blue)] hover:underline"
                >
                  open Filters
                </button>{" "}
                to list{" "}
                {browsable
                  .map((id) => providerOptions.find((o) => o.provider === id)?.name ?? id)
                  .join(", ")}{" "}
                works by year, type, citations and more.
              </Note>
            )}
            {browsable.length > 0 && generationReady && (
              <Note>
                Or say it in a sentence — “open-access reviews on CRISPR since 2020,
                most cited first” — and press the lightning button to have the
                filters set for you.
              </Note>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function Note({ children }: { children: ReactNode }) {
  return (
    <p className="py-1.5 text-[10px] leading-snug text-[var(--text-muted)]">{children}</p>
  );
}

/**
 * Which sources are asked, above what they answered.
 *
 * Nothing stored means every member is on, so a chip reads as pressed when the
 * selection admits it rather than when the selection happens to name it —
 * otherwise the default state would show every chip lit and every chip
 * unpressed to a screen reader.
 */
function FilterChips<T extends string>({
  selection,
  all,
  label,
  onToggle,
}: {
  selection: Selection<T>;
  all: T[];
  label: (item: T) => string;
  onToggle: (item: T) => void;
}) {
  if (all.length === 0) return null;
  return (
    // Named for what it decides. These chips choose which sources are asked;
    // the Filters button beside the search box narrows what one of them
    // returns, and both being called "filters" sent people to the wrong one.
    <div role="group" aria-label="Search in" className="flex flex-wrap items-center gap-1 pb-1 pt-2">
      <span className="pr-0.5 text-[9px] uppercase tracking-wider text-[var(--text-muted)]">
        Search in
      </span>
      {all.map((item) => {
        const on = selects(selection, item);
        return (
          <button
            key={item}
            type="button"
            onClick={() => onToggle(item)}
            aria-pressed={on}
            className={`rounded border px-1.5 py-0.5 text-[9px] font-bold uppercase tracking-wider transition-colors ${
              on
                ? "border-[var(--border-strong)] bg-[var(--bg-active)] text-[var(--text-main)]"
                : "border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-dim)]"
            }`}
          >
            {label(item)}
          </button>
        );
      })}
    </div>
  );
}

function Spinner() {
  return (
    <div className="flex items-center justify-center py-4">
      <div className="h-5 w-5 animate-spin rounded-full border-2 border-[var(--accent-blue)] border-t-transparent" />
    </div>
  );
}

/**
 * Chooses whether what is typed is searched for or composed into filters.
 *
 * A choice held in the box and not a guess about what was typed: with it off
 * Enter searches for the words, as it always has, and a sentence that happens
 * to read like a description is still words someone may have meant to search
 * for. While a composition works it is the control that stops it.
 */
function ComposeToggle({ active, onToggle }: { active: boolean; onToggle: () => void }) {
  const composing = useCatalogueStore((s) => s.composing);
  const cancel = useCatalogueStore((s) => s.cancelCompose);
  const label = composing ? "Stop composing filters" : "Compose filters from what you type";
  return (
    <button
      type="button"
      onClick={composing ? cancel : onToggle}
      aria-label={label}
      aria-pressed={composing ? undefined : active}
      title={
        composing
          ? label
          : "Describe what you want in a sentence, and the filters are set for you"
      }
      className={`inline-flex shrink-0 rounded p-1 transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)] ${
        active ? "bg-[var(--accent-blue-muted)] text-[var(--accent-blue)]" : "text-[var(--text-dim)]"
      }`}
    >
      {composing ? <Square size={11} /> : <Zap size={11} />}
    </button>
  );
}

/**
 * What a composition is doing, or what became of it.
 *
 * The filters it set are drawn where filters are drawn, as chips a person can
 * remove. This says the two things chips cannot: what they were composed
 * from, and what was asked for and could not be set — a name the provider did
 * not know is a filter silently missing unless something says so.
 */
function ComposeStatus() {
  const composing = useCatalogueStore((s) => s.composing);
  const composed = useCatalogueStore((s) => s.composed);
  const error = useCatalogueStore((s) => s.composeError);
  const dismiss = useCatalogueStore((s) => s.dismissComposition);
  if (composing) {
    return (
      <p role="status" className="px-3 pb-1.5 text-[10px] leading-snug text-[var(--text-muted)]">
        Composing filters…
      </p>
    );
  }
  if (composed === null && error === null) return null;
  return (
    <div
      role="status"
      aria-label="Composed filters"
      className="flex items-start justify-between gap-2 px-3 pb-1.5 text-[10px] leading-snug"
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        {error !== null && <p className="text-red-400">The filters could not be composed: {error}</p>}
        {composed !== null && (
          <>
            <p className="text-[var(--text-muted)]">Filters composed from “{composed.text}”.</p>
            {composed.notes.map((note) => (
              <p key={note} className="text-[var(--text-dim)]">
                {note}
              </p>
            ))}
            {composed.failures.map((failure) => (
              <p key={failure.name} className="text-red-400">
                {failure.name} could not be composed for: {failure.error}
              </p>
            ))}
          </>
        )}
      </div>
      <button
        type="button"
        onClick={dismiss}
        aria-label="Dismiss"
        className="shrink-0 text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)]"
      >
        <X size={11} />
      </button>
    </div>
  );
}

/**
 * The control that opens the filters, beside the search box rather than under
 * a section: what a provider can be filtered by is a property of the whole
 * search, and a control hidden at the foot of the results was one nobody found.
 * It shows how many filters are set, so a closed panel still says the results
 * below are narrowed.
 */
function FiltersButton() {
  const open = useCatalogueStore((s) => s.browseOpen);
  const setOpen = useCatalogueStore((s) => s.setBrowseOpen);
  const count = useCatalogueStore(totalRefinements);
  const options = useCatalogueStore((s) => s.providerOptions);
  if (browseProviderIds(options).length === 0) return null;
  return (
    <button
      type="button"
      onClick={() => setOpen(!open)}
      aria-expanded={open}
      aria-label={count > 0 ? `Filters, ${count} set` : "Filters"}
      title="Filter and sort a provider's works, with or without a keyword"
      className={`flex shrink-0 items-center gap-1 rounded border px-2 text-[10px] font-bold uppercase tracking-wider transition-colors ${
        open || count > 0
          ? "border-[var(--border-strong)] bg-[var(--bg-active)] text-[var(--text-main)]"
          : "border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-dim)] hover:text-[var(--text-main)]"
      }`}
    >
      <Sliders size={11} />
      Filters
      {count > 0 && (
        <span className="rounded-full bg-[var(--accent-blue)] px-1 text-[9px] leading-[14px] text-white">
          {count}
        </span>
      )}
    </button>
  );
}

/**
 * Filtering and sorting, drawn entirely from what each provider's manifest
 * declares: the sort, the filters and what kind each is all come from the
 * provider, so a provider added tomorrow gets controls with no change here.
 *
 * Every selected provider that declares something to refine gets its own set,
 * and several can be filtered at once; a provider that is off, or declares
 * nothing, has no set. Filters belong to the provider that declared them, not
 * to the pane, so nothing here chooses "the" provider to filter.
 *
 * Changes apply as they are made. A filter is a narrowing of what is already
 * on screen, and a separate "apply" button between the choice and its effect
 * made the pane look inert. Ranges apply when the field is left, since a
 * request per keystroke would ask a rate-limited service about "2", "20" and
 * "202". The keyword in the search box rides along wherever a listing takes one.
 */
function RefineBar({
  keyword,
  options,
}: {
  keyword: string;
  options: LiteratureProviderInfo[];
}) {
  const open = useCatalogueStore((s) => s.browseOpen);
  const refinements = useCatalogueStore((s) => s.refinements);
  const selected = useCatalogueStore((s) => s.providers);
  const clearFilter = useCatalogueStore((s) => s.clearBrowseFilter);
  const setSort = useCatalogueStore((s) => s.setBrowseSort);
  const reset = useCatalogueStore((s) => s.resetRefinement);
  const refine = useCatalogueStore((s) => s.refine);

  const offered = browseProviderIds(options);
  if (offered.length === 0) return null;
  const shown = offered.filter((id) => selects(selected, id));
  const listed = refinedProviderIds({ refinements, providerOptions: options, providers: selected });
  const typed = keyword.trim() !== "";

  return (
    <div className="px-3 pb-1.5">
      {listed.length > 0 && !open && (
        <ActiveFilters
          options={options}
          refinements={refinements}
          listed={listed}
          onClearFilter={(provider, name) => {
            clearFilter(provider, name);
            void refine(keyword, provider);
          }}
          onClearSort={(provider) => {
            setSort(provider, null);
            void refine(keyword, provider);
          }}
          onReset={() => {
            reset();
            void refine(keyword);
          }}
        />
      )}
      {open && (
        <div
          role="group"
          aria-label="Filters"
          className="flex max-h-[45vh] flex-col gap-2 overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-app)] p-2"
        >
          {shown.length === 0 && (
            <p className="text-[10px] leading-snug text-[var(--text-muted)]">
              None of the sources selected above can be filtered. Select one that can to
              set filters.
            </p>
          )}
          {shown.map((id) => (
            <ProviderFilters
              key={id}
              info={options.find((option) => option.provider === id)}
              typed={typed}
              keyword={keyword}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** A provider that declares more filters than this shows only the ones in use,
 *  and offers the rest from a menu: forty controls is not a panel. */
const FILTERS_SHOWN_AT_ONCE = 8;

/** What is drawn on one line of a provider's filters: one filter, or the
 *  filters that share a group. */
interface FilterRow {
  key: string;
  label: string;
  members: FilterControl[];
}

/** The declared filters as rows, in declaration order, a group standing where
 *  its first member does. A filter whose value only ever comes from a result
 *  row has no control of its own and is a row only once it is set. */
function filterRows(controls: BrowseControls, filters: Record<string, string[]>): FilterRow[] {
  const rows: FilterRow[] = [];
  for (const filter of controls.filters) {
    if (filter.input === "result" && !(filter.name in filters)) continue;
    const row =
      filter.group === null ? undefined : rows.find((candidate) => candidate.key === `group:${filter.group}`);
    if (row !== undefined) row.members.push(filter);
    else {
      rows.push({
        key: filter.group === null ? filter.name : `group:${filter.group}`,
        label: filter.group ?? filter.label,
        members: [filter],
      });
    }
  }
  return rows;
}

/** One provider's filters and sort, under its name. */
function ProviderFilters({
  info,
  typed,
  keyword,
}: {
  info: LiteratureProviderInfo | undefined;
  typed: boolean;
  keyword: string;
}) {
  const refinement = useCatalogueStore((s) => (info ? s.refinements[info.provider] : undefined));
  const setSort = useCatalogueStore((s) => s.setBrowseSort);
  const reset = useCatalogueStore((s) => s.resetRefinement);
  const refine = useCatalogueStore((s) => s.refine);
  // Rows the user asked to see and has not yet set. A display choice, like
  // the panel being open: it is not part of what is asked.
  const [added, setAdded] = useState<string[]>([]);
  const controls = info?.capabilities.browse ?? null;
  if (!info || controls === null) return null;
  const provider = info.provider;
  const filters = refinement?.filters ?? {};
  const sort = refinement?.sort ?? null;
  const count = refinementCount(refinement);
  const apply = () => void refine(keyword, provider);

  const rows = filterRows(controls, filters);
  const inUse = (row: FilterRow) => row.members.some((member) => member.name in filters);
  const compact = controls.filters.length > FILTERS_SHOWN_AT_ONCE;
  const shown = compact ? rows.filter((row) => inUse(row) || added.includes(row.key)) : rows;
  const offered = compact ? rows.filter((row) => !shown.includes(row)) : [];

  return (
    <div role="group" aria-label={`${info.name} filters`} className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
          {info.name}
        </span>
        {count > 0 && (
          <button
            type="button"
            onClick={() => {
              reset(provider);
              setAdded([]);
              apply();
            }}
            aria-label={`Reset ${info.name} filters`}
            className="text-[10px] text-[var(--text-dim)] underline-offset-2 hover:text-[var(--text-main)] hover:underline"
          >
            Reset
          </button>
        )}
      </div>
      <div className="grid grid-cols-2 gap-1.5">
        {controls.sort !== null && (
          <ControlRow label="Sort by" wide={!compact && rows.length === 0}>
            <select
              aria-label="Sort"
              value={sort ?? ""}
              onChange={(e) => {
                setSort(provider, e.target.value === "" ? null : e.target.value);
                apply();
              }}
              className={CONTROL_CLASS}
            >
              <option value="">{defaultSortLabel(controls, typed)}</option>
              {controls.sort.options
                .filter((option) => option.value !== controls.sort?.default)
                .map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
            </select>
          </ControlRow>
        )}
        {offered.length > 0 && (
          <ControlRow label="Filter by">
            <select
              aria-label={`Add a ${info.name} filter`}
              value=""
              onChange={(e) => {
                if (e.target.value !== "") setAdded((current) => [...current, e.target.value]);
              }}
              className={CONTROL_CLASS}
            >
              <option value="">Add a filter…</option>
              {offered.map((row) => (
                <option key={row.key} value={row.key}>
                  {row.label}
                </option>
              ))}
            </select>
          </ControlRow>
        )}
        {shown.map((row) => (
          <FilterRowView
            key={row.key}
            row={row}
            provider={provider}
            modes={controls.modes}
            refinement={refinement ?? NO_REFINEMENT}
            onApply={apply}
          />
        ))}
      </div>
      {typed && !controls.keyword && (
        <p className="text-[10px] leading-snug text-[var(--text-muted)]">
          {info.name}'s listing does not take a keyword, so “{keyword.trim()}” only
          searches the other sources.
        </p>
      )}
      {count === 0 && (
        <p className="text-[10px] leading-snug text-[var(--text-muted)]">
          Set a filter or a sort to list {info.name} works
          {controls.keyword ? ", with or without a keyword" : ""}.
        </p>
      )}
    </div>
  );
}

/** What the unset sort means right now: with a keyword typed, the provider
 *  ranks by relevance; without one, by its own default. */
function defaultSortLabel(controls: BrowseControls, keywordTyped: boolean): string {
  const sort = controls.sort;
  if (sort === null) return "";
  if (keywordTyped && controls.keyword && sort.keyword_default) {
    return `${optionLabel(sort.options, sort.keyword_default, "Relevance")} (default)`;
  }
  return `${optionLabel(sort.options, sort.default, sort.default)} (default)`;
}

/** What a filter is called on its own: a grouped one by its group as well,
 *  since "From" says nothing without "Year". */
function filterName(filter: FilterControl): string {
  return filter.group === null || filter.input === "toggle"
    ? filter.label
    : `${filter.group} ${filter.label.toLowerCase()}`;
}

/** One row: a lone on/off filter is just its checkbox; anything else is a
 *  heading over its members, each drawn by the control its value calls for. */
function FilterRowView({
  row,
  provider,
  modes,
  refinement,
  onApply,
}: {
  row: FilterRow;
  provider: string;
  modes: ControlOption[];
  refinement: Refinement;
  onApply: () => void;
}) {
  const setFilter = useCatalogueStore((s) => s.setBrowseFilter);
  const setMode = useCatalogueStore((s) => s.setBrowseMode);
  const member = (filter: FilterControl) => {
    const Control = CONTROLS[filter.input];
    const values = refinement.filters[filter.name] ?? [];
    return (
      <div key={filter.name} className="flex min-w-0 flex-1 flex-col gap-1">
        {filter.modes.length > 1 && values.length > 0 && (
          <select
            aria-label={`${filterName(filter)} matches`}
            value={refinement.modes[filter.name] ?? filter.modes[0]}
            onChange={(e) => {
              setMode(provider, filter.name, e.target.value);
              onApply();
            }}
            className="self-start rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1 py-0.5 text-[10px] text-[var(--text-dim)] outline-none"
          >
            {filter.modes.map((id) => (
              <option key={id} value={id}>
                {optionLabel(modes, id, id)}
              </option>
            ))}
          </select>
        )}
        <Control
          provider={provider}
          filter={filter}
          name={filterName(filter)}
          caption={row.members.length > 1 ? filter.label : undefined}
          values={values}
          labels={refinement.labels[filter.name] ?? {}}
          onChange={(next, labels) => setFilter(provider, filter.name, next, labels)}
          onApply={onApply}
        />
      </div>
    );
  };
  const only = row.members.length === 1 ? row.members[0] : null;
  if (only !== null && only.input === "toggle") {
    return <div className="col-span-2 flex">{member(only)}</div>;
  }
  // A single plain control fits half the panel; a group, a checklist or a
  // typeahead needs its width.
  const narrow =
    only !== null &&
    only.modes.length === 0 &&
    only.input !== "suggest" &&
    only.input !== "result";
  return (
    <ControlRow label={row.label} wide={!narrow} group>
      <span className={`flex gap-1.5 ${row.members.every((m) => m.input === "toggle") ? "flex-col" : "items-start"}`}>
        {row.members.map(member)}
      </span>
    </ControlRow>
  );
}

/** What every filter control is given. The value is always a list, whatever
 *  sets it, so a control differs from another only in how it is drawn. */
interface FilterControlProps {
  provider: string;
  filter: FilterControl;
  /** What the control is called to a screen reader: the filter's full name. */
  name: string;
  /** What to say inside the control when it shares a row with others. */
  caption?: string;
  values: string[];
  /** What the chosen values are called, where they cannot be read. */
  labels: Record<string, string>;
  onChange: (values: string[], labels?: Record<string, string>) => void;
  onApply: () => void;
}

/** The control each value source is set by. A new value type is one row here
 *  and one arm of the engine's `ValueRule::admit`; nothing else tells filters
 *  apart. */
const CONTROLS: Record<FilterInput, (props: FilterControlProps) => ReactNode> = {
  toggle: ToggleControl,
  options: OptionsControl,
  integer: TypedControl,
  number: TypedControl,
  date: TypedControl,
  text: TypedControl,
  suggest: SuggestControl,
  result: ChosenValues,
};

function ToggleControl({ filter, values, onChange, onApply }: FilterControlProps) {
  return (
    <label className="flex items-center gap-1.5 text-[11px] text-[var(--text-main)]">
      <input
        type="checkbox"
        checked={values[0] === "true"}
        onChange={(e) => {
          onChange(e.target.checked ? ["true"] : []);
          onApply();
        }}
      />
      {filter.label}
    </label>
  );
}

/** One of a closed list, or several of it where the filter combines values. */
function OptionsControl({ filter, name, caption, values, onChange, onApply }: FilterControlProps) {
  if (filter.modes.length === 0) {
    return (
      <select
        aria-label={name}
        value={values[0] ?? ""}
        onChange={(e) => {
          onChange(e.target.value === "" ? [] : [e.target.value]);
          onApply();
        }}
        className={CONTROL_CLASS}
      >
        <option value="">{caption ?? "Any"}</option>
        {filter.options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    );
  }
  return (
    <div
      role="group"
      aria-label={name}
      className="flex max-h-28 flex-col gap-0.5 overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1"
    >
      {filter.options.map((option) => (
        <label key={option.value} className="flex items-center gap-1.5 text-[11px] text-[var(--text-main)]">
          <input
            type="checkbox"
            checked={values.includes(option.value)}
            onChange={(e) => {
              onChange(
                e.target.checked
                  ? [...values, option.value]
                  : values.filter((value) => value !== option.value),
              );
              onApply();
            }}
          />
          {option.label}
        </label>
      ))}
    </div>
  );
}

const TYPED_INPUTS: Partial<Record<FilterInput, { type: string; step?: string; hint: string }>> = {
  integer: { type: "number", step: "1", hint: "Number" },
  number: { type: "number", step: "any", hint: "Number" },
  date: { type: "date", hint: "Date" },
  text: { type: "text", hint: "Words" },
};

/** A typed value. Applies when the field is left, and only if it changed while
 *  it held focus; Enter leaves it, so the two ways of finishing are one
 *  request, and a value typed a character at a time is asked about once. */
function TypedControl({ filter, name, caption, values, onChange, onApply }: FilterControlProps) {
  const value = values[0] ?? "";
  const atFocus = useRef(value);
  const shape = TYPED_INPUTS[filter.input];
  return (
    <input
      type={shape?.type ?? "text"}
      step={shape?.step}
      aria-label={name}
      placeholder={caption ?? shape?.hint}
      value={value}
      onFocus={() => {
        atFocus.current = value;
      }}
      onChange={(e) => onChange(e.target.value === "" ? [] : [e.target.value])}
      onBlur={() => {
        if (value !== atFocus.current) onApply();
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          e.currentTarget.blur();
        }
      }}
      className={CONTROL_CLASS}
    />
  );
}

/** The values chosen so far, each removable, called what they were called
 *  when they were chosen. */
function ChosenValues({ name, values, labels, onChange, onApply }: FilterControlProps) {
  if (values.length === 0) return null;
  return (
    <span className="flex flex-wrap gap-1">
      {values.map((value) => (
        <button
          key={value}
          type="button"
          onClick={() => {
            onChange(values.filter((kept) => kept !== value));
            onApply();
          }}
          aria-label={`Remove ${labels[value] ?? value} from ${name}`}
          title={labels[value] ?? value}
          className="flex max-w-full items-center gap-1 rounded-full border border-[var(--border-strong)] bg-[var(--bg-active)] px-2 py-0.5 text-[10px] text-[var(--text-main)] transition-colors hover:border-red-400"
        >
          <span className="truncate">{labels[value] ?? value}</span>
          <X size={9} className="shrink-0" />
        </button>
      ))}
    </span>
  );
}

/** How long typing must pause before the provider is asked. These are
 *  rate-limited services, and "hin", "hint" and "hinto" are one question. */
const SUGGEST_PAUSE_MS = 250;

/** A value chosen from what the provider suggests for what was typed. */
function SuggestControl(props: FilterControlProps) {
  const { provider, filter, name, caption, values, onChange, onApply } = props;
  const [text, setText] = useState("");
  const [suggestions, setSuggestions] = useState<FilterSuggestion[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const asked = useRef(0);
  const several = filter.modes.length > 0;

  useEffect(() => {
    const typed = text.trim();
    const request = ++asked.current;
    setError(null);
    if (typed.length < 2) {
      setSuggestions(null);
      return;
    }
    const timer = setTimeout(() => {
      api
        .literatureFilterSuggest(provider, filter.name, typed)
        .then((found) => {
          if (request === asked.current) setSuggestions(found);
        })
        .catch((reason) => {
          if (request !== asked.current) return;
          console.error(`filter suggestions for ${provider}/${filter.name} failed:`, reason);
          setSuggestions(null);
          setError(reason?.toString?.() ?? "Suggestions could not be read");
        });
    }, SUGGEST_PAUSE_MS);
    return () => clearTimeout(timer);
  }, [text, provider, filter.name]);

  return (
    <div className="flex flex-col gap-1">
      <ChosenValues {...props} />
      <input
        type="text"
        aria-label={name}
        placeholder={caption ?? "Type to search"}
        value={text}
        onChange={(e) => setText(e.target.value)}
        className={CONTROL_CLASS}
      />
      {error !== null && <p className="text-[10px] leading-snug text-red-400">{error}</p>}
      {suggestions !== null && suggestions.length === 0 && (
        <p className="text-[10px] leading-snug text-[var(--text-muted)]">Nothing matches.</p>
      )}
      {suggestions !== null && suggestions.length > 0 && (
        <ul
          aria-label={`${name} suggestions`}
          className="flex max-h-36 flex-col overflow-y-auto rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)]"
        >
          {suggestions
            .filter((suggestion) => !values.includes(suggestion.value))
            .map((suggestion) => (
              <li key={suggestion.value}>
                <button
                  type="button"
                  onClick={() => {
                    onChange(several ? [...values, suggestion.value] : [suggestion.value], {
                      [suggestion.value]: suggestion.label,
                    });
                    setText("");
                    setSuggestions(null);
                    onApply();
                  }}
                  className="flex w-full flex-col px-1.5 py-1 text-left transition-colors hover:bg-[var(--bg-active)]"
                >
                  <span className="text-[11px] text-[var(--text-main)]">{suggestion.label}</span>
                  {suggestion.hint !== null && (
                    <span className="truncate text-[10px] text-[var(--text-muted)]">
                      {suggestion.hint}
                    </span>
                  )}
                </button>
              </li>
            ))}
        </ul>
      )}
    </div>
  );
}

/** The filters that are set, one removable chip each, for when the panel is
 *  closed and the results would otherwise look unexplained. With more than one
 *  provider filtered, each chip is prefixed with the provider it belongs to. */
function ActiveFilters({
  options,
  refinements,
  listed,
  onClearFilter,
  onClearSort,
  onReset,
}: {
  options: LiteratureProviderInfo[];
  refinements: Record<string, Refinement>;
  listed: string[];
  onClearFilter: (provider: string, name: string) => void;
  onClearSort: (provider: string) => void;
  onReset: () => void;
}) {
  const chips: { key: string; text: string; clear: () => void }[] = [];
  for (const provider of listed) {
    const info = options.find((option) => option.provider === provider);
    const controls = info?.capabilities.browse ?? null;
    const refinement = refinements[provider];
    if (!info || controls === null || refinement === undefined) continue;
    const prefix = listed.length > 1 ? `${info.name} · ` : "";
    for (const filter of controls.filters) {
      const values = refinement.filters[filter.name];
      if (values === undefined) continue;
      chips.push({
        key: `${provider}:${filter.name}`,
        text: prefix + filterSummary(filter, values, refinement, controls.modes),
        clear: () => onClearFilter(provider, filter.name),
      });
    }
    if (refinement.sort !== null && controls.sort !== null) {
      chips.push({
        key: `${provider}:sort`,
        text: `${prefix}Sort: ${optionLabel(controls.sort.options, refinement.sort, refinement.sort)}`,
        clear: () => onClearSort(provider),
      });
    }
  }
  return (
    <div className="flex flex-wrap items-center gap-1" aria-label="Active filters">
      {chips.map((chip) => (
        <button
          key={chip.key}
          type="button"
          onClick={chip.clear}
          aria-label={`Remove ${chip.text}`}
          title={chip.text}
          className="flex max-w-full items-center gap-1 rounded-full border border-[var(--border-strong)] bg-[var(--bg-active)] px-2 py-0.5 text-[10px] text-[var(--text-main)] transition-colors hover:border-red-400"
        >
          <span className="truncate">{chip.text}</span>
          <X size={9} className="shrink-0" />
        </button>
      ))}
      {chips.length > 1 && (
        <button
          type="button"
          onClick={onReset}
          className="px-1 text-[10px] text-[var(--text-dim)] underline-offset-2 hover:text-[var(--text-main)] hover:underline"
        >
          Clear all
        </button>
      )}
    </div>
  );
}

/** How one set filter reads in a chip: its name, how its values are combined
 *  where that is not the usual way, and the values as a person would say
 *  them. */
function filterSummary(
  filter: FilterControl,
  values: string[],
  refinement: Refinement,
  modes: ControlOption[],
): string {
  if (filter.input === "toggle") return filter.label;
  const labels = refinement.labels[filter.name] ?? {};
  const said = values
    .map((value) => optionLabel(filter.options, value, labels[value] ?? value))
    .join(", ");
  const mode = refinement.modes[filter.name];
  const how =
    mode !== undefined && mode !== filter.modes[0]
      ? ` (${optionLabel(modes, mode, mode).toLowerCase()})`
      : "";
  return `${filterName(filter)}${how}: ${said}`;
}

const CONTROL_CLASS =
  "w-full rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--border-strong)]";

/** A labelled control. A row holding several controls is a group and not a
 *  `<label>`, which would name only the first of them. */
function ControlRow({
  label,
  wide = false,
  group = false,
  children,
}: {
  label: string;
  wide?: boolean;
  group?: boolean;
  children: ReactNode;
}) {
  const heading = (
    <span className="text-[9px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
      {label}
    </span>
  );
  const className = `flex min-w-0 flex-col gap-0.5 ${wide ? "col-span-2" : ""}`;
  return group ? (
    <div className={className}>
      {heading}
      {children}
    </div>
  ) : (
    <label className={className}>
      {heading}
      {children}
    </label>
  );
}

function optionLabel(
  options: readonly { value: string; label: string }[],
  value: string,
  fallback: string,
): string {
  return options.find((option) => option.value === value)?.label ?? fallback;
}

/**
 * A provider's filtered works, under its name like any other provider's
 * answer, with the next page one press away.
 *
 * While the first page is on its way or has failed there is no answer yet; the
 * heading and the reason are shown either way, so a refinement that is asking
 * does not look like one that was ignored.
 */
function ListingBody({ provider, name }: { provider: string; name: string }) {
  const listing = useCatalogueStore((s) => s.listings[provider]);
  const more = useCatalogueStore((s) => s.browseMore);
  const answer = listing?.answer ?? null;
  const loading = listing?.loading ?? false;
  const error = listing?.error ?? null;
  return (
    <div className="flex flex-col" aria-label={`${name} listing`}>
      <h4 className="mt-2 flex items-baseline gap-1.5 border-b border-[var(--border-strong)] pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
        {name}
        <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
          filtered
          {answer !== null &&
            ` · ${answer.results.length.toLocaleString()}${
              // A page of ten out of twelve thousand is a different thing from
              // ten in all, so the total is said where the provider gives one.
              answer.total !== null && answer.total > answer.results.length
                ? ` of ${answer.total.toLocaleString()}`
                : ""
            }`}
        </span>
      </h4>
      {answer === null && loading && <Spinner />}
      {error !== null && !loading && (
        <p className="py-1.5 text-[10px] leading-snug text-red-400">
          {name} could not list works: {error}
        </p>
      )}
      {answer !== null &&
        (answer.results.length === 0 ? (
          <Note>Nothing in {name} matches these filters.</Note>
        ) : (
          <div className="divide-y divide-[var(--border-main)]">
            {answer.results.map((work) => (
              <CatalogueCandidate key={work.id} provider={provider} work={work} />
            ))}
          </div>
        ))}
      {answer !== null && !answer.exhausted && (
        <button
          type="button"
          onClick={() => void more(provider)}
          disabled={loading}
          aria-label={`Load more filtered ${name}`}
          className="mt-1.5 self-start rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)] disabled:opacity-50"
        >
          {loading ? "Loading" : "Load more"}
        </button>
      )}
    </div>
  );
}

/** An entry with neither results nor an error breaks the response contract.
 *  It is reported as a failure rather than read as "nothing found". */
function providerAnswerMalformed(provider: string): string {
  console.error(`literature search: provider ${provider} answered with neither results nor an error`);
  return "the answer carried neither results nor an error";
}

/**
 * Every selected provider's part of the answer, in the registry's order and each under its own name: a filtered listing for a provider with
 * filters set, otherwise its answer to the keyword.
 *
 * The held answers can cover more providers than the selection admits —
 * narrowing the selection hides a provider rather than re-asking the rest — so
 * what is listed is the intersection, not everything that replied.
 */
function ProviderResults({
  answer,
  listed,
  providerOptions,
  selection,
}: {
  answer: LiteratureAnswer | null;
  listed: string[];
  providerOptions: LiteratureProviderInfo[];
  selection: Selection<string>;
}) {
  // No source being on is a setting, not a search result: saying "nothing
  // found" would send someone rephrasing a query no index ever saw.
  if (answer !== null && answer.providers.length === 0 && listed.length === 0) {
    return (
      <Note>
        No source is on, so nothing was searched. Settings › Integrations fetches
        the open catalogues, and turns on the indexes that ship with Wilkes or
        one you describe.
      </Note>
    );
  }
  const known = enabledProviderIds(providerOptions).filter((id) => selects(selection, id));
  // An answer from a provider the list does not know is kept, at the end,
  // rather than dropped: the answer is what the backend said, and the list may
  // be older than it.
  const order = [
    ...known,
    ...(answer?.providers ?? [])
      .map((entry) => entry.provider)
      .filter((id) => !known.includes(id) && selects(selection, id)),
  ];
  return (
    <div className="flex flex-col">
      {order.map((id) => {
        const info = providerOptions.find((option) => option.provider === id);
        if (listed.includes(id)) {
          return <ListingBody key={id} provider={id} name={info?.name ?? id} />;
        }
        const entry = answer?.providers.find((candidate) => candidate.provider === id);
        if (answer === null || entry === undefined) return null;
        return (
          <ProviderEntry
            key={id}
            answer={answer}
            entry={entry}
            paged={info?.capabilities.paged_search === true}
            grain={info?.mirror?.grain ?? null}
          />
        );
      })}
    </div>
  );
}

/** One provider's answer to the keyword, with its next page one press away
 *  where the provider can be read on. */
function ProviderEntry({
  answer,
  entry,
  paged,
  grain,
}: {
  answer: LiteratureAnswer;
  entry: LiteratureProviderAnswer;
  paged: boolean;
  /** What a learning catalogue holds; null for any other provider. */
  grain: CatalogueGrain | null;
}) {
  const moreLoading = useCatalogueStore((s) => s.literatureMore);
  const moreError = useCatalogueStore((s) => s.literatureMoreError);
  const loadMore = useCatalogueStore((s) => s.loadMoreLiterature);
  return (
    <div className="flex flex-col">
      {/* The provider is the strongest label in the pane: it says whose
          index a row came from, and for most rows it is the only thing that
          does. So it reads bright and sits on a rule of its own, rather than
          as a dim line that scanned as part of the row beneath. A catalogue
          says beside its name what it is a catalogue of. */}
      <h4 className="mt-2 flex items-baseline gap-1.5 border-b border-[var(--border-strong)] pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
        {entry.name}
        {(grain !== null || entry.results !== null) && (
          <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
            {[grain !== null ? GRAIN_LABELS[grain] : null, entry.results?.length ?? null]
              .filter((part) => part !== null)
              .join(" · ")}
          </span>
        )}
      </h4>
      {entry.error !== null || entry.results === null ? (
        <p className="py-1.5 text-[10px] leading-snug text-red-400">
          {entry.name} could not answer: {entry.error ?? providerAnswerMalformed(entry.provider)}
        </p>
      ) : entry.results.length === 0 ? (
        <Note>
          {entry.name} returned nothing for “{answer.query}”.
        </Note>
      ) : (
        <div className="divide-y divide-[var(--border-main)]">
          {entry.results.map((work) => (
            <CatalogueCandidate key={work.id} provider={entry.provider} work={work} />
          ))}
        </div>
      )}
      {moreError[entry.provider] !== undefined && (
        <p className="py-1 text-[10px] leading-snug text-red-400">
          {entry.name} could not read the next page: {moreError[entry.provider]}
        </p>
      )}
      {entry.results !== null &&
        entry.results.length > 0 &&
        paged &&
        answer.pages[entry.provider]?.exhausted === false && (
          <button
            type="button"
            onClick={() => void loadMore(entry.provider)}
            disabled={moreLoading.includes(entry.provider)}
            aria-label={`Load more from ${entry.name}`}
            className="mt-1.5 self-start rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)] disabled:opacity-50"
          >
            {moreLoading.includes(entry.provider) ? "Loading" : "Load more"}
          </button>
        )}
    </div>
  );
}
