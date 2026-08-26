/**
 * Imperative control of a mounted reader.
 *
 * Navigation is a command, not state. Expressing "go here now" as props means
 * the host cannot ask for the same destination twice (nothing changed, so
 * nothing happens) and has to smuggle re-triggers in through counters. A ref
 * says what is actually meant, once, at the moment it is meant.
 *
 * Declarative props still own the *initial* destination for a document — where
 * a reader opens is a property of what was opened, not an action.
 */
export interface ReaderHandle {
  /** Bring a decoration into view. No-op when no decoration has that id. */
  scrollToDecoration: (id: string) => void;
}

export interface FindableReaderHandle extends ReaderHandle {
  /** Open the find bar, optionally seeded with a query. */
  openFind: (query?: string) => void;
  closeFind: () => void;
}

export interface ZoomableReaderHandle {
  getZoom: () => number;
  /** Set the zoom factor. Clamped to the reader's own limits. */
  setZoom: (zoom: number) => void;
}
