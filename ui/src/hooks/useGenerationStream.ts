import { useCallback, useEffect, useState } from "react";
import type { GenerationTask } from "../lib/types";
import { api } from "../services";

export type GenerationStreamPhase =
  | { kind: "absent" }
  | { kind: "queued" }
  | { kind: "streaming"; text: string }
  | { kind: "done"; text: string }
  | { kind: "failed"; error: string };

interface GenerationStreamOptions {
  enabled: boolean;
  requestKey: string | null;
  task: GenerationTask;
  start: (requestId: string) => Promise<void>;
}

let requestSequence = 0;

function nextRequestId(task: GenerationTask): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return `${task}-${uuid}`;
  requestSequence += 1;
  return `${task}-${Date.now()}-${requestSequence}`;
}

function messageFor(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Owns the complete lifecycle shared by every user-facing generation stream:
 * subscribe before starting, correlate by opaque request id and task, assemble
 * deltas, accept one terminal event, and reject stale events after cleanup.
 */
export function useGenerationStream({
  enabled,
  requestKey,
  task,
  start,
}: GenerationStreamOptions): {
  phase: GenerationStreamPhase;
  retry: () => void;
} {
  const [stream, setStream] = useState<{
    requestKey: string | null;
    phase: GenerationStreamPhase;
  }>({ requestKey: null, phase: { kind: "absent" } });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!enabled || requestKey == null) {
      setStream({ requestKey: null, phase: { kind: "absent" } });
      return;
    }

    const requestId = nextRequestId(task);
    let cancelled = false;
    let terminal = false;
    let unlisten: (() => void) | undefined;
    setStream({ requestKey, phase: { kind: "queued" } });

    api.onGenerationStream((event) => {
      if (
        cancelled ||
        terminal ||
        event.request_id !== requestId ||
        event.task !== task
      ) {
        return;
      }

      if (event.phase === "delta") {
        setStream((current) => ({
          requestKey,
          phase: {
            kind: "streaming",
            text:
              (current.requestKey === requestKey &&
              current.phase.kind === "streaming"
                ? current.phase.text
                : "") + event.delta,
          },
        }));
      } else if (event.phase === "completed") {
        terminal = true;
        setStream({ requestKey, phase: { kind: "done", text: event.text } });
      } else {
        terminal = true;
        setStream({ requestKey, phase: { kind: "failed", error: event.error } });
      }
    })
      .then((stopListening) => {
        if (cancelled) {
          stopListening();
          return;
        }
        unlisten = stopListening;
        return start(requestId);
      })
      .catch((error) => {
        if (cancelled || terminal) return;
        terminal = true;
        setStream({
          requestKey,
          phase: { kind: "failed", error: messageFor(error) },
        });
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [attempt, enabled, requestKey, start, task]);

  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  const phase =
    !enabled || requestKey == null
      ? ({ kind: "absent" } as const)
      : stream.requestKey === requestKey
        ? stream.phase
        : ({ kind: "queued" } as const);
  return { phase, retry };
}
