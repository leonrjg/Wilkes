import { useState, type ReactNode } from "react";
import { Search, X } from "react-feather";
import type { CatalogueHit } from "../lib/types";
import {
  ALL_KINDS,
  type LiteratureAnswer,
  type SearchKind,
  useCatalogueStore,
  wantsCatalogue,
  wantsLiterature,
} from "../stores/useCatalogueStore";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import CatalogueCandidate, { PaperCandidate } from "./CatalogueCandidate";

const KIND_LABELS: Record<SearchKind, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
  paper: "Papers",
};

/**
 * Looking for something to add: the open learning catalogues and the
 * scholarly literature providers, asked one question at once.
 *
 * Sits with the other ways documents enter a library rather than with the
 * search results: looking for material to acquire is an acquisition, and the
 * pane it opens beside is the file list it will change.
 *
 * The two kinds of source are searched together and listed apart. They are
 * different answers — a textbook that teaches a subject, a paper that reports
 * on it — each with its own empty states, and neither is ranked against the
 * other.
 */
export default function CataloguePane() {
  const closePane = useCatalogueStore((s) => s.closePane);
  const kinds = useCatalogueStore((s) => s.kinds);
  const toggleKind = useCatalogueStore((s) => s.toggleKind);
  const runSearch = useCatalogueStore((s) => s.search);
  const loading = useCatalogueStore((s) => s.loading);
  const answer = useCatalogueStore((s) => s.answer);
  const error = useCatalogueStore((s) => s.error);
  const literatureLoading = useCatalogueStore((s) => s.literatureLoading);
  const literature = useCatalogueStore((s) => s.literature);
  const literatureError = useCatalogueStore((s) => s.literatureError);
  const acquireError = useCatalogueStore((s) => s.acquireError);
  const { readOnly, needsDirectory } = useCatalogueAdd();
  const [draft, setDraft] = useState(useCatalogueStore.getState().query);

  const showCatalogue = wantsCatalogue(kinds);
  const showLiterature = wantsLiterature(kinds);
  const catalogueAsked = loading || error !== null || answer !== null;
  const literatureAsked = literatureLoading || literatureError !== null || literature !== null;
  const asked = (showCatalogue && catalogueAsked) || (showLiterature && literatureAsked);
  // Section headings only earn their place when there is more than one
  // section to tell apart.
  const both = showCatalogue && showLiterature;

  return (
    <div className="flex h-full flex-col border-l border-[var(--border-main)] bg-[var(--bg-sidebar)]">
      <div className="flex items-center justify-between border-b border-[var(--border-main)] px-3 py-2">
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
        className="flex flex-col gap-2 border-b border-[var(--border-main)] px-3 py-2"
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
        <div className="flex flex-wrap gap-1">
          {ALL_KINDS.map((kind) => {
            // No kind selected means every kind: the sources publish at
            // different grains and a question is rarely answerable at only one.
            const on = kinds.length === 0 || kinds.includes(kind);
            return (
              <button
                key={kind}
                type="button"
                onClick={() => toggleKind(kind)}
                aria-pressed={kinds.includes(kind)}
                className={`rounded border px-2 py-0.5 text-[9px] font-bold uppercase tracking-wider transition-colors ${
                  on
                    ? "border-[var(--border-strong)] bg-[var(--bg-active)] text-[var(--text-main)]"
                    : "border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-dim)]"
                }`}
              >
                {KIND_LABELS[kind]}
              </button>
            );
          })}
        </div>
      </form>

      {(readOnly || needsDirectory) && (
        <p className="border-b border-[var(--border-main)] px-3 py-2 text-[10px] leading-relaxed text-[var(--text-dim)]">
          {readOnly
            ? "This workspace is read-only, so nothing can be added to it. You can still search and open what the sources hold."
            : "Choose a directory before adding documents."}
        </p>
      )}

      <div className="flex-1 overflow-y-auto px-3">
        {acquireError !== null && (
          <p className="py-2 text-[10px] leading-relaxed text-red-400">{acquireError}</p>
        )}

        {showCatalogue && catalogueAsked && (
          <section aria-label="Open catalogues">
            {both && <SectionHeading>Open catalogues</SectionHeading>}
            {loading && <Spinner />}
            {error !== null && !loading && (
              <p className="py-3 text-[10px] leading-relaxed text-red-400">{error}</p>
            )}
            {!loading && error === null && answer !== null && (
              <CatalogueAnswerBody answer={answer} />
            )}
          </section>
        )}

        {showLiterature && literatureAsked && (
          <section aria-label="Papers">
            {both && <SectionHeading>Papers</SectionHeading>}
            {literatureLoading && <Spinner />}
            {literatureError !== null && !literatureLoading && (
              <p className="py-3 text-[10px] leading-relaxed text-red-400">{literatureError}</p>
            )}
            {!literatureLoading && literatureError === null && literature !== null && (
              <LiteratureAnswerBody answer={literature} />
            )}
          </section>
        )}

        {!asked && (
          <p className="py-3 text-[10px] leading-relaxed text-[var(--text-muted)]">
            Describe what you are trying to understand. This searches open
            textbooks, courses and documentation sets, and the scholarly
            literature providers enabled in Settings › Integrations — the search
            is deliberately wide, so read the results rather than trusting their
            order.
          </p>
        )}
      </div>
    </div>
  );
}

function SectionHeading({ children }: { children: ReactNode }) {
  return (
    <h3 className="pt-3 pb-1 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
      {children}
    </h3>
  );
}

function Spinner() {
  return (
    <div className="flex items-center justify-center py-6">
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
      <p className="py-3 text-[10px] leading-relaxed text-[var(--text-muted)]">
        Nothing in “{answer.query}” could be searched for — single letters and
        very common words are dropped before the query runs. Try naming the
        subject in a word or two more.
      </p>
    );
  }
  if (answer.hits.length === 0) {
    return (
      <p className="py-3 text-[10px] leading-relaxed text-[var(--text-muted)]">
        No catalogue here holds anything matching {answer.terms.map((t) => `“${t}”`).join(", ")}.
        The mirror may also simply be empty — Settings › Catalogues says when
        each was last fetched.
      </p>
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

/** An entry with neither results nor an error breaks the response contract.
 *  It is reported as a failure rather than read as "nothing found". */
function providerAnswerMalformed(provider: string): string {
  console.error(`literature search: provider ${provider} answered with neither results nor an error`);
  return "the answer carried neither results nor an error";
}

function LiteratureAnswerBody({ answer }: { answer: LiteratureAnswer }) {
  // No provider enabled is a setting, not a search result: saying "no papers
  // found" would send someone rephrasing a query no index ever saw.
  if (answer.providers.length === 0) {
    return (
      <p className="py-3 text-[10px] leading-relaxed text-[var(--text-muted)]">
        No literature provider is enabled, so no papers were searched for.
        Settings › Integrations turns on OpenAlex, Semantic Scholar, or a
        provider you describe.
      </p>
    );
  }
  return (
    <div className="flex flex-col">
      {answer.providers.map((entry) => (
        <div key={entry.provider} className="flex flex-col">
          <span className="pt-2 text-[10px] text-[var(--text-dim)]">{entry.name}</span>
          {entry.error !== null || entry.results === null ? (
            <p className="py-2 text-[10px] leading-relaxed text-red-400">
              {entry.name} could not answer: {entry.error ?? providerAnswerMalformed(entry.provider)}
            </p>
          ) : entry.results.length === 0 ? (
            <p className="py-2 text-[10px] leading-relaxed text-[var(--text-muted)]">
              {entry.name} returned nothing for “{answer.query}”.
            </p>
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
