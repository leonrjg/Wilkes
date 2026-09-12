import { useEffect, useState, type ReactNode } from "react";
import { AlertTriangle, Check, Copy, Loader } from "react-feather";
import type { StartupStatus } from "../lib/types";
import { api } from "../services";
import { CopyButton } from "./CopyButton";

interface Props {
  children: ReactNode;
}

export default function StartupGate({ children }: Props) {
  const [status, setStatus] = useState<StartupStatus | null>(null);

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

  return (
    <main className="flex h-full items-center justify-center overflow-hidden bg-[var(--bg-app)] p-6 text-[var(--text-main)]">
      <section className="flex max-h-full w-full max-w-xl flex-col overflow-hidden rounded-xl border border-[var(--border-main)] bg-[var(--bg-app)] shadow-2xl">
        <div className="flex shrink-0 items-center gap-2 border-b border-[var(--border-main)] px-4 py-2.5">
          <span className={status ? "text-[var(--accent-amber)]" : "text-[var(--text-dim)]"}>
            {status
              ? <AlertTriangle size={16} aria-hidden="true" />
              : <Loader size={16} className="animate-spin" aria-hidden="true" />}
          </span>
          <h1 className="text-base font-semibold text-[var(--text-main)]">
            {status ? "Action required before Wilkes can start" : "Checking this installation…"}
          </h1>
        </div>

        <div className="flex-1 space-y-3 overflow-y-auto p-4">
          <p className="text-[11px] text-[var(--text-dim)] italic">
            {status
              ? "Wilkes has paused normal startup. Complete the steps below, then restart Wilkes."
              : "Verifying that all features are ready to start."}
          </p>

          {status?.blockers.map((blocker) => (
            <article
              key={blocker.id}
              className="rounded-lg border border-[var(--border-main)] bg-[var(--bg-sidebar)] p-3"
            >
              <h2 className="text-[10px] font-bold uppercase tracking-wider text-[var(--accent-blue)]">
                {blocker.feature}
              </h2>
              <p className="mt-1.5 text-xs font-semibold text-[var(--text-main)]">{blocker.title}</p>
              <p className="mt-1 text-[11px] leading-relaxed text-[var(--text-muted)]">
                {blocker.message}
              </p>

              {blocker.actions.length > 0 && (
                <div className="mt-3 space-y-2.5">
                  {blocker.actions.map((action, index) => (
                    <div key={`${blocker.id}-${index}`} className="space-y-1">
                      <div className="flex items-baseline justify-between gap-3">
                        <span className="text-xs text-[var(--text-main)]">{action.label}</span>
                        <span className="text-right text-[10px] text-[var(--text-dim)] italic">
                          {action.description}
                        </span>
                      </div>
                      {action.command && (
                        <div className="flex items-center gap-1.5 rounded border border-[var(--border-main)] bg-[var(--bg-input)] py-1 pl-2.5 pr-1">
                          <code className="min-w-0 flex-1 select-all overflow-x-auto whitespace-nowrap font-mono text-[10px] text-[var(--text-main)]">
                            {action.command}
                          </code>
                          <CopyButton
                            copy={() => api.writeClipboard(action.command!)}
                            aria-label="Copy"
                            copiedChildren={<><Check size={11} /> Copied</>}
                            className="flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wider text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--accent-blue)]"
                          >
                            <Copy size={11} /> Copy
                          </CopyButton>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </article>
          ))}
        </div>
      </section>
    </main>
  );
}
