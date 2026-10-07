import { useEffect, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, Search, X } from "react-feather";
import type {
  CatalogueGrain,
  CatalogueHit,
  FilterControl,
  LiteratureProviderInfo,
} from "../lib/types";
import {
  ALL_GRAINS,
  browseProviderIds,
  type BrowseAnswer,
  enabledProviderIds,
  type LiteratureAnswer,
  type PaneSection,
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
  const browseAnswer = useCatalogueStore((s) => s.browseAnswer);
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
  const literatureCount =
    literature === null
      ? null
      : literature.providers
          .filter((entry) => selects(providers, entry.provider))
          .reduce((total, entry) => total + (entry.results?.length ?? 0), 0);
  const asked =
    (selectsAny(grains) && (loading || error !== null || answer !== null)) ||
    (selectsAny(providers) &&
      (literatureLoading || literatureError !== null || literature !== null));

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
        className="border-b border-[var(--border-main)] px-3 py-1.5"
        onSubmit={(e) => {
          e.preventDefault();
          void runSearch(draft);
        }}
      >
        <div className="flex items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2">
          <Search size={12} className="shrink-0 text-[var(--text-dim)]" />
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="What do you want to learn or read?"
            aria-label="Search catalogues and literature"
            className="w-full bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
          />
        </div>
      </form>

      {(readOnly || needsDirectory) && (
        <p className="border-b border-[var(--border-main)] px-3 py-1.5 text-[10px] leading-snug text-[var(--text-dim)]">
          {readOnly
            ? "This workspace is read-only. You can search and open what the sources hold, but nothing can be added to it."
            : "Choose a directory before adding documents."}
        </p>
      )}

      <div className="flex-1 overflow-y-auto px-3 pb-2">
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
              {!literatureLoading && literature !== null && (
                <LiteratureAnswerBody answer={literature} selection={providers} />
              )}
            </>
          )}
          <BrowsePanel keyword={draft} options={providerOptions} />
          {browseAnswer !== null && <BrowseAnswerBody answer={browseAnswer} />}
        </Section>

        {!asked && (
          <Note>
            Describe what you are trying to understand. The catalogues are open
            textbooks, courses and documentation sets; the providers are the
            live sources enabled in Settings › Integrations.
          </Note>
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
    <div className="flex flex-wrap gap-1 pb-1">
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
 * Listing a provider's works by filter and sort, for when there is no keyword
 * to type.
 *
 * Shown only when an enabled provider declares a listing, and drawn entirely
 * from that declaration: the sort, the filters and what kind each is all come
 * from the provider's manifest, so a provider added tomorrow gets a panel with
 * no change here. The keyword above, if there is one, is combined with the
 * filters when the provider's listing takes one.
 */
function BrowsePanel({
  keyword,
  options,
}: {
  keyword: string;
  options: LiteratureProviderInfo[];
}) {
  const open = useCatalogueStore((s) => s.browseOpen);
  const setOpen = useCatalogueStore((s) => s.setBrowseOpen);
  const chosen = useCatalogueStore((s) => s.browseProvider);
  const selectProvider = useCatalogueStore((s) => s.selectBrowseProvider);
  const filters = useCatalogueStore((s) => s.browseFilters);
  const setFilter = useCatalogueStore((s) => s.setBrowseFilter);
  const sort = useCatalogueStore((s) => s.browseSort);
  const setSort = useCatalogueStore((s) => s.setBrowseSort);
  const loading = useCatalogueStore((s) => s.browseLoading);
  const error = useCatalogueStore((s) => s.browseError);
  const browse = useCatalogueStore((s) => s.browse);

  const offered = browseProviderIds(options);
  if (offered.length === 0) return null;
  const provider = chosen !== null && offered.includes(chosen) ? chosen : offered[0];
  const info = options.find((option) => option.provider === provider);
  const controls = info?.capabilities.browse ?? null;
  if (!info || controls === null) return null;

  return (
    <div className="border-t border-[var(--border-main)] pt-1.5">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full items-center gap-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:text-[var(--text-main)]"
      >
        {open ? <ChevronDown size={11} /> : <ChevronRight size={11} />}
        Browse by filter
      </button>
      {open && (
        <form
          className="flex flex-col gap-1.5 py-1.5"
          aria-label="Browse by filter"
          onSubmit={(e) => {
            e.preventDefault();
            void browse(keyword);
          }}
        >
          {offered.length > 1 && (
            <ControlRow label="Provider">
              <select
                aria-label="Provider to browse"
                value={provider}
                onChange={(e) => selectProvider(e.target.value)}
                className={CONTROL_CLASS}
              >
                {offered.map((id) => (
                  <option key={id} value={id}>
                    {options.find((option) => option.provider === id)?.name ?? id}
                  </option>
                ))}
              </select>
            </ControlRow>
          )}
          {controls.sort !== null && (
            <ControlRow label="Sort">
              <select
                aria-label="Sort"
                value={sort ?? ""}
                onChange={(e) => setSort(e.target.value === "" ? null : e.target.value)}
                className={CONTROL_CLASS}
              >
                <option value="">
                  {optionLabel(controls.sort.options, controls.sort.default)} (default)
                </option>
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
              onChange={setFilter}
            />
          ))}
          <div className="flex items-center gap-2 pt-0.5">
            <button
              type="submit"
              disabled={loading}
              className="rounded border border-[var(--border-strong)] bg-[var(--bg-active)] px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)] disabled:opacity-50"
            >
              {loading ? "Listing" : "Show works"}
            </button>
            {controls.keyword && keyword.trim() !== "" && (
              <span className="text-[10px] text-[var(--text-dim)]">
                with “{keyword.trim()}”
              </span>
            )}
          </div>
          {error !== null && !loading && (
            <p className="text-[10px] leading-snug text-red-400">
              {info.name} could not list works: {error}
            </p>
          )}
        </form>
      )}
    </div>
  );
}

const CONTROL_CLASS =
  "w-full rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none";

function ControlRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-0.5">
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
): string {
  return options.find((option) => option.value === value)?.label ?? value;
}

