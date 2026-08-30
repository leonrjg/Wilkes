import { useCallback, useEffect, useState } from "react";
import type { SearchApi } from "../services/api";
import type {
  CatalogueFetchProgress,
  CatalogueProviderStatus,
  CatalogueStatus,
  CatalogueSyncOutcome,
} from "../lib/types";
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
  isActive: boolean;
}

/** The provider ids are wire values; these are how the four are written down. */
const PROVIDER_LABELS: Record<string, string> = {
  libretexts: "LibreTexts",
  openstax: "OpenStax",
  mit_ocw: "MIT OpenCourseWare",
  devdocs: "DevDocs",
};

const GRAIN_LABELS: Record<string, string> = {
  textbook: "Textbooks",
  course: "Courses",
  reference: "Reference",
};

/** A provider the mirror still holds rows for but this build no longer knows
 *  is shown under its raw id rather than hidden: the rows are real. */
function providerLabel(provider: string): string {
  return PROVIDER_LABELS[provider] ?? provider;
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

export default function CataloguePanel({ api, isActive }: Props) {
  const [status, setStatus] = useState<CatalogueStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState<string | null>(null);
  const [queued, setQueued] = useState<string[]>([]);
  const [reports, setReports] = useState<Record<string, ProviderReport>>({});
  /** The last page reported by each provider still fetching. A whole-catalogue
   *  fetch is minutes long; without this the row says "Fetching…" and then
   *  jumps to a final count, which is indistinguishable from being stuck. */
  const [fetching, setFetching] = useState<Record<string, CatalogueFetchProgress>>({});

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.catalogueStatus());
    } catch (e: any) {
      setError(e?.toString?.() ?? "Failed to read the catalogue mirror");
    }
  }, [api]);

  useEffect(() => {
    if (!isActive) return;
    setError(null);
    refresh();
  }, [isActive, refresh]);

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
        setStatus((prev) =>
          prev === null ? prev : { ...prev, total_records: response.total_records },
        );
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
    await refresh();
  };

  const busy = syncing !== null;

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

  const empty = status.total_records === 0;

  return (
    <div className="flex flex-col gap-6">
      <section>
        <div className="flex flex-col gap-1.5 mb-4">
          <h3 className="text-[10px] font-bold text-[var(--text-dim)] uppercase tracking-wider">
            Teaching catalogues
          </h3>
          <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
            A local copy of what several open textbook, course and documentation
            catalogues publish, so that a search which your library cannot answer
            can suggest something that would. Shared by every workspace on this
            machine, and refreshed only when you ask.
          </p>
        </div>

        <div className="p-3 bg-[var(--bg-active)] rounded-lg border border-[var(--border-main)] flex flex-col gap-3">
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-1">
              <span className="text-[10px] text-[var(--text-dim)] uppercase font-bold tracking-tighter">
                Records held
              </span>
              <span className="text-[10px] text-[var(--text-main)] font-mono">
                {status.total_records.toLocaleString()}
              </span>
            </div>
            <button
              onClick={() => syncProviders(status.providers.map((p) => p.provider))}
              disabled={busy}
              className="px-3 py-1.5 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors disabled:opacity-50"
            >
              {busy ? "Syncing…" : "Sync all"}
            </button>
          </div>

          {empty && !busy && (
            <p className="text-[11px] text-[var(--text-muted)] leading-relaxed">
              This mirror is empty. Syncing fetches each catalogue in full, which
              takes a few minutes and a few megabytes.
            </p>
          )}

          <div className="flex flex-col divide-y divide-[var(--border-main)]">
            {status.providers.map((provider) => (
              <ProviderRow
                key={provider.provider}
                provider={provider}
                report={reports[provider.provider] ?? null}
                progress={fetching[provider.provider] ?? null}
                syncing={syncing === provider.provider}
                queued={queued.includes(provider.provider)}
                disabled={busy}
                onSync={() => syncProviders([provider.provider])}
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
}: {
  provider: CatalogueProviderStatus;
  report: ProviderReport | null;
  progress: CatalogueFetchProgress | null;
  syncing: boolean;
  queued: boolean;
  disabled: boolean;
  onSync: () => void;
}) {
  const failed =
    report !== null &&
    (report.kind === "error" || report.outcome.error !== null);
  return (
    <div className="flex items-center justify-between gap-3 py-2 first:pt-0 last:pb-0">
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex items-center gap-2">
          <span className="text-[11px] text-[var(--text-main)] font-medium truncate">
            {providerLabel(provider.provider)}
          </span>
          <span className="px-1.5 py-0.5 rounded bg-[var(--bg-app)] border border-[var(--border-main)] text-[9px] uppercase tracking-wider text-[var(--text-dim)]">
            {GRAIN_LABELS[provider.grain] ?? provider.grain}
          </span>
        </div>
        <span
          className={`text-[10px] leading-relaxed ${
            failed ? "text-red-400" : "text-[var(--text-muted)]"
          }`}
        >
          {syncing
            ? progress === null
              ? "Fetching…"
              : `Fetching… page ${progress.pages.toLocaleString()}, ${progress.records.toLocaleString()} records`
            : queued
              ? "Waiting"
              : report !== null
                ? outcomeSummary(report)
                : provider.synced_at_ms === null
                  ? "Never synced"
                  : `${provider.records.toLocaleString()} records · ${formatSyncedAt(provider.synced_at_ms)}`}
        </span>
      </div>
      <Tooltip
        content={
          provider.synced_at_ms === null
            ? "Fetch this catalogue for the first time"
            : "Replace this catalogue's records with what it publishes now"
        }
      >
        <button
          onClick={onSync}
          disabled={disabled}
          aria-label={`Sync ${providerLabel(provider.provider)}`}
          className="px-2.5 py-1 shrink-0 bg-[var(--bg-app)] hover:bg-[var(--bg-active)] text-[var(--text-main)] text-[10px] font-bold uppercase tracking-wider rounded border border-[var(--border-main)] transition-colors disabled:opacity-50"
        >
          {syncing ? "…" : "Sync"}
        </button>
      </Tooltip>
    </div>
  );
}
