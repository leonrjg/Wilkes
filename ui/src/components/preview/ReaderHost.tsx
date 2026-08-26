import { createContext, useContext, type ReactNode } from "react";

/**
 * The services a reader needs from the application hosting it. This exists so
 * the readers reach for an injected capability instead of importing the app's
 * singletons: there is exactly one Tauri `api` and one settings store in
 * Wilkes, and a reader that imports them cannot be mounted anywhere else.
 *
 * Deliberately not optional and deliberately not defaulted — a reader rendered
 * without a host is a wiring mistake, and a silent fallback would hide it until
 * a link failed to open in front of a user.
 */
export interface ReaderHostServices {
  /** Open a URL or path outside the reader (external links, `file://`). */
  openExternal: (url: string) => void;
  /** Target CSS-pixel height for body text when a PDF is first opened.
   *  `undefined` disables auto-zoom. */
  pdfAutoZoomTargetPx?: number;
}

const ReaderHostContext = createContext<ReaderHostServices | null>(null);

export function ReaderHostProvider({
  value,
  children,
}: {
  value: ReaderHostServices;
  children: ReactNode;
}) {
  return <ReaderHostContext.Provider value={value}>{children}</ReaderHostContext.Provider>;
}

export function useReaderHost(): ReaderHostServices {
  const host = useContext(ReaderHostContext);
  if (!host) {
    throw new Error(
      "Reader components must be rendered inside <ReaderHostProvider>; " +
        "it supplies openExternal and the reader settings.",
    );
  }
  return host;
}
