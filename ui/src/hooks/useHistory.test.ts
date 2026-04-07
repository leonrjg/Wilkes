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

  it("should use line 0 when opening a file directly so no line is highlighted", () => {
    const { result } = renderHook(() => useHistory());

    act(() => {
      result.current.handleFileClick("/test.txt");
    });

    const call = (useSearchStore.getState().selectMatch as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(call.origin).toEqual({ TextFile: { line: 0, col: 0 } });
  });

  it("should use page 1 and no bbox when opening a PDF directly", () => {
    const { result } = renderHook(() => useHistory());

    act(() => {
      result.current.handleFileClick("/doc.pdf");
    });

    const call = (useSearchStore.getState().selectMatch as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(call.origin).toEqual({ PdfPage: { page: 1, bbox: null } });
  });
});