/** One declared filter, as the control its kind calls for. */
function FilterInput({
  filter,
  values,
  onChange,
}: {
  filter: FilterControl;
  values: Record<string, string>;
  onChange: (name: string, value: string) => void;
}) {
  if (filter.kind === "choice") {
    return (
      <ControlRow label={filter.label}>
        <select
          aria-label={filter.label}
          value={values[filter.name] ?? ""}
          onChange={(e) => onChange(filter.name, e.target.value)}
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
      <label className="flex items-center gap-1.5 text-[11px] text-[var(--text-main)]">
        <input
          type="checkbox"
          checked={values[filter.name] === "true"}
          onChange={(e) => onChange(filter.name, e.target.checked ? "true" : "")}
        />
        {filter.label}
      </label>
    );
  }
  return (
    <ControlRow label={filter.label}>
      <span className="flex items-center gap-1.5">
        {filter.has_min && (
          <input
            type="number"
            inputMode="numeric"
            aria-label={`${filter.label} min`}
            placeholder={filter.has_max ? "From" : "At least"}
            value={values[`${filter.name}.min`] ?? ""}
            onChange={(e) => onChange(`${filter.name}.min`, e.target.value)}
            className={CONTROL_CLASS}
          />
        )}
        {filter.has_max && (
          <input
            type="number"
            inputMode="numeric"
            aria-label={`${filter.label} max`}
            placeholder={filter.has_min ? "To" : "At most"}
            value={values[`${filter.name}.max`] ?? ""}
            onChange={(e) => onChange(`${filter.name}.max`, e.target.value)}
            className={CONTROL_CLASS}
          />
        )}
      </span>
    </ControlRow>
  );
}

/** What the listing returned, under the provider's name, with the next page
 *  one press away. */
function BrowseAnswerBody({ answer }: { answer: BrowseAnswer }) {
  const loading = useCatalogueStore((s) => s.browseLoading);
  const more = useCatalogueStore((s) => s.browseMore);
  return (
    <div className="flex flex-col" aria-label={`${answer.name} listing`}>
      <h4 className="mt-2 flex items-baseline gap-1.5 border-b border-[var(--border-strong)] pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)]">
        {answer.name}
        <span className="font-normal normal-case tracking-normal text-[var(--text-muted)]">
          listing · {answer.results.length}
        </span>
      </h4>
      {answer.results.length === 0 ? (
        <Note>Nothing in {answer.name} matches these filters.</Note>
      ) : (
        <div className="divide-y divide-[var(--border-main)]">
          {answer.results.map((work) => (
            <PaperCandidate key={work.id} provider={answer.provider} work={work} />
          ))}
        </div>
      )}
      {!answer.exhausted && (
        <button
          type="button"
          onClick={() => void more()}
          disabled={loading}
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
 * Every selected provider's answer, each under its own name.
 *
 * The held answers can cover more providers than the filter admits — narrowing
 * a filter hides a provider rather than re-asking the rest — so what is listed
 * is the intersection, not everything that replied.
 */
function LiteratureAnswerBody({
  answer,
  selection,
}: {
  answer: LiteratureAnswer;
  selection: Selection<string>;
}) {
  // No provider enabled is a setting, not a search result: saying "nothing
  // found" would send someone rephrasing a query no index ever saw.
  if (answer.providers.length === 0) {
    return (
      <Note>
        No provider is enabled, so nothing was searched. Settings › Integrations
        turns on the providers that ship with Wilkes, or one you describe.
      </Note>
    );
  }
  const shown = answer.providers.filter((entry) => selects(selection, entry.provider));
  return (
    <div className="flex flex-col">
      {shown.map((entry) => (
        <div key={entry.provider} className="flex flex-col">
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
        </div>
      ))}
    </div>
  );
}
