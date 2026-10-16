import { useEffect, useState } from "react";
import { api } from "../services";
import type { LiteratureSearchResult } from "../lib/types";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import CatalogueCandidate from "./CatalogueCandidate";

/** Five, because this is an offer made under a failed search, not a reading
 *  list: a strip long enough to scroll is a second set of results. */
const GAP_LIMIT = 5;

interface Props {
  /** What the user asked their own library, reused verbatim as the probe. */
  query: string;
}

/** One result and the catalogue it came from: the strip mixes catalogues, so
 *  each row has to say whose it is. */
interface Offer {
  provider: string;
  name: string;
  work: LiteratureSearchResult;
}

/** What the strip found, or why it has nothing to show. */
type Gap =
  | { kind: "offers"; offers: Offer[] }
  /** No catalogue has been fetched: the one empty answer that is the user's to
   *  fix. */
  | { kind: "unfetched" }
  | { kind: "nothing" };

/**
 * What the catalogues hold on a question this library could not answer.
 *
 * Shown only under a *completed* search that returned nothing. That is the one
 * moment Wilkes has proof the library falls short and already holds a probe of
 * the shape the mirror's recall wants — the words the user chose. Nothing is
 * fetched on the user's behalf; each row offers, and the user decides.
 *
 * It asks the mirrored providers and no others. This runs unprompted after
 * every empty search, and a provider answered from this machine is the only
 * kind that can be asked without sending the user's query somewhere they did
 * not send it. Which providers those are is the registry's answer
 * (`capabilities.mirrored`), not a list kept here.
 */
export default function CatalogueGapStrip({ query }: Props) {
  const [gap, setGap] = useState<Gap | null>(null);
  const openPane = useCatalogueStore((s) => s.openPane);
  const paneSearch = useCatalogueStore((s) => s.search);

  useEffect(() => {
    let cancelled = false;
    const probe = query.trim();
    setGap(null);
    if (!probe) return;
    (async () => {
      const local = (await api.literatureProviders()).filter(
        (provider) => provider.capabilities.mirrored,
      );
      const held = local.filter((provider) => provider.enabled);
      if (held.length === 0) {
        // No catalogue in this build is a different thing from none fetched,
        // and only the second has a setting to point at.
        return local.length === 0 ? { kind: "nothing" as const } : { kind: "unfetched" as const };
      }
      const response = await api.literatureSearch(
        probe,
        GAP_LIMIT,
        held.map((provider) => provider.provider),
      );
      const offers: Offer[] = [];
      for (const entry of response.providers) {
        // A catalogue that could not answer — a probe with nothing searchable
        // in it, say — is left out. The pane explains the rule; under a failed
        // search it would be noise about a feature the user did not invoke.
        if (entry.results === null) {
          console.debug(`catalogue gap lookup: ${entry.provider} did not answer:`, entry.error);
          continue;
        }
        for (const work of entry.results) {
          offers.push({ provider: entry.provider, name: entry.name, work });
        }
      }
      // The catalogues share one index, so their scores are one scale and the
      // strip is the best five across them rather than each one's best.
      offers.sort((a, b) => (b.work.recall_score ?? 0) - (a.work.recall_score ?? 0));
      return offers.length === 0
        ? { kind: "nothing" as const }
        : { kind: "offers" as const, offers: offers.slice(0, GAP_LIMIT) };
    })()
      .then((found) => {
        if (!cancelled) setGap(found);
      })
      .catch((error) => {
        // A catalogue that cannot answer must not turn an empty search into an
        // error message about a feature the user did not invoke.
        console.debug("catalogue gap lookup failed:", error);
        if (!cancelled) setGap({ kind: "nothing" });
      });
    return () => {
      cancelled = true;
    };
  }, [query]);

  if (gap === null || gap.kind === "nothing") return null;

  if (gap.kind === "unfetched") {
    return (
      <div className="mx-4 mb-4 rounded-lg border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-2">
        <p className="text-[10px] leading-relaxed text-[var(--text-muted)]">
          Sync catalogues in Settings › Integrations › Catalogues to get
          textbook and course suggestions.
        </p>
      </div>
    );
  }

  return (
    <div className="mx-4 mb-4 rounded-lg border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-2 text-left">
      <div className="flex items-center justify-between gap-2 pb-1">
        <h3 className="text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)]">
          Related learning resources
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
        Ranked by text relevance.
      </p>
      <div className="divide-y divide-[var(--border-main)]">
        {gap.offers.map(({ provider, name, work }) => (
          <CatalogueCandidate
            key={`${provider}:${work.id}`}
            provider={provider}
            work={work}
            source={name}
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
        Search learning catalogues
      </button>
    </div>
  );
}
