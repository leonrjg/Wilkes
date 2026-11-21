import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { Lock, Loader } from "react-feather";
import { isTauri } from "../services";
import { checkApiSession, loginToApi } from "../services/http";

interface Props {
  children: ReactNode;
}

type Gate = { kind: "checking" } | { kind: "open" } | { kind: "locked" } | { kind: "failed"; error: string };

/**
 * Asks for the server's token before the web interface starts, when the server
 * requires one. A `wilkes-server` does when `WILKES_API_TOKEN` is set; by
 * default it does not, and the desktop app has no web interface to guard.
 */
export default function ApiLoginGate({ children }: Props) {
  const [gate, setGate] = useState<Gate>(isTauri ? { kind: "open" } : { kind: "checking" });
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (isTauri) return;
    let mounted = true;
    checkApiSession()
      .then((open) => {
        if (mounted) setGate(open ? { kind: "open" } : { kind: "locked" });
      })
      .catch((reason) => {
        if (mounted) setGate({ kind: "failed", error: reason instanceof Error ? reason.message : String(reason) });
      });
    return () => {
      mounted = false;
    };
  }, []);

  if (gate.kind === "open") return <>{children}</>;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await loginToApi(token.trim());
      setGate({ kind: "open" });
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="flex h-full items-center justify-center overflow-hidden bg-[var(--bg-app)] p-6 text-[var(--text-main)]">
      <section className="w-full max-w-sm rounded-xl border border-[var(--border-main)] bg-[var(--bg-app)] shadow-2xl">
        <div className="flex items-center gap-2 border-b border-[var(--border-main)] px-4 py-2.5">
          <span className="text-[var(--text-dim)]">
            {gate.kind === "checking"
              ? <Loader size={16} className="animate-spin" aria-hidden="true" />
              : <Lock size={16} aria-hidden="true" />}
          </span>
          <h1 className="text-base font-semibold">
            {gate.kind === "checking" ? "Connecting…" : "This Wilkes requires its token"}
          </h1>
        </div>
        {gate.kind === "failed" && (
          <p role="alert" className="m-4 rounded border border-red-900/50 bg-red-900/20 p-2 text-xs text-red-400 break-all">
            {gate.error}
          </p>
        )}
        {gate.kind === "locked" && (
          <form onSubmit={(event) => void submit(event)} className="space-y-3 p-4">
            <p className="text-xs text-[var(--text-muted)]">
              Enter the token set as WILKES_API_TOKEN on the server.
            </p>
            <input
              aria-label="API token"
              type="password"
              autoComplete="current-password"
              autoFocus
              value={token}
              disabled={busy}
              onChange={(event) => setToken(event.target.value)}
              className="w-full rounded border border-[var(--border-main)] bg-[var(--bg-input)] px-2.5 py-1.5 text-xs font-mono focus:border-[var(--accent-blue)] focus:outline-none"
            />
            {error && (
              <p role="alert" className="rounded border border-red-900/50 bg-red-900/20 p-2 text-xs text-red-400 break-all">
                {error}
              </p>
            )}
            <button
              type="submit"
              disabled={busy || token.trim().length === 0}
              className="w-full rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-3 py-1.5 text-xs hover:bg-[var(--bg-hover)] disabled:opacity-50"
            >
              Continue
            </button>
          </form>
        )}
      </section>
    </main>
  );
}
