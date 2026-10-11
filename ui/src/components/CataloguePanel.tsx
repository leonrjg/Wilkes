import { useCallback, useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type {
  CatalogueFetchProgress,
  CatalogueSyncOutcome,
  LiteratureProviderInfo,
  MirrorStatus,
} from "../lib/types";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import { Tooltip } from "@leonrjg/wilkes-reader";

/**
 * What this panel knows about one provider's last sync attempt.
 *
 * A call that never reached the backend has no outcome to report — not one
 * with the fields blanked out, which would mean inventing a grain and a
 * provider id the reply never contained.
 */
type ProviderReport =
  | { kind: "outcome"; outcome: CatalogueSyncOutcome }
  | { kind: "error"; message: string };

interface Props {
  api: SearchApi;
  /** Told after a catalogue is fetched or removed, which is what turns it on
   *  or off as a provider: whoever lists providers is now out of date. */
  onChanged?: () => void;
}

/** One catalogue as this panel shows it: a provider that is answered from the
 *  mirror, and what the mirror holds for it. */
interface Catalogue {
  provider: string;
  name: string;
  mirror: MirrorStatus;
}

/** The providers that keep a mirror, in registry order. Which they are is the
 *  registry's answer; this panel names no catalogue. */
function catalogues(providers: LiteratureProviderInfo[]): Catalogue[] {
  return providers.flatMap((info) =>
    info.mirror === null ? [] : [{ provider: info.provider, name: info.name, mirror: info.mirror }],
  );
}

const GRAIN_LABELS: Record<string, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
};

/** Whether the mirror has this catalogue, which is what makes it a provider
 *  that is searched. */
function isHeld(catalogue: Catalogue): boolean {
  return catalogue.mirror.synced_at_ms !== null || catalogue.mirror.records > 0;
}

function formatSyncedAt(syncedAtMs: number | null): string {
  if (syncedAtMs === null) return "Never";
  const elapsed = Date.now() - syncedAtMs;
  if (elapsed < 60_000) return "Just now";
  if (elapsed < 3_600_000) return `${Math.floor(elapsed / 60_000)} min ago`;
  if (elapsed < 86_400_000) return `${Math.floor(elapsed / 3_600_000)} h ago`;
  const days = Math.floor(elapsed / 86_400_000);
  return days === 1 ? "Yesterday" : `${days} days ago`;
}

/**
 * What one sync did, in the terms the store reports it.
 *
 * `offered` and `stored` are both shown when they differ, because they do
 * differ — LibreTexts and MIT OpenCourseWare repeat ids across a paged fetch —
 * and a panel that showed only the stored count would make a provider whose
 * pagination has changed under us look like one that simply shrank.
 */
function outcomeSummary(report: ProviderReport): string {
  if (report.kind === "error") return report.message;
  const outcome = report.outcome;
  if (outcome.error !== null) return outcome.error;
  const stored = outcome.records ?? 0;
  const parts: string[] = [`${stored.toLocaleString()} stored`];
  if (outcome.offered !== null && outcome.offered !== stored) {
    parts.push(`${outcome.offered.toLocaleString()} offered`);
  }
  if (outcome.duplicates) parts.push(`${outcome.duplicates.toLocaleString()} duplicate`);
  if (outcome.unusable) parts.push(`${outcome.unusable.toLocaleString()} unusable`);
  return parts.join(", ");
}

/**
 * The learning catalogues: the providers that are answered from a copy kept on
 * this machine, and so have to be fetched before they can answer.
 *
 * A tab of the integrations settings, beside the providers asked over the
 * network, because that is what a catalogue is. What it has that they do not
 * is here: fetching it, which is also what switches it on, and dropping what
 * was fetched, which switches it off.
 */
