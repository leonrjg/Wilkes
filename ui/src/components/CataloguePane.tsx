import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, Search, Sliders, X } from "react-feather";
import type {
  BrowseControls,
  CatalogueGrain,
  CatalogueHit,
  FilterControl,
  LiteratureProviderAnswer,
  LiteratureProviderInfo,
} from "../lib/types";
import {
  ALL_GRAINS,
  browseProviderIds,
  enabledProviderIds,
  type LiteratureAnswer,
  type Refinement,
  type PaneSection,
  refinedProviderIds,
  refinementCount,
  totalRefinements,
  type Selection,
  selects,
  selectsAny,
  useCatalogueStore,
} from "../stores/useCatalogueStore";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import CatalogueCandidate, { PaperCandidate } from "./CatalogueCandidate";

const GRAIN_LABELS: Record<CatalogueGrain, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
};

/**
 * Looking for something to add: the open learning catalogues and the live
 * sources, asked one question at once.
 *
 * Sits with the other ways documents enter a library rather than with the
 * search results: looking for material to acquire is an acquisition, and the
 * pane it opens beside is the file list it will change.
 *
 * The two kinds of source are searched together and listed apart. They are
 * different answers — a textbook that teaches a subject, a work a provider
 * indexes — each with its own empty states, and neither is ranked against the
 * other. Each carries its own filter, under its own heading, because a grain
 * is the mirror's vocabulary and means nothing to a live source, and a
 * provider is a service the user configured and has no grain.
 */
export default function CataloguePane() {
  const closePane = useCatalogueStore((s) => s.closePane);
  const grains = useCatalogueStore((s) => s.grains);
  const providers = useCatalogueStore((s) => s.providers);
  const providerOptions = useCatalogueStore((s) => s.providerOptions);
  const providerOptionsError = useCatalogueStore((s) => s.providerOptionsError);
  const collapsed = useCatalogueStore((s) => s.collapsed);
  const toggleGrain = useCatalogueStore((s) => s.toggleGrain);
  const toggleProvider = useCatalogueStore((s) => s.toggleProvider);
  const toggleSection = useCatalogueStore((s) => s.toggleSection);
  const loadProviders = useCatalogueStore((s) => s.loadProviders);
  const runSearch = useCatalogueStore((s) => s.search);
  const loading = useCatalogueStore((s) => s.loading);
  const answer = useCatalogueStore((s) => s.answer);
  const error = useCatalogueStore((s) => s.error);
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
  const { readOnly, needsDirectory } = useCatalogueAdd();
  const [draft, setDraft] = useState(useCatalogueStore.getState().query);

  // The filter is offered before anything has been asked, so the list it is
  // built from is read when the pane opens rather than from a search.
  useEffect(() => {
    void loadProviders();
  }, [loadProviders]);

  const enabled = providerOptions.filter((option) => option.enabled);
  // What a collapsed section is hiding. A rolled-up heading that said only its
  // name would be a place results could land unseen.
  const catalogueCount = answer === null ? null : answer.hits.length;
  // A provider shown as a filtered listing is counted once, as the listing.
  const listedCount = listed.reduce(
    (total, id) => total + (listings[id]?.answer?.results.length ?? 0),
    0,
  );
  const listedAsked = listed.some((id) => {
    const listing = listings[id];
    return listing !== undefined && (listing.loading || listing.error !== null || listing.answer !== null);
  });
  const literatureCount =
    literature === null && !listedAsked
      ? null
      : (literature?.providers ?? [])
          .filter((entry) => selects(providers, entry.provider) && !listed.includes(entry.provider))
          .reduce((total, entry) => total + (entry.results?.length ?? 0), 0) + listedCount;
  const asked =
    (selectsAny(grains) && (loading || error !== null || answer !== null)) ||
    (selectsAny(providers) &&
      (literatureLoading || literatureError !== null || literature !== null)) ||
    listedAsked;
  const browsable = browseProviderIds(providerOptions);

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
          void runSearch(draft);
        }}
      >
        <div className="flex items-stretch gap-1.5">
          <div className="flex min-w-0 flex-1 items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2">
            <Search size={12} className="shrink-0 text-[var(--text-dim)]" />
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder="What do you want to learn or read?"
              aria-label="Search catalogues and literature"
              className="w-full bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
            />
          </div>
          <FiltersButton />
        </div>
      </form>
      <RefineBar keyword={draft} options={providerOptions} />

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

        <Section
          section="catalogues"
          label="Open catalogues"
          count={catalogueCount}
          collapsed={collapsed.includes("catalogues")}
          onToggle={toggleSection}
        >
          <FilterChips
            selection={grains}
            all={ALL_GRAINS}
            label={(grain) => GRAIN_LABELS[grain]}
            onToggle={toggleGrain}
          />
          {!selectsAny(grains) ? (
            <Note>No category selected, so the catalogues were not searched.</Note>
          ) : (
            <>
              {loading && <Spinner />}
              {error !== null && !loading && (
                <p className="py-2 text-[10px] leading-snug text-red-400">{error}</p>
              )}
              {!loading && error === null && answer !== null && (
                <CatalogueAnswerBody answer={answer} />
              )}
            </>
          )}
        </Section>

        <Section
          section="literature"
          label="Literature"
          count={literatureCount}
          collapsed={collapsed.includes("literature")}
          onToggle={toggleSection}
        >
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
            <Note>No provider selected, so nothing was searched.</Note>
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
        </Section>

        {!asked && (
          <>
            <Note>
              Describe what you are trying to understand. The catalogues are open
              textbooks, courses and documentation sets; the providers are the
              live sources enabled in Settings › Integrations.
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
          </>
        )}
      </div>
    </div>
  );
}

