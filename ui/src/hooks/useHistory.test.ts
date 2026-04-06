import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useHistory } from "./useHistory";
import { useSearchStore } from "../stores/useSearchStore";

describe("useHistory", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useSearchStore.setState({
      selectMatch: vi.fn(),
    });
  });

  it("should initialize with canGoBack and canGoForward as false", () => {
    const { result } = renderHook(() => useHistory());
    expect(result.current.canGoBack).toBe(false);
    expect(result.current.canGoForward).toBe(false);
  });

  it("should add to history when handleMatchClick is called", () => {
    const { result } = renderHook(() => useHistory());
    const mockMatchRef = { path: "/test.txt", origin: { TextFile: { line: 1, col: 1 } } } as any;

    act(() => {
      result.current.handleMatchClick(mockMatchRef);
    });

    expect(useSearchStore.getState().selectMatch).toHaveBeenCalledWith(mockMatchRef);
    // After one click, canGoBack should still be false (no previous state)
    expect(result.current.canGoBack).toBe(false);
  });

  it("should be able to go back after two clicks", () => {
    const { result } = renderHook(() => useHistory());
    const match1 = { path: "/1.txt", origin: { TextFile: { line: 1, col: 1 } } } as any;
    const match2 = { path: "/2.txt", origin: { TextFile: { line: 1, col: 1 } } } as any;

    act(() => {
      result.current.handleMatchClick(match1);
    });
    act(() => {
      result.current.handleMatchClick(match2);
    });

    expect(result.current.canGoBack).toBe(true);

    act(() => {
      result.current.goBack();
    });

    expect(useSearchStore.getState().selectMatch).toHaveBeenLastCalledWith(match1);
    expect(result.current.canGoBack).toBe(false);
    expect(result.current.canGoForward).toBe(true);

    act(() => {
      result.current.goForward();
    });

    expect(useSearchStore.getState().selectMatch).toHaveBeenLastCalledWith(match2);
    expect(result.current.canGoBack).toBe(true);
    expect(result.current.canGoForward).toBe(false);
  });

  it("should handle handleFileClick", () => {
    const { result } = renderHook(() => useHistory());
    const path = "/test.txt";

    act(() => {
      result.current.handleFileClick(path);
    });

    expect(useSearchStore.getState().selectMatch).toHaveBeenCalledWith(expect.objectContaining({
      path,
      origin: expect.objectContaining({ TextFile: expect.any(Object) }),
    }));
  });
});
