import { useState } from "react";
import { Search, X } from "react-feather";
import type { CatalogueGrain } from "../lib/types";
import { ALL_GRAINS, useCatalogueStore } from "../stores/useCatalogueStore";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import CatalogueCandidate from "./CatalogueCandidate";

const GRAIN_LABELS: Record<CatalogueGrain, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
};

/**
 * Browsing the teaching catalogues for something to add.
 *
 * Sits with the other ways documents enter a library rather than with the
 * search results: looking for material to acquire is an acquisition, and the
 * pane it opens beside is the file list it will change.
 */
export default function CataloguePane() {
  const closePane = useCatalogueStore((s) => s.closePane);
  const grains = useCatalogueStore((s) => s.grains);
  const toggleGrain = useCatalogueStore((s) => s.toggleGrain);
  const runSearch = useCatalogueStore((s) => s.search);
  const loading = useCatalogueStore((s) => s.loading);
  const answer = useCatalogueStore((s) => s.answer);
  const error = useCatalogueStore((s) => s.error);
  const { readOnly, needsDirectory } = useCatalogueAdd();
  const [draft, setDraft] = useState(useCatalogueStore.getState().query);

  return (
    <div className="flex h-full flex-col border-l border-[var(--border-main)] bg-[var(--bg-sidebar)]">
      <div className="flex items-center justify-between border-b border-[var(--border-main)] px-3 py-2">
        <h2 className="text-[11px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
          Teaching catalogues
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
            placeholder="What do you want to learn?"
            aria-label="Search the teaching catalogues"
            className="w-full bg-transparent py-1.5 text-[11px] text-[var(--text-main)] outline-none placeholder:text-[var(--text-dim)]"
          />
        </div>
        <div className="flex flex-wrap gap-1">
          {ALL_GRAINS.map((grain) => {
            // No grain selected means every grain: the catalogues publish at
            // different grains and a question is rarely answerable at only one.
            const on = grains.length === 0 || grains.includes(grain);
            return (
              <button
                key={grain}
                type="button"
                onClick={() => toggleGrain(grain)}
                aria-pressed={grains.includes(grain)}
                className={`rounded border px-2 py-0.5 text-[9px] font-bold uppercase tracking-wider transition-colors ${
                  on
                    ? "border-[var(--border-strong)] bg-[var(--bg-active)] text-[var(--text-main)]"
                    : "border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-dim)]"
                }`}
              >
                {GRAIN_LABELS[grain]}
              </button>
            );
          })}
        </div>
      </form>

      {(readOnly || needsDirectory) && (
        <p className="border-b border-[var(--border-main)] px-3 py-2 text-[10px] leading-relaxed text-[var(--text-dim)]">
          {readOnly
            ? "This workspace is read-only, so nothing can be added to it. You can still search and open what the catalogues hold."
            : "Choose a directory before adding documents."}
        </p>
      )}

      <div className="flex-1 overflow-y-auto px-3">
        {loading && (
          <div className="flex items-center justify-center py-8">
            <div className="h-5 w-5 animate-spin rounded-full border-2 border-[var(--accent-blue)] border-t-transparent" />
          </div>
        )}

        {error !== null && !loading && (
          <p className="py-3 text-[10px] leading-relaxed text-red-400">{error}</p>
        )}

        {!loading && error === null && answer !== null && (
          <CatalogueAnswerBody answer={answer} />
        )}

        {!loading && error === null && answer === null && (
          <p className="py-3 text-[10px] leading-relaxed text-[var(--text-muted)]">
            Describe what you are trying to understand. These are open textbooks,
            courses and documentation sets — the search is deliberately wide, so
            read the results rather than trusting their order.
          </p>
        )}
      </div>
    </div>
  );
}

function CatalogueAnswerBody({
  answer,
}: {
  answer: { query: string; terms: string[]; hits: import("../lib/types").CatalogueHit[] };
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
