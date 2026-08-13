import { useEffect, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Copy } from "react-feather";
import type { StartupStatus } from "../lib/types";
import { api } from "../services";

interface Props {
  children: ReactNode;
}

export default function StartupGate({ children }: Props) {
  const [status, setStatus] = useState<StartupStatus | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  useEffect(() => {
    let mounted = true;
    api.getStartupStatus()
      .then((next) => {
        if (mounted) setStatus(next);
      })
      .catch((error) => {
        if (!mounted) return;
        setStatus({
          blockers: [{
            id: "application.startup-status-unavailable",
            feature: "Application startup",
            title: "Wilkes could not check its startup status",
            message: error instanceof Error ? error.message : String(error),
            actions: [],
          }],
        });
      });
    return () => {
      mounted = false;
    };
  }, []);

  if (status?.blockers.length === 0) return <>{children}</>;

  const copyCommand = async (id: string, command: string) => {
    await api.writeClipboard(command);
    setCopied(id);
    window.setTimeout(() => setCopied((current) => current === id ? null : current), 1600);
  };

  return (
    <main className="flex min-h-screen items-center justify-center overflow-auto bg-[var(--bg-app)] px-6 py-12 text-[var(--text-main)]">
      <section className="w-full max-w-2xl rounded-xl border border-[var(--border-main)] bg-[var(--bg-sidebar)] p-8 shadow-2xl">
        <div className="mb-7 flex items-start gap-4">
          <div className="mt-0.5 rounded-lg bg-amber-500/15 p-3 text-amber-400">
            <AlertTriangle size={24} aria-hidden="true" />
          </div>
          <div>
            <p className="mb-1 text-xs font-semibold uppercase tracking-[0.18em] text-[var(--text-muted)]">
              Wilkes startup
            </p>
            <h1 className="text-2xl font-semibold">
              {status ? "Action required before Wilkes can start" : "Checking this installation…"}
            </h1>
            <p className="mt-2 text-sm leading-6 text-[var(--text-muted)]">
              {status
                ? "Wilkes has paused normal startup. Complete the steps below, then restart Wilkes."
                : "Verifying that all features are ready to start."}
            </p>
          </div>
        </div>

        {status?.blockers.map((blocker) => (
          <article
            key={blocker.id}
            className="mb-4 rounded-lg border border-[var(--border-main)] bg-[var(--bg-active)] p-5 last:mb-0"
          >
            <p className="text-xs font-medium uppercase tracking-wider text-[var(--accent-blue)]">
              {blocker.feature}
            </p>
            <h2 className="mt-1 text-base font-semibold">{blocker.title}</h2>
            <p className="mt-2 text-sm leading-6 text-[var(--text-muted)]">{blocker.message}</p>

            {blocker.actions.length > 0 && (
              <div className="mt-5 space-y-3">
                {blocker.actions.map((action, index) => {
                  const actionId = `${blocker.id}-${index}`;
                  return (
                    <div key={actionId}>
                      <div className="mb-1.5 flex items-baseline justify-between gap-3">
                        <span className="text-sm font-medium">{action.label}</span>
                        <span className="text-right text-xs text-[var(--text-dim)]">
                          {action.description}
                        </span>
                      </div>
                      {action.command && (
                        <div className="flex items-center gap-2 rounded-md border border-[var(--border-main)] bg-[var(--bg-app)] p-2 pl-3">
                          <code className="min-w-0 flex-1 select-all overflow-x-auto whitespace-nowrap text-xs text-[var(--text-main)]">
                            {action.command}
                          </code>
                          <button
                            type="button"
                            onClick={() => void copyCommand(actionId, action.command!)}
                            className="flex shrink-0 items-center gap-1.5 rounded px-2 py-1 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-main)]"
                          >
                            {copied === actionId ? <Check size={12} /> : <Copy size={12} />}
                            {copied === actionId ? "Copied" : "Copy"}
                          </button>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </article>
        ))}
      </section>
    </main>
  );
}