/**
 * One half of the pane: its heading, the control that rolls it up, and what it
 * holds when it is not rolled up.
 *
 * The count is shown rather than only the name, because a collapsed section is
 * still searched — collapsing decides what is *shown*, the filters decide what
 * is *asked* — and a heading that said only "Literature" would be a place
 * results could land unseen. A section's own filter rolls up with it, since a
 * filter with nothing visible under it controls nothing the user can see.
 */
function Section({
  section,
  label,
  count,
  collapsed,
  onToggle,
  children,
}: {
  section: PaneSection;
  label: string;
  /** How many results the section holds, or null before it has been asked. */
  count: number | null;
  collapsed: boolean;
  onToggle: (section: PaneSection) => void;
  children: ReactNode;
}) {
  return (
    <section aria-label={label}>
      <h3 className="pt-2.5 pb-1">
        <button
          type="button"
          onClick={() => onToggle(section)}
          aria-expanded={!collapsed}
          className="flex w-full items-center gap-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)]"
        >
          {collapsed ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
          {label}
          {count !== null && (
            <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
              {count}
            </span>
          )}
        </button>
      </h3>
      {collapsed ? null : children}
    </section>
  );
}

function Note({ children }: { children: ReactNode }) {
  return (
    <p className="py-1.5 text-[10px] leading-snug text-[var(--text-muted)]">{children}</p>
  );
}

/**
 * The filter for one section, beside the results it filters.
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
    <div role="group" aria-label="Search in" className="flex flex-wrap items-center gap-1 pb-1">
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

function CatalogueAnswerBody({
  answer,
}: {
  answer: { query: string; terms: string[]; hits: CatalogueHit[] };
}) {
  // Two different empty answers. Saying "nothing found" to someone who typed a
  // word the index never looked for would be a lie by omission.
  if (answer.hits.length === 0 && answer.terms.length === 0) {
    return (
      <Note>
        Nothing in “{answer.query}” could be searched for — single letters and
        very common words are dropped before the query runs. Try naming the
        subject in a word or two more.
      </Note>
    );
  }
  if (answer.hits.length === 0) {
    return (
      <Note>
        No catalogue here holds anything matching{" "}
        {answer.terms.map((t) => `“${t}”`).join(", ")}. The mirror may also
        simply be empty — Settings › Catalogues says when each was last fetched.
      </Note>
    );
  }
  return (
    <div className="divide-y divide-[var(--border-main)]">
      {answer.hits.map((hit) => (
        <CatalogueCandidate key={`${hit.provider}:${hit.external_id}`} hit={hit} />
      ))}
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
  const setFilter = useCatalogueStore((s) => s.setBrowseFilter);
  const setSort = useCatalogueStore((s) => s.setBrowseSort);
  const reset = useCatalogueStore((s) => s.resetRefinement);
  const refine = useCatalogueStore((s) => s.refine);
  const controls = info?.capabilities.browse ?? null;
  if (!info || controls === null) return null;
  const provider = info.provider;
  const filters = refinement?.filters ?? {};
  const sort = refinement?.sort ?? null;
  const count = refinementCount(refinement);
  const apply = () => void refine(keyword, provider);

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
          <ControlRow label="Sort by" wide={controls.filters.length === 0}>
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
        {controls.filters.map((filter) => (
          <FilterInput
            key={filter.name}
            filter={filter}
            values={filters}
            onChange={(name, value) => setFilter(provider, name, value)}
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
    if (!info || controls === null) continue;
    const prefix = listed.length > 1 ? `${info.name} · ` : "";
    const refinement = refinements[provider];
    for (const filter of controls.filters) {
      const text = filterSummary(filter, refinement?.filters ?? {});
      if (text !== null) {
        chips.push({
          key: `${provider}:${filter.name}`,
          text: prefix + text,
          clear: () => onClearFilter(provider, filter.name),
        });
      }
    }
    if (refinement?.sort != null && controls.sort !== null) {
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
          title="Remove"
          className="flex items-center gap-1 rounded-full border border-[var(--border-strong)] bg-[var(--bg-active)] px-2 py-0.5 text-[10px] text-[var(--text-main)] transition-colors hover:border-red-400"
        >
          {chip.text}
          <X size={9} />
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

/** How one filter's value reads in a chip, or null when it is not set. */
function filterSummary(filter: FilterControl, values: Record<string, string>): string | null {
  if (filter.kind === "choice") {
    const value = values[filter.name];
    return value === undefined
      ? null
      : `${filter.label}: ${optionLabel(filter.options, value, value)}`;
  }
  if (filter.kind === "flag") return values[filter.name] === "true" ? filter.label : null;
  const min = values[`${filter.name}.min`];
  const max = values[`${filter.name}.max`];
  if (min !== undefined && max !== undefined) return `${filter.label}: ${min}–${max}`;
  if (min !== undefined) return `${filter.label} ≥ ${min}`;
  if (max !== undefined) return `${filter.label} ≤ ${max}`;
  return null;
}

const CONTROL_CLASS =
  "w-full rounded border border-[var(--border-main)] bg-[var(--bg-sidebar)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--border-strong)]";

function ControlRow({
  label,
  wide = false,
  children,
}: {
  label: string;
  wide?: boolean;
  children: ReactNode;
}) {
  return (
    <label className={`flex flex-col gap-0.5 ${wide ? "col-span-2" : ""}`}>
      <span className="text-[9px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
        {label}
      </span>
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

/** One declared filter, as the control its kind calls for. */
function FilterInput({
  filter,
  values,
  onChange,
  onApply,
}: {
  filter: FilterControl;
  values: Record<string, string>;
  onChange: (name: string, value: string) => void;
  onApply: () => void;
}) {
  if (filter.kind === "choice") {
    return (
      <ControlRow label={filter.label}>
        <select
          aria-label={filter.label}
          value={values[filter.name] ?? ""}
          onChange={(e) => {
            onChange(filter.name, e.target.value);
            onApply();
          }}
          className={CONTROL_CLASS}
        >
          <option value="">Any</option>
          {filter.options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </ControlRow>
    );
  }
  if (filter.kind === "flag") {
    return (
      <label className="col-span-2 flex items-center gap-1.5 text-[11px] text-[var(--text-main)]">
        <input
          type="checkbox"
          checked={values[filter.name] === "true"}
          onChange={(e) => {
            onChange(filter.name, e.target.checked ? "true" : "");
            onApply();
          }}
        />
        {filter.label}
      </label>
    );
  }
  return (
    <ControlRow label={filter.label} wide>
      <span className="flex items-center gap-1.5">
        {filter.has_min && (
          <RangeEnd
            label={`${filter.label} min`}
            placeholder={filter.has_max ? "From" : "At least"}
            value={values[`${filter.name}.min`] ?? ""}
            onChange={(v) => onChange(`${filter.name}.min`, v)}
            onApply={onApply}
          />
        )}
        {filter.has_max && (
          <RangeEnd
            label={`${filter.label} max`}
            placeholder={filter.has_min ? "To" : "At most"}
            value={values[`${filter.name}.max`] ?? ""}
            onChange={(v) => onChange(`${filter.name}.max`, v)}
            onApply={onApply}
          />
        )}
      </span>
    </ControlRow>
  );
}

/** One end of a range. Applies when left, and only if it changed while it held
 *  focus; Enter leaves it, so the two ways of finishing are one request. */
function RangeEnd({
  label,
  placeholder,
  value,
  onChange,
  onApply,
}: {
  label: string;
  placeholder: string;
  value: string;
  onChange: (value: string) => void;
  onApply: () => void;
}) {
  const atFocus = useRef(value);
  return (
    <input
      type="number"
      inputMode="numeric"
      aria-label={label}
      placeholder={placeholder}
      value={value}
      onFocus={() => {
        atFocus.current = value;
      }}
      onChange={(e) => onChange(e.target.value)}
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
          filtered{answer !== null ? ` · ${answer.results.length}` : ""}
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
              <PaperCandidate key={work.id} provider={provider} work={work} />
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
 * Every selected provider's part of the Literature section, in the registry's
 * order and each under its own name: a filtered listing for a provider with
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
  // No provider enabled is a setting, not a search result: saying "nothing
  // found" would send someone rephrasing a query no index ever saw.
  if (answer !== null && answer.providers.length === 0 && listed.length === 0) {
    return (
      <Note>
        No provider is enabled, so nothing was searched. Settings › Integrations
        turns on the providers that ship with Wilkes, or one you describe.
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
}: {
  answer: LiteratureAnswer;
  entry: LiteratureProviderAnswer;
  paged: boolean;
}) {
  const moreLoading = useCatalogueStore((s) => s.literatureMore);
  const moreError = useCatalogueStore((s) => s.literatureMoreError);
  const loadMore = useCatalogueStore((s) => s.loadMoreLiterature);
  return (
    <div className="flex flex-col">
      {/* The provider is the strongest label in this half: it says whose
          index a row came from, and it is the only thing that does now
          that no row carries a kind badge. So it reads brighter than the
          section heading above it and sits on a rule of its own, rather
          than as a dim line that scanned as part of the row beneath. */}
      <h4 className="mt-2 flex items-baseline gap-1.5 border-b border-[var(--border-strong)] pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
        {entry.name}
        {entry.results !== null && (
          <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
            {entry.results.length}
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
            <PaperCandidate key={work.id} provider={entry.provider} work={work} />
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
