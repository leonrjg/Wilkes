import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useDocumentFind } from "./useDocumentFind";

describe("useDocumentFind", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
  });

  it("starts closed with an empty query and no active match", () => {
    const { result } = renderHook(() => useDocumentFind(0));
    expect(result.current.isOpen).toBe(false);
    expect(result.current.query).toBe("");
    expect(result.current.currentIdx).toBe(-1);
  });

  it("opens on Cmd/Ctrl+F and closes on Escape", () => {
    const { result } = renderHook(() => useDocumentFind(0));

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true }));
    });
    expect(result.current.isOpen).toBe(true);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(result.current.isOpen).toBe(false);
  });

  it("activates the first match once matches appear", () => {
    const { result, rerender } = renderHook(({ count }) => useDocumentFind(count), {
      initialProps: { count: 0 },
    });
    expect(result.current.currentIdx).toBe(-1);

    rerender({ count: 3 });
    expect(result.current.currentIdx).toBe(0);
  });

  it("wraps forward and backward through matches", () => {
    const { result } = renderHook(() => useDocumentFind(3));
    expect(result.current.currentIdx).toBe(0);

    act(() => result.current.next());
    expect(result.current.currentIdx).toBe(1);

    act(() => result.current.next());
    act(() => result.current.next());
    expect(result.current.currentIdx).toBe(0); // wrapped past the end

    act(() => result.current.prev());
    expect(result.current.currentIdx).toBe(2); // wrapped before the start
  });

  it("steps on Enter and reverses on Shift+Enter", () => {
    const { result } = renderHook(() => useDocumentFind(3));
    const preventDefault = vi.fn();

    act(() => {
      result.current.onInputKeyDown({ key: "Enter", shiftKey: false, preventDefault } as any);
    });
    expect(preventDefault).toHaveBeenCalled();
    expect(result.current.currentIdx).toBe(1);

    act(() => {
      result.current.onInputKeyDown({ key: "Enter", shiftKey: true, preventDefault: vi.fn() } as any);
    });
    expect(result.current.currentIdx).toBe(0);
  });

  it("resets to no active match when matches disappear", () => {
    const { result, rerender } = renderHook(({ count }) => useDocumentFind(count), {
      initialProps: { count: 2 },
    });
    expect(result.current.currentIdx).toBe(0);

    rerender({ count: 0 });
    expect(result.current.currentIdx).toBe(-1);
  });
});
