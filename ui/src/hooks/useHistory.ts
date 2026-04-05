import { useRef, useState, useCallback } from "react";
import { useSearchStore } from "../stores/useSearchStore";
import type { MatchRef } from "../lib/types";

export function useHistory() {
  const [history, setHistory] = useState<MatchRef[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const isNavigatingHistory = useRef(false);
  const selectMatch = useSearchStore((s) => s.selectMatch);

  const addToHistory = useCallback(
    (matchRef: MatchRef) => {
      if (isNavigatingHistory.current) return;
      setHistory((prev) => {
        const next = prev.slice(0, historyIndex + 1);
        if (
          next.length > 0 &&
          next[next.length - 1].path === matchRef.path &&
          JSON.stringify(next[next.length - 1].origin) === JSON.stringify(matchRef.origin)
        ) {
          return prev;
        }
        return [...next, matchRef];
      });
      setHistoryIndex((prev) => prev + 1);
    },
    [historyIndex],
  );

  const goBack = useCallback(() => {
    if (historyIndex > 0) {
      isNavigatingHistory.current = true;
      const nextIndex = historyIndex - 1;
      const matchRef = history[nextIndex];
      setHistoryIndex(nextIndex);
      selectMatch(matchRef);
      setTimeout(() => {
        isNavigatingHistory.current = false;
      }, 0);
    }
  }, [history, historyIndex, selectMatch]);

  const goForward = useCallback(() => {
    if (historyIndex < history.length - 1) {
      isNavigatingHistory.current = true;
      const nextIndex = historyIndex + 1;
      const matchRef = history[nextIndex];
      setHistoryIndex(nextIndex);
      selectMatch(matchRef);
      setTimeout(() => {
        isNavigatingHistory.current = false;
      }, 0);
    }
  }, [history, historyIndex, selectMatch]);

  const handleMatchClick = useCallback(
    (matchRef: MatchRef) => {
      addToHistory(matchRef);
      selectMatch(matchRef);
    },
    [addToHistory, selectMatch],
  );

  const handleFileClick = useCallback(
    (path: string) => {
      const isPdf = path.toLowerCase().endsWith(".pdf");
      const origin: MatchRef["origin"] = isPdf
        ? { PdfPage: { page: 1, bbox: null } }
        : { TextFile: { line: 1, col: 0 } };
      const matchRef: MatchRef = { path, origin };
      addToHistory(matchRef);
      selectMatch(matchRef);
    },
    [addToHistory, selectMatch],
  );

  return {
    canGoBack: historyIndex > 0,
    canGoForward: historyIndex < history.length - 1,
    goBack,
    goForward,
    handleMatchClick,
    handleFileClick,
  };
}
