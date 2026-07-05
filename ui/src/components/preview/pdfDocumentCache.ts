import { useEffect, useState } from "react";
import { pdfjs } from "react-pdf";
import type { PDFDocumentProxy } from "pdfjs-dist";

// The parsed PDF documents (`PDFDocumentProxy`) for the N most-recently opened
// files are kept alive here so switching back to a recent document is instant.
// This module is the single owner of the document lifecycle: proxies are
// destroyed only on eviction, never on component unmount. That deliberately
// takes the lifecycle away from react-pdf's `<Document>`, which destroys its
// loading task (and the proxy) on unmount and would otherwise force a full
// re-fetch + re-parse every time the reader navigates back and forth.
const MAX_CACHED_DOCUMENTS = 3;

interface CacheEntry {
  /** Resolves to the proxy; shared so concurrent mounts don't double-load. */
  promise: Promise<PDFDocumentProxy>;
  /** The resolved proxy once available, for synchronous revisit rendering. */
  proxy: PDFDocumentProxy | null;
}

// Insertion order in a Map is its LRU order: the first key is least-recently
// used, the last is most-recently used.
const cache = new Map<string, CacheEntry>();

function touch(url: string, entry: CacheEntry) {
  cache.delete(url);
  cache.set(url, entry);
}

function evictExcess() {
  while (cache.size > MAX_CACHED_DOCUMENTS) {
    const oldestUrl = cache.keys().next().value as string;
    const oldest = cache.get(oldestUrl);
    cache.delete(oldestUrl);
    // The evicted document is, by definition, not the active one (the active
    // document is always the most-recently touched entry), so destroying it
    // cannot pull the rug from under a mounted viewer.
    oldest?.promise
      .then((proxy) => proxy.destroy())
      .catch(() => {
        /* Load already failed and was removed below; nothing to destroy. */
      });
  }
}

/** The parsed proxy if this document is currently cached, else null. Lets a
 *  revisited document render synchronously with no reload flash. */
export function peekCachedPdfDocument(url: string): PDFDocumentProxy | null {
  return cache.get(url)?.proxy ?? null;
}

/** Load a PDF, reusing the cached proxy when present. */
export function loadPdfDocument(url: string): Promise<PDFDocumentProxy> {
  const existing = cache.get(url);
  if (existing) {
    touch(url, existing);
    return existing.promise;
  }

  const loadingTask = pdfjs.getDocument(url);
  const entry: CacheEntry = { proxy: null, promise: loadingTask.promise };
  entry.promise = loadingTask.promise.then(
    (proxy) => {
      entry.proxy = proxy;
      return proxy;
    },
    (error) => {
      // Drop the failed entry so a later open retries instead of replaying the
      // rejection forever.
      cache.delete(url);
      throw error;
    },
  );
  cache.set(url, entry);
  evictExcess();
  return entry.promise;
}

/** The cached-or-loading `PDFDocumentProxy` for `url`, or null while loading. */
export function usePdfDocument(url: string): PDFDocumentProxy | null {
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(() => peekCachedPdfDocument(url));

  useEffect(() => {
    const cached = peekCachedPdfDocument(url);
    if (cached) {
      setPdf(cached);
      return;
    }

    let cancelled = false;
    setPdf(null);
    loadPdfDocument(url)
      .then((proxy) => {
        if (!cancelled) setPdf(proxy);
      })
      .catch((e) => {
        if (!cancelled) console.error("PDF document load failed:", e);
      });

    return () => {
      cancelled = true;
    };
  }, [url]);

  return pdf;
}
