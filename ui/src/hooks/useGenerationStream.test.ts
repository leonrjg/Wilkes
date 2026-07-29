import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GenerationStreamEvent } from "../lib/types";
import { api } from "../services";
import { useGenerationStream } from "./useGenerationStream";

vi.mock("../services", () => ({
  api: {
    onGenerationStream: vi.fn(),
  },
}));

describe("useGenerationStream", () => {
  let handler: (event: GenerationStreamEvent) => void;
  const unlisten = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    (api.onGenerationStream as any).mockImplementation(
      (next: (event: GenerationStreamEvent) => void) => {
        handler = next;
        return Promise.resolve(unlisten);
      },
    );
  });

  it("subscribes before starting and assembles only its correlated stream", async () => {
    const start = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useGenerationStream({
        enabled: true,
        requestKey: "/docs/paper.pdf",
        task: "document_summary",
        start,
      }),
    );

    expect(api.onGenerationStream).toHaveBeenCalledOnce();
    expect(start).not.toHaveBeenCalled();
    await waitFor(() => expect(start).toHaveBeenCalledOnce());
    const requestId = start.mock.calls[0][0];

    act(() => {
      handler({
        phase: "delta",
        request_id: "stale-request",
        task: "document_summary",
        delta: "wrong id",
      });
      handler({
        phase: "delta",
        request_id: requestId,
        task: "relation_explanation",
        delta: "wrong task",
      });
      handler({
        phase: "delta",
        request_id: requestId,
        task: "document_summary",
        delta: "First ",
      });
      handler({
        phase: "delta",
        request_id: requestId,
        task: "document_summary",
        delta: "second",
      });
    });
    expect(result.current.phase).toEqual({
      kind: "streaming",
      text: "First second",
    });

    act(() => {
      handler({
        phase: "completed",
        request_id: requestId,
        task: "document_summary",
        text: "Authoritative final.",
      });
      handler({
        phase: "delta",
        request_id: requestId,
        task: "document_summary",
        delta: " ignored",
      });
    });
    expect(result.current.phase).toEqual({
      kind: "done",
      text: "Authoritative final.",
    });
  });

  it("surfaces terminal events and invocation failures through one failed phase", async () => {
    const start = vi.fn().mockResolvedValue(undefined);
    const { result, unmount } = renderHook(() =>
      useGenerationStream({
        enabled: true,
        requestKey: "relation",
        task: "relation_explanation",
        start,
      }),
    );
    await waitFor(() => expect(start).toHaveBeenCalledOnce());
    const requestId = start.mock.calls[0][0];

    act(() => {
      handler({
        phase: "failed",
        request_id: requestId,
        task: "relation_explanation",
        error: "model stopped",
      });
    });
    expect(result.current.phase).toEqual({
      kind: "failed",
      error: "model stopped",
    });
    unmount();
    expect(unlisten).toHaveBeenCalledOnce();

    const failedStart = vi.fn().mockRejectedValue(new Error("transport closed"));
    const second = renderHook(() =>
      useGenerationStream({
        enabled: true,
        requestKey: "summary",
        task: "document_summary",
        start: failedStart,
      }),
    );
    await waitFor(() =>
      expect(second.result.current.phase).toEqual({
        kind: "failed",
        error: "transport closed",
      }),
    );
  });

  it("rejects events from a request after its key changes", async () => {
    const start = vi.fn().mockResolvedValue(undefined);
    const { result, rerender } = renderHook(
      ({ requestKey }) =>
        useGenerationStream({
          enabled: true,
          requestKey,
          task: "document_summary",
          start,
        }),
      { initialProps: { requestKey: "first" } },
    );
    await waitFor(() => expect(start).toHaveBeenCalledOnce());
    const firstRequestId = start.mock.calls[0][0];

    rerender({ requestKey: "second" });
    expect(result.current.phase).toEqual({ kind: "queued" });
    await waitFor(() => expect(start).toHaveBeenCalledTimes(2));

    act(() => {
      handler({
        phase: "completed",
        request_id: firstRequestId,
        task: "document_summary",
        text: "stale",
      });
    });
    expect(result.current.phase).not.toEqual({ kind: "done", text: "stale" });
  });
});