export default function CataloguePanel({ api, onChanged }: Props) {
  const [status, setStatus] = useState<Catalogue[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<string | null>(null);
  const [queued, setQueued] = useState<string[]>([]);
  const [reports, setReports] = useState<Record<string, ProviderReport>>({});
  /** The last page reported by each provider still fetching. A whole-catalogue
   *  fetch is minutes long; without this the row says "Fetching…" and then
   *  jumps to a final count, which is indistinguishable from being stuck. */
  const [fetching, setFetching] = useState<Record<string, CatalogueFetchProgress>>({});
  const [removing, setRemoving] = useState<string | null>(null);
  /** The provider whose Remove has been clicked once. Refetching a catalogue is
   *  minutes, so dropping one takes a second click on the same button. */
  const [armed, setArmed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(catalogues(await api.literatureProviders()));
    } catch (e: any) {
      console.error("catalogue: the mirror could not be read", e);
      setError(e?.toString?.() ?? "Failed to read the catalogue mirror");
    }
  }, [api]);

  useEffect(() => {
    setError(null);
    refresh();
  }, [refresh]);

  /** A catalogue was fetched or dropped, so it is now a provider that is on or
   *  one that is off: the pane's list and the tab strip's are both stale. */
  const noteChanged = useCallback(async () => {
    await refresh();
    void useCatalogueStore.getState().loadProviders();
    onChanged?.();
  }, [refresh, onChanged]);

  // Subscribed while the tab is open rather than globally: this is the only
  // surface that renders a provider fetch, and a listener outliving it would
  // be one nobody reads.
  useEffect(() => {
    let mounted = true;
    let unlisten: (() => void) | undefined;
    api
      .onCatalogueSyncProgress((progress) => {
        if (!mounted) return;
        setFetching((prev) => ({ ...prev, [progress.provider]: progress }));
      })
      .then((stop) => {
        if (!mounted) stop();
        else unlisten = stop;
      })
      .catch((e) => console.debug("catalogue: progress unavailable", e));
    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [api]);

  /**
   * Syncs the named providers one at a time.
   *
   * One at a time rather than one call for all of them: fetching all four is a
   * minutes-long request, and a single call would leave the panel with nothing
   * to say until every provider had finished or failed. The route accepts a
   * list precisely so a caller that wants to show progress can decline to use
   * it.
   */
  const syncProviders = async (providers: string[]) => {
    setError(null);
    setQueued(providers.slice(1));
    for (const [index, provider] of providers.entries()) {
      setSyncing(provider);
      setQueued(providers.slice(index + 1));
      // Last run's pages would otherwise be this run's opening figure.
      setFetching((prev) => {
        const { [provider]: _previous, ...rest } = prev;
        return rest;
      });
      try {
        const response = await api.catalogueSync([provider]);
        const outcome = response.providers.find((p) => p.provider === provider);
        if (outcome) {
          setReports((prev) => ({ ...prev, [provider]: { kind: "outcome", outcome } }));
        }
      } catch (e: any) {
        // The loop continues: one provider being down is not the others being
        // down, and stopping here would hide that the rest would have worked.
        setReports((prev) => ({
          ...prev,
          [provider]: { kind: "error", message: e?.toString?.() ?? "Sync failed" },
        }));
      }
    }
    setSyncing(null);
    setQueued([]);
    setFetching({});
    await noteChanged();
  };

  /**
   * Drops one catalogue from the mirror.
   *
   * The row's last sync report goes with it: "2,219 stored" under a catalogue
   * that now holds nothing would be this panel contradicting the mirror.
   */
  const removeProvider = async (provider: string) => {
    setError(null);
    setArmed(null);
    setRemoving(provider);
    try {
      await api.catalogueRemove(provider);
      setReports((prev) => {
        const { [provider]: _removed, ...rest } = prev;
        return rest;
      });
    } catch (e: any) {
      console.error("catalogue: removal failed", e);
      setError(e?.toString?.() ?? "Could not remove the catalogue");
    }
    setRemoving(null);
    await noteChanged();
  };

  const busy = syncing !== null || removing !== null;

  if (error && status === null) {
    return (
      <div className="p-4 bg-red-900/20 border border-red-900/50 rounded-lg">
        <p className="text-xs text-red-400 leading-relaxed">{error}</p>
        <button
          onClick={() => {
            setError(null);
            refresh();
          }}
          className="mt-2 text-[10px] text-red-400 underline hover:text-red-300"
        >
          Try again
        </button>
      </div>
    );
  }

  if (status === null) {
    return (
      <div className="flex items-center justify-center h-32">
        <div className="w-5 h-5 border-2 border-[var(--accent-blue)] border-t-transparent rounded-full animate-spin" />
      </div>
    );
  }

  const totalRecords = status.reduce((total, catalogue) => total + catalogue.mirror.records, 0);
  const empty = totalRecords === 0;
  // The mirror is what says which catalogues this installation has, so the one
  // button refreshes those and leaves a removed catalogue removed. With none
  // held there is nothing to refresh, and it fills the mirror instead.
  const held = status.filter(isHeld);
  const syncTargets = held.length > 0 ? held : status;
  const syncsEverything = syncTargets.length === status.length;

  return (
    <div className="flex flex-col gap-6">
      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">
            Learning catalogues
          </h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            Open textbook, course and documentation catalogues, searched from a
            copy kept on this machine: fetching one is what switches it on, and
            asking it sends your query nowhere. A search your library cannot
            answer can then suggest something that would. Shared by every
            workspace on this machine, and refreshed only when you ask.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-1">
              <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">
                Records held
              </span>
              <span className="text-[10px] text-[var(--text-main)] font-mono">
                {totalRecords.toLocaleString()}
              </span>
            </div>
            <button
              onClick={() => syncProviders(syncTargets.map((p) => p.provider))}
              disabled={busy}
              className="px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors disabled:opacity-50"
            >
              {syncing !== null ? "Syncing…" : syncsEverything ? "Sync all" : "Sync held"}
            </button>
          </div>

          {empty && !busy && (
            <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
              This mirror is empty. Syncing fetches each catalogue in full, which
              takes a few minutes and a few megabytes.
            </p>
          )}

          <div className="flex flex-col divide-y divide-[var(--border-main)]">
            {status.map((provider) => (
              <ProviderRow
                key={provider.provider}
                provider={provider}
                report={reports[provider.provider] ?? null}
                progress={fetching[provider.provider] ?? null}
                syncing={syncing === provider.provider}
                queued={queued.includes(provider.provider)}
                disabled={busy}
                onSync={() => syncProviders([provider.provider])}
                removing={removing === provider.provider}
                armed={armed === provider.provider}
                onArm={(next) => setArmed(next ? provider.provider : null)}
                onRemove={() => removeProvider(provider.provider)}
              />
            ))}
          </div>
        </div>

        {error !== null && (
          <p className="mt-2 text-[10px] text-red-400 leading-relaxed">{error}</p>
        )}
      </section>
    </div>
  );
}

