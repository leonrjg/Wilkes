import { afterEach, describe, expect, it, vi } from "vitest";
import { installReadableStreamAsyncIterator } from "./readableStreamAsyncIterator";

const ASYNC_ITERATOR = Symbol.asyncIterator;
const original = Object.getOwnPropertyDescriptor(
  ReadableStream.prototype,
  ASYNC_ITERATOR,
);

/** Reproduce a WebKit-shaped engine: no async iteration on ReadableStream. */
function removeNativeAsyncIteration() {
  Reflect.deleteProperty(ReadableStream.prototype, ASYNC_ITERATOR);
  Reflect.deleteProperty(ReadableStream.prototype, "values");
  vi.spyOn(console, "info").mockImplementation(() => {});
}

function streamOf<T>(...chunks: T[]) {
  return new ReadableStream<T>({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
}

afterEach(() => {
  Reflect.deleteProperty(ReadableStream.prototype, ASYNC_ITERATOR);
  Reflect.deleteProperty(ReadableStream.prototype, "values");
  if (original) {
    Object.defineProperty(ReadableStream.prototype, ASYNC_ITERATOR, original);
  }
  vi.restoreAllMocks();
});

describe("installReadableStreamAsyncIterator", () => {
  it("leaves an engine that already has async iteration untouched", () => {
    const before = ReadableStream.prototype[ASYNC_ITERATOR];
    expect(installReadableStreamAsyncIterator()).toBe(false);
    expect(ReadableStream.prototype[ASYNC_ITERATOR]).toBe(before);
  });

  it("drains a stream in order once installed", async () => {
    removeNativeAsyncIteration();
    expect(installReadableStreamAsyncIterator()).toBe(true);

    const seen: number[] = [];
    for await (const chunk of streamOf(1, 2, 3)) seen.push(chunk);
    expect(seen).toEqual([1, 2, 3]);
  });

  it("reports that it filled a gap rather than installing silently", () => {
    removeNativeAsyncIteration();
    installReadableStreamAsyncIterator();
    expect(console.info).toHaveBeenCalledOnce();
  });

  it("is idempotent, so a second importer does not replace the first install", () => {
    removeNativeAsyncIteration();
    expect(installReadableStreamAsyncIterator()).toBe(true);
    const installed = ReadableStream.prototype[ASYNC_ITERATOR];
    expect(installReadableStreamAsyncIterator()).toBe(false);
    expect(ReadableStream.prototype[ASYNC_ITERATOR]).toBe(installed);
  });

  it("releases the reader when iteration ends, so the stream can be read again", async () => {
    removeNativeAsyncIteration();
    installReadableStreamAsyncIterator();

    const stream = streamOf("a");
    for await (const chunk of stream) void chunk;
    // A retained lock would make this throw.
    expect(() => stream.getReader()).not.toThrow();
  });

  it("cancels the stream when iteration breaks early", async () => {
    removeNativeAsyncIteration();
    installReadableStreamAsyncIterator();

    const cancel = vi.fn();
    const stream = new ReadableStream<number>({
      start(controller) {
        controller.enqueue(1);
        controller.enqueue(2);
      },
      cancel,
    });
    for await (const chunk of stream) {
      expect(chunk).toBe(1);
      break;
    }
    expect(cancel).toHaveBeenCalledOnce();
  });

  it("propagates a stream error instead of swallowing it", async () => {
    removeNativeAsyncIteration();
    installReadableStreamAsyncIterator();

    const boom = new Error("boom");
    const stream = new ReadableStream({
      start(controller) {
        controller.error(boom);
      },
    });
    await expect(async () => {
      for await (const chunk of stream) void chunk;
    }).rejects.toThrow(boom);
  });
});
