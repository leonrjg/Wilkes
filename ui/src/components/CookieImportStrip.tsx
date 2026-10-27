// Bringing the page's site's cookies over from the user's own browser, so a
// sign-in there carries into the browser pane. A strip under the pane's
// header rather than a popover: anything drawn over the page would be hidden
// under its native view.
import { useEffect, useState } from "react";
import { X } from "react-feather";
import { api } from "../services";
import type { CookieSourceInfo } from "../services/api";

/** The site a page belongs to, as the domain field starts out: its host
 *  without `www.`. The user can widen it, say to a parent domain. */
export function siteOf(url: string | null): string {
  if (!url) return "";
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

type Outcome = { kind: "imported"; count: number; site: string; source: string } | { kind: "error"; message: string };

export default function CookieImportStrip({
  url,
  onImported,
  onClose,
}: {
  url: string | null;
  onImported: () => void;
  onClose: () => void;
}) {
  const [sources, setSources] = useState<CookieSourceInfo[]>([]);
  const [source, setSource] = useState("");
  const [site, setSite] = useState(() => siteOf(url));
  const [importing, setImporting] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  useEffect(() => {
    let disposed = false;
    api
      .browserCookieSources?.()
      .then((list) => {
        if (disposed) return;
        setSources(list);
        setSource((current) => current || list[0]?.id || "");
      })
      .catch((e) => {
        console.error("browser pane: could not list browsers to import from", e);
        if (!disposed) setOutcome({ kind: "error", message: String(e) });
      });
    return () => {
      disposed = true;
    };
  }, []);

  const importCookies = async () => {
    const name = sources.find((s) => s.id === source)?.name ?? source;
    setImporting(true);
    setOutcome(null);
    try {
      const count = (await api.browserPaneImportCookies?.(source, site)) ?? 0;
      setOutcome({ kind: "imported", count, site, source: name });
      if (count > 0) onImported();
    } catch (e) {
      console.error("browser pane: cookie import failed", e);
      setOutcome({ kind: "error", message: String(e) });
    } finally {
      setImporting(false);
    }
  };

  const field =
    "rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1.5 py-1 text-[11px] text-[var(--text-main)] outline-none focus:border-[var(--accent-blue)]";

  return (
    <div className="border-b border-[var(--border-main)] px-2 py-1.5 text-[11px] text-[var(--text-muted)]">
      <form
        className="flex flex-wrap items-center gap-1.5"
        onSubmit={(event) => {
          event.preventDefault();
          void importCookies();
        }}
      >
        <span>Sign in with cookies from</span>
        <select
          value={source}
          onChange={(e) => setSource(e.target.value)}
          aria-label="Browser to import cookies from"
          className={field}
        >
          {sources.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </select>
        <span>for</span>
        <input
          value={site}
          onChange={(e) => setSite(e.target.value)}
          aria-label="Site to import cookies for"
          placeholder="example.org"
          spellCheck={false}
          className={`${field} min-w-0 flex-1`}
        />
        <button
          type="submit"
          disabled={importing || !source || site.trim() === ""}
          className="rounded bg-[var(--accent-blue)] px-2 py-1 text-[11px] text-white disabled:opacity-50"
        >
          {importing ? "Importing…" : "Import"}
        </button>
        <button
          type="button"
          aria-label="Close cookie import"
          onClick={onClose}
          className="inline-flex shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
        >
          <X size={12} />
        </button>
      </form>
      {outcome?.kind === "imported" && (
        <p role="status" className="mt-1 text-[var(--text-dim)]">
          {outcome.count === 0
            ? `${outcome.source} has no cookies for ${outcome.site}.`
            : `Imported ${outcome.count} cookie${outcome.count === 1 ? "" : "s"} for ${outcome.site} from ${outcome.source}.`}
        </p>
      )}
      {outcome?.kind === "error" && (
        <p role="alert" className="mt-1 break-words text-[var(--accent-red)]">
          {outcome.message}
        </p>
      )}
    </div>
  );
}
