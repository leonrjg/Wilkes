import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { usePdfInnerSearch } from "./usePdfInnerSearch";

describe("usePdfInnerSearch", () => {
  const mockPage = {
    getTextContent: vi.fn().mockResolvedValue({
      items: [
        { str: "Hello world", transform: [1, 0, 0, 1, 10, 10], width: 50 },
      ],
    }),
    getViewport: vi.fn().mockReturnValue({ width: 600, height: 800 }),
  };

  const mockPdf = {
    numPages: 2,
    getPage: vi.fn().mockResolvedValue(mockPage),
  };

  const scrollToPage = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    vi.useFakeTimers();
  });

  it("initializes with default values", () => {
    const { result } = renderHook(() => usePdfInnerSearch(null, scrollToPage));
    expect(result.current.isSearchOpen).toBe(false);
    expect(result.current.innerQuery).toBe("");
    expect(result.current.innerMatches).toEqual([]);
  });

  it("opens search on Ctrl+F", async () => {
    renderHook(() => usePdfInnerSearch(null, scrollToPage));
    
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "f", ctrlKey: true }));
    });
    
    // The focus timeout
    act(() => {
      vi.advanceTimersByTime(100);
    });
  });

  it("performs search when query and pdf are present", async () => {
    const { result } = renderHook(() => usePdfInnerSearch(mockPdf as any, scrollToPage));
    
    act(() => {
      result.current.setIsSearchOpen(true);
      result.current.setInnerQuery("hello");
    });
    
    // Advance timer for debounce
    await act(async () => {
      vi.advanceTimersByTime(300);
    });
    
    expect(mockPdf.getPage).toHaveBeenCalledWith(1);
    expect(mockPdf.getPage).toHaveBeenCalledWith(2);
    expect(result.current.innerMatches.length).toBeGreaterThan(0);
    expect(scrollToPage).toHaveBeenCalledWith(1);
  });

  it("navigates through matches", async () => {
    const { result } = renderHook(() => usePdfInnerSearch(mockPdf as any, scrollToPage));
    
    act(() => {
      result.current.setIsSearchOpen(true);
      result.current.setInnerQuery("hello");
    });
    
    await act(async () => {
      vi.advanceTimersByTime(300);
    });
    
    expect(result.current.currentMatchIdx).toBe(0);
    
    act(() => {
      result.current.handleNextMatch();
    });
    expect(result.current.currentMatchIdx).toBe(1); // Second match
    
    act(() => {
      result.current.handleNextMatch();
    });
    expect(result.current.currentMatchIdx).toBe(0); // Wrapped around
    
    act(() => {
      result.current.handlePrevMatch();
    });
    expect(result.current.currentMatchIdx).toBe(1); // Wrapped backwards
  });

  it("advances on Enter and goes backward on Shift+Enter", async () => {
    const { result } = renderHook(() => usePdfInnerSearch(mockPdf as any, scrollToPage));

    act(() => {
      result.current.setIsSearchOpen(true);
      result.current.setInnerQuery("hello");
    });

    await act(async () => {
      vi.advanceTimersByTime(300);
    });

    const preventDefault = vi.fn();

    act(() => {
      result.current.handleSearchInputKeyDown({
        key: "Enter",
        shiftKey: false,
        preventDefault,
      } as any);
    });
    expect(preventDefault).toHaveBeenCalled();
    expect(result.current.currentMatchIdx).toBe(1);

    act(() => {
      result.current.handleSearchInputKeyDown({
        key: "Enter",
        shiftKey: true,
        preventDefault: vi.fn(),
      } as any);
    });
    expect(result.current.currentMatchIdx).toBe(0);
  });

  it("closes search on Escape", () => {
    const { result } = renderHook(() => usePdfInnerSearch(null, scrollToPage));
    
    act(() => {
      result.current.setIsSearchOpen(true);
    });
    expect(result.current.isSearchOpen).toBe(true);
    
    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(result.current.isSearchOpen).toBe(false);
  });
});
