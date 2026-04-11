import { useEffect, useRef, useState } from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import type { BoundingBox } from "../../lib/types";

export interface InnerMatch {
  page: number;
  bbox: BoundingBox;
}

export function usePdfInnerSearch(
  pdf: PDFDocumentProxy | null,
  scrollToPage: (page: number) => void,
) {
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [isSearchOpen, setIsSearchOpen] = useState(false);
  const [innerQuery, setInnerQuery] = useState("");
  const [innerMatches, setInnerMatches] = useState<InnerMatch[]>([]);
  const [currentMatchIdx, setCurrentMatchIdx] = useState(-1);
  const [isSearching, setIsSearching] = useState(false);

  useEffect(() => {
    if (!isSearchOpen || !innerQuery.trim() || !pdf) {
      setInnerMatches([]);
      setCurrentMatchIdx(-1);
      return;
    }

    const abort = new AbortController();

    const search = async () => {
      setIsSearching(true);
      const matches: InnerMatch[] = [];
      const query = innerQuery.toLowerCase();

      try {
        for (let i = 1; i <= pdf.numPages; i++) {
          if (abort.signal.aborted) return;
          const p = await pdf.getPage(i);
          const textContent = await p.getTextContent();

          for (const item of textContent.items) {
            if ("str" in item) {
              const text = item.str.toLowerCase();
              if (text.includes(query)) {
                const [scX, _skY, _skX, scY, tx, ty] = item.transform;
                const vp = p.getViewport({ scale: 1 });
                matches.push({
                  page: i,
                  bbox: {
                    x: tx,
                    y: vp.height - ty - scY,
                    width: item.width || text.length * scX * 0.6,
                    height: Math.abs(scY),
                  },
                });
              }
            }
          }
        }

        if (!abort.signal.aborted) {
          setInnerMatches(matches);
          setCurrentMatchIdx(matches.length > 0 ? 0 : -1);
          if (matches.length > 0) scrollToPage(matches[0].page);
        }
      } catch (e) {
        console.error("PDF inner search failed:", e);
      } finally {
        if (!abort.signal.aborted) setIsSearching(false);
      }
    };

    const timeout = setTimeout(search, 300);
    return () => {
      abort.abort();
      clearTimeout(timeout);
    };
  }, [innerQuery, isSearchOpen, pdf]);

  useEffect(() => {
    if (currentMatchIdx >= 0 && innerMatches[currentMatchIdx]) {
      scrollToPage(innerMatches[currentMatchIdx].page);
    }
  }, [currentMatchIdx, innerMatches]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        setIsSearchOpen(true);
        setTimeout(() => searchInputRef.current?.focus(), 50);
      }
      if (e.key === "Escape" && isSearchOpen) {
        setIsSearchOpen(false);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isSearchOpen]);

  const handleNextMatch = () => {
    if (innerMatches.length === 0) return;
    setCurrentMatchIdx((prev) => (prev + 1) % innerMatches.length);
  };

  const handlePrevMatch = () => {
    if (innerMatches.length === 0) return;
    setCurrentMatchIdx((prev) => (prev - 1 + innerMatches.length) % innerMatches.length);
  };

  const handleSearchInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    if (e.shiftKey) {
      handlePrevMatch();
      return;
    }
    handleNextMatch();
  };

  return {
    searchInputRef,
    isSearchOpen,
    setIsSearchOpen,
    innerQuery,
    setInnerQuery,
    innerMatches,
    currentMatchIdx,
    isSearching,
    handleNextMatch,
    handlePrevMatch,
    handleSearchInputKeyDown,
  };
}