function ProviderRow({
  provider,
  report,
  progress,
  syncing,
  queued,
  disabled,
  onSync,
  removing,
  armed,
  onArm,
  onRemove,
}: {
  provider: Catalogue;
  report: ProviderReport | null;
  progress: CatalogueFetchProgress | null;
  syncing: boolean;
  queued: boolean;
  disabled: boolean;
  onSync: () => void;
  removing: boolean;
  armed: boolean;
  onArm: (armed: boolean) => void;
  onRemove: () => void;
}) {
  const failed =
    report !== null &&
    (report.kind === "error" || report.outcome.error !== null);
  return (
    <div className="flex items-center justify-between gap-3 py-2 first:pt-0 last:pb-0">
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex items-center gap-2">
          <span className="text-[11px] text-[var(--text-main)] font-medium truncate">
            {provider.name}
          </span>
          <span className="px-1.5 py-0.5 rounded bg-[var(--bg-app)] border border-[var(--border-main)] text-[9px] uppercase tracking-wider text-[var(--text-dim)]">
            {GRAIN_LABELS[provider.mirror.grain] ?? provider.mirror.grain}
          </span>
        </div>
        <span
          className={`text-[10px] leading-relaxed ${
            failed ? "text-red-400" : "text-[var(--text-muted)]"
          }`}
        >
          {removing
            ? "Removing…"
            : syncing
              ? progress === null
                ? "Fetching…"
                : `Fetching… page ${progress.pages.toLocaleString()}, ${progress.records.toLocaleString()} records`
              : queued
                ? "Waiting"
                : report !== null
                  ? outcomeSummary(report)
                  : provider.mirror.synced_at_ms === null
                    ? "Never synced"
                    : `${provider.mirror.records.toLocaleString()} records · ${formatSyncedAt(provider.mirror.synced_at_ms)}`}
        </span>
      </div>
      <div className="flex shrink-0 items-center gap-1.5">
        {isHeld(provider) && (
          <Tooltip
            content={
              armed
                ? "Click again to drop these records from the mirror"
                : "Stop searching this catalogue and drop its records. Syncing it again brings it back."
            }
          >
            <button
              onClick={armed ? onRemove : () => onArm(true)}
              // Both, because a WebKit button is not focused by a click and
              // so never blurs.
              onBlur={() => onArm(false)}
              onMouseLeave={() => onArm(false)}
              disabled={disabled}
              aria-label={`${armed ? "Confirm removing" : "Remove"} ${provider.name}`}
              className={`px-2.5 py-1 text-[10px] font-bold uppercase tracking-wider rounded border transition-colors disabled:opacity-50 ${
                armed
                  ? "bg-red-900/20 border-red-900/50 text-red-400 hover:bg-red-900/30"
                  : "bg-[var(--bg-app)] hover:bg-[var(--bg-active)] border-[var(--border-main)] text-[var(--text-muted)]"
              }`}
            >
              {removing ? "…" : armed ? "Confirm" : "Remove"}
            </button>
          </Tooltip>
        )}
        <Tooltip
          content={
            provider.mirror.synced_at_ms === null
              ? "Fetch this catalogue for the first time"
              : "Replace this catalogue's records with what it publishes now"
          }
        >
          <button
            onClick={onSync}
            disabled={disabled}
            aria-label={`Sync ${provider.name}`}
            className="px-2.5 py-1 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors disabled:opacity-50"
          >
            {syncing ? "…" : "Sync"}
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
