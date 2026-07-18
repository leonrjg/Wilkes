import { useCallback, useEffect, useRef, useState } from "react";

/**
 * Substrate-agnostic controller for an in-document find bar: open/close state,
 * the query string, and which of `matchCount` matches is active. It owns the
 * keyboard contract (Cmd/Ctrl+F to open, Esc to close, Enter / Shift+Enter to
 * step) so every viewer -- PDF, Markdown -- shares one behaviour. How matches
 * are computed and drawn is left entirely to the caller.
 */
export function useDocumentFind(matchCount: number) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [isOpen, setIsOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [currentIdx, setCurrentIdx] = useState(-1);

  const open = useCallback(() => {
    setIsOpen(true);
    // The input mounts with the bar; focus once the browser has painted it.
    setTimeout(() => inputRef.current?.focus(), 50);
  }, []);

  const close = useCallback(() => setIsOpen(false), []);

  // The active index only makes sense relative to the current match set, so the
  // match set is its single source of truth: any change snaps back to the first
  // match (or nothing when empty).
  useEffect(() => {
    setCurrentIdx(matchCount > 0 ? 0 : -1);
  }, [matchCount]);

  const next = useCallback(() => {
    if (matchCount === 0) return;
    setCurrentIdx((prev) => (prev + 1) % matchCount);
  }, [matchCount]);

  const prev = useCallback(() => {
    if (matchCount === 0) return;
    setCurrentIdx((prev) => (prev - 1 + matchCount) % matchCount);
  }, [matchCount]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key === "f") {
        event.preventDefault();
        open();
      } else if (event.key === "Escape" && isOpen) {
        close();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isOpen, open, close]);

  const onInputKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      if (event.shiftKey) prev();
      else next();
    },
    [next, prev],
  );

  return { inputRef, isOpen, open, close, query, setQuery, currentIdx, next, prev, onInputKeyDown };
}

export type DocumentFind = ReturnType<typeof useDocumentFind>;
