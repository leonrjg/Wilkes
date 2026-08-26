import { createContext, useContext, type ReactNode } from "react";

/** The appearance a reader renders in. The host resolves whatever theme setting
 *  it has ("system", a class on the document, an OS query) down to this. */
export type ColorScheme = "light" | "dark";

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
  /** The appearance to render in. The *treatment* is the readers' own -- the
   *  inverted PDF canvas, the dark syntax theme -- but which one applies is the
   *  application's decision, and how it decides is its own business. Reading it
   *  off a class on the document, as these readers used to, silently assumes
   *  the host stamps that particular class on that particular element. */
  colorScheme: ColorScheme;
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
