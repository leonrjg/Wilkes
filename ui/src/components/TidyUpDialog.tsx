import { useCallback, useEffect, useMemo, useState } from "react";
import { source } from "../services";
import type { DesktopSourceApi } from "../services/api";
import type { TidyDestination, TidyPlan, TidySuggestion } from "../lib/types";
import { useTidyStore } from "../stores/useTidyStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import { fileName } from "./DocumentEntryRow";
import { useToasts } from "./Toast";

type Load =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "ready"; plan: TidyPlan };

/** What the user has decided for one document: whether to move it, and where. */
interface Choice {
  move: boolean;
  destination: string;
}

function parentName(path: string): string {
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts.length > 1 ? parts[parts.length - 2] : "";
}

/** A short name for each destination: its own name, or its parent's and its
 *  own where two destinations share a name. */
export function destinationLabels(destinations: TidyDestination[]): Map<string, string> {
  const counts = new Map<string, number>();
  for (const destination of destinations) {
    const name = fileName(destination.path);
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return new Map(
    destinations.map((destination) => {
      const name = fileName(destination.path);
      const parent = parentName(destination.path);
      return [
        destination.path,
        (counts.get(name) ?? 0) > 1 && parent ? `${parent}/${name}` : name,
      ];
    }),
  );
}

/** A close call is left for the user to decide; every other document starts
 *  out moving to its best destination. */
function initialChoices(plan: TidyPlan): Record<string, Choice> {
  return Object.fromEntries(
    plan.suggestions.map((suggestion) => [
      suggestion.entry.path,
      { move: !suggestion.close_call, destination: suggestion.ranking[0].destination },
    ]),
  );
}

/** Suggestions grouped by where they were suggested to go, largest group
 *  first. The groups stay as suggested when the user picks another folder
 *  for a row, so rows do not jump about under the pointer. */
function groupBySuggestion(
  suggestions: TidySuggestion[],
  labels: Map<string, string>,
): [string, TidySuggestion[]][] {
  const groups = new Map<string, TidySuggestion[]>();
  for (const suggestion of suggestions) {
    const destination = suggestion.ranking[0].destination;
    groups.set(destination, [...(groups.get(destination) ?? []), suggestion]);
  }
  return Array.from(groups.entries()).sort(
    ([left, a], [right, b]) =>
      b.length - a.length
      || (labels.get(left) ?? left).localeCompare(labels.get(right) ?? right),
  );
}

export default function TidyUpDialog() {
  const folder = useTidyStore((state) => state.folder);
  const close = useTidyStore((state) => state.close);
  const { addToast } = useToasts();
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [choices, setChoices] = useState<Record<string, Choice>>({});
  const [applying, setApplying] = useState(false);

  const fetchPlan = useCallback(async (target: string) => {
    setLoad({ status: "loading" });
    // A plan that arrives after the dialog moved on to another folder, or
    // closed, is for nothing on screen.
    const current = () => useTidyStore.getState().folder === target;
    try {
      const plan = await (source as DesktopSourceApi).tidyFolder(target);
      if (!current()) return;
      setChoices(initialChoices(plan));
      setLoad({ status: "ready", plan });
    } catch (error) {
      console.error("Failed to plan tidy-up:", error);
      if (!current()) return;
      setLoad({
        status: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }, []);

  useEffect(() => {
    if (folder) void fetchPlan(folder);
    else setLoad({ status: "loading" });
  }, [folder, fetchPlan]);

  const plan = load.status === "ready" ? load.plan : null;
  const labels = useMemo(
    () => destinationLabels(plan?.destinations ?? []),
    [plan],
  );
  const groups = useMemo(
    () => (plan ? groupBySuggestion(plan.suggestions, labels) : []),
    [plan, labels],
  );
  const moves = plan
    ? plan.suggestions.filter((suggestion) => choices[suggestion.entry.path]?.move)
    : [];

  if (!folder) return null;

  const label = (path: string) => labels.get(path) ?? fileName(path);
  const setChoice = (path: string, update: Partial<Choice>) =>
    setChoices((current) => ({ ...current, [path]: { ...current[path], ...update } }));
  const setGroup = (members: TidySuggestion[], move: boolean) =>
    setChoices((current) => {
      const next = { ...current };
      for (const member of members) {
        next[member.entry.path] = { ...next[member.entry.path], move };
      }
      return next;
    });

  const apply = async () => {
    if (!plan || moves.length === 0) return;
    setApplying(true);
    const failures: string[] = [];
    let moved = 0;
    for (const suggestion of moves) {
      const path = suggestion.entry.path;
      try {
        await (source as DesktopSourceApi).moveFile(path, choices[path].destination);
        useViewerStore.getState().closePath(path);
        moved += 1;
      } catch (error) {
        console.error(`Failed to move ${path}:`, error);
        failures.push(
          `${fileName(path)}: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
    }
    if (useSearchStore.getState().hasQuery) {
      await useSearchStore.getState().replaySearch();
    } else {
      await useSettingsStore.getState().refreshFileList();
    }
    setApplying(false);
    if (failures.length === 0) {
      addToast(`Moved ${moved} document${moved === 1 ? "" : "s"}`, { type: "success" });
      close();
      return;
    }
    addToast(
      `Moved ${moved}; ${failures.length} could not be moved. ${failures[0]}`,
      { type: "error" },
    );
    // What is left in the folder is what the dialog should now show.
    await fetchPlan(folder);
  };

  const partlyIndexed = plan
    ? plan.destinations.filter(
      (destination) =>
        destination.documents > 0 && destination.indexed_documents < destination.documents,
    )
    : [];
  const empty = plan ? plan.destinations.filter((destination) => destination.documents === 0) : [];

  return (
    <div className="fixed inset-0 z-[160] flex items-center justify-center bg-black/35 px-4">
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="tidy-up-title"
        onKeyDown={(event) => {
          if (event.key === "Escape" && !applying) close();
        }}
        className="flex max-h-[85vh] w-full max-w-2xl flex-col rounded-lg border border-[var(--border-main)] bg-[var(--bg-app)] shadow-2xl"
      >
        <div className="border-b border-[var(--border-main)] px-4 py-3">
          <div id="tidy-up-title" className="text-sm font-semibold text-[var(--text-main)]">
            Tidy up "{fileName(folder)}"
          </div>
          <div className="mt-0.5 text-xs text-[var(--text-muted)]">
            Each document is matched, by the semantic index, to the folder whose documents
            and name it is closest to.
            {plan && ` ${moves.length} of ${plan.suggestions.length} selected to move.`}
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2 text-xs">
          {load.status === "loading" && (
            <div className="py-6 text-center text-[var(--text-muted)]">
              Comparing documents with the other folders…
            </div>
          )}
          {load.status === "error" && (
            <div role="alert" className="py-4 text-[var(--text-error)]">
              {load.message}
            </div>
          )}
          {plan && groups.map(([destination, members]) => {
            const selected = members.filter((member) => choices[member.entry.path]?.move).length;
            return (
              <section key={destination} aria-label={`Suggested for ${label(destination)}`} className="mb-3">
                <label className="sticky top-0 z-10 flex items-center gap-2 border-b border-[var(--border-main)] bg-[var(--bg-app)] py-1 font-semibold text-[var(--text-main)]">
                  <input
                    type="checkbox"
                    checked={selected === members.length}
                    ref={(input) => {
                      if (input) input.indeterminate = selected > 0 && selected < members.length;
                    }}
                    onChange={(event) => setGroup(members, event.target.checked)}
                    disabled={applying}
                    aria-label={`Move all suggested for ${label(destination)}`}
                  />
                  <span className="truncate" title={destination}>→ {label(destination)}</span>
                  <span className="ml-auto font-normal tabular-nums text-[var(--text-dim)]">
                    {members.length}
                  </span>
                </label>
                <ul>
                  {members.map((suggestion) => {
                    const path = suggestion.entry.path;
                    const choice = choices[path];
                    const runnerUp = suggestion.ranking[1];
                    return (
                      <li key={path} className="flex items-center gap-2 py-1">
                        <input
                          type="checkbox"
                          checked={choice?.move ?? false}
                          onChange={(event) => setChoice(path, { move: event.target.checked })}
                          disabled={applying}
                          aria-label={`Move ${fileName(path)}`}
                        />
                        <div className="min-w-0 flex-1" title={path}>
                          <div className="truncate text-[var(--text-main)]">{fileName(path)}</div>
                          {suggestion.entry.title && (
                            <div className="truncate text-[10px] text-[var(--text-dim)]">
                              {suggestion.entry.title}
                            </div>
                          )}
                        </div>
                        {suggestion.close_call && runnerUp && (
                          <span
                            title={`Fits ${label(runnerUp.destination)} nearly as well`}
                            className="shrink-0 rounded bg-[var(--bg-active)] px-1.5 py-0.5 text-[10px] text-[var(--text-muted)]"
                          >
                            Close call
                          </span>
                        )}
                        <select
                          aria-label={`Destination for ${fileName(path)}`}
                          value={choice?.destination}
                          disabled={applying}
                          onChange={(event) =>
                            setChoice(path, { destination: event.target.value, move: true })}
                          className="w-44 shrink-0 truncate rounded border border-[var(--border-main)] bg-[var(--bg-active)] px-1 py-0.5 text-[11px] text-[var(--text-main)]"
                        >
                          {suggestion.ranking.map((placement) => (
                            <option key={placement.destination} value={placement.destination}>
                              {label(placement.destination)}
                            </option>
                          ))}
                        </select>
                      </li>
                    );
                  })}
                </ul>
              </section>
            );
          })}
          {plan && (plan.unindexed.length > 0 || partlyIndexed.length > 0 || empty.length > 0) && (
            <div className="mt-2 space-y-1 border-t border-[var(--border-main)] pt-2 text-[11px] text-[var(--text-muted)]">
              {plan.unindexed.length > 0 && (
                <details>
                  <summary>
                    {plan.unindexed.length} document{plan.unindexed.length === 1 ? " is" : "s are"} not
                    in the semantic index, so {plan.unindexed.length === 1 ? "it stays" : "they stay"} here.
                  </summary>
                  <ul className="ml-4 list-disc">
                    {plan.unindexed.map((entry) => (
                      <li key={entry.path} className="truncate" title={entry.path}>
                        {fileName(entry.path)}
                      </li>
                    ))}
                  </ul>
                </details>
              )}
              {partlyIndexed.length > 0 && (
                <div>
                  Judged partly by name, as not all their documents are indexed:{" "}
                  {partlyIndexed
                    .map((d) => `${label(d.path)} (${d.indexed_documents} of ${d.documents})`)
                    .join(", ")}
                  .
                </div>
              )}
              {empty.length > 0 && (
                <div>
                  Judged by name, as they are empty: {empty.map((d) => label(d.path)).join(", ")}.
                </div>
              )}
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 border-t border-[var(--border-main)] px-4 py-3">
          <button
            type="button"
            onClick={close}
            disabled={applying}
            className="rounded border border-[var(--border-main)] px-3 py-1.5 text-xs text-[var(--text-muted)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void apply()}
            disabled={!plan || moves.length === 0 || applying}
            className="rounded bg-[var(--accent-blue)] px-3 py-1.5 text-xs font-medium text-white hover:opacity-90 disabled:opacity-50"
          >
            {applying
              ? "Moving…"
              : `Move ${moves.length} document${moves.length === 1 ? "" : "s"}`}
          </button>
        </div>
      </div>
    </div>
  );
}
