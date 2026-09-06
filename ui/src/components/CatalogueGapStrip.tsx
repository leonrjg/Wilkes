import { useEffect, useState } from "react";
import { api } from "../services";
import type { CatalogueHit } from "../lib/types";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import CatalogueCandidate from "./CatalogueCandidate";

/** Five, because this is an offer made under a failed search, not a reading
 *  list: a strip long enough to scroll is a second set of results. */
const GAP_LIMIT = 5;

interface Props {
  /** What the user asked their own library, reused verbatim as the probe. */
  query: string;
}

/**
 * What the catalogues hold on a question this library could not answer.
 *
 * Shown only under a *completed* search that returned nothing. That is the one
 * moment Wilkes has proof the library falls short and already holds a probe of
 * the shape the mirror's recall wants — the words the user chose. Nothing is
 * fetched on the user's behalf; each row offers, and the user decides.
 */
export default function CatalogueGapStrip({ query }: Props) {
  const [hits, setHits] = useState<CatalogueHit[] | null>(null);
  const [terms, setTerms] = useState<string[]>([]);
  const [mirrorEmpty, setMirrorEmpty] = useState(false);
  const openPane = useCatalogueStore((s) => s.openPane);
  const paneSearch = useCatalogueStore((s) => s.search);

  useEffect(() => {
    let cancelled = false;
    const probe = query.trim();
    if (!probe) {
      setHits(null);
      return;
    }
    setHits(null);
    setMirrorEmpty(false);
    api
      .catalogueSearch([{ key: "gap", text: probe }], GAP_LIMIT)
      .then(async (response) => {
        if (cancelled) return;
        const result = response.results.find((r) => r.key === "gap");
        setTerms(result?.terms ?? []);
        setHits(result?.hits ?? []);
        // "Nothing matched" and "nothing has been fetched yet" read the same
        // from here, and only one of them is the user's to fix. One status
        // call, and only on the path where the difference matters.
        if ((result?.hits.length ?? 0) === 0 && (result?.terms.length ?? 0) > 0) {
          const status = await api.catalogueStatus().catch(() => null);
          if (!cancelled && status !== null) setMirrorEmpty(status.total_records === 0);
        }
      })
      .catch((error) => {
        // A catalogue that cannot answer must not turn an empty search into an
        // error message about a feature the user did not invoke.
        console.debug("catalogue gap lookup failed:", error);
        if (!cancelled) setHits([]);
      });
    return () => {
      cancelled = true;
    };
  }, [query]);

  // Nothing usable in the query means nothing to offer here. The browse pane
  // explains the rule; under a failed search it would just be noise.
  if (hits === null || terms.length === 0) return null;

  if (hits.length === 0) {
    if (!mirrorEmpty) return null;
    return (
      <div className="mx-4 mb-4 rounded-lg border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-2">
        <p className="text-[10px] leading-relaxed text-[var(--text-muted)]">
          The learning catalogues have not been fetched yet. Settings ›
          Catalogues will fill them, and this search can then suggest open
          textbooks and courses when your library has nothing.
        </p>
      </div>
    );
  }

  return (
    <div className="mx-4 mb-4 rounded-lg border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-2 text-left">
      <div className="flex items-center justify-between gap-2 pb-1">
        <h3 className="text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
          Nothing here teaches this
        </h3>
        <button
          type="button"
          onClick={() => {
            openPane();
            void paneSearch(query);
          }}
          className="text-[10px] text-[var(--text-muted)] underline transition-colors hover:text-[var(--text-main)]"
        >
          Search the catalogues
        </button>
      </div>
      <p className="pb-1 text-[10px] leading-relaxed text-[var(--text-muted)]">
        Open textbooks, courses and documentation that mention it. Ordered by a
        text match, not by which is the better place to start.
      </p>
      <div className="divide-y divide-[var(--border-main)]">
        {hits.map((hit) => (
          <CatalogueCandidate
            key={`${hit.provider}:${hit.external_id}`}
            hit={hit}
            compact
          />
        ))}
      </div>
    </div>
  );
}

/**
 * The same offer, for a search that *did* return something.
 *
 * A line rather than a strip, and it fetches nothing until it is clicked.
 * Wilkes has no basis for deciding that a user's own results are inadequate —
 * grep matches carry no relevance score, and a threshold would be an invented
 * one — so the judgement stays theirs and this only makes the catalogues
 * reachable from the moment they might be wanted.
 */
export function CatalogueGapPrompt({ query }: Props) {
  const openPane = useCatalogueStore((s) => s.openPane);
  const paneSearch = useCatalogueStore((s) => s.search);
  if (query.trim() === "") return null;
  return (
    <div className="px-4 py-3 text-center">
      <button
        type="button"
        onClick={() => {
          openPane();
          void paneSearch(query);
        }}
        className="text-[10px] text-[var(--text-dim)] underline transition-colors hover:text-[var(--text-main)]"
      >
        Nothing here teaches this? Search the open catalogues
      </button>
    </div>
  );
}
