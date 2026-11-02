import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { FileText } from "react-feather";
import { loadFullCover, loadThumbnail, watchVisibility } from "../lib/covers";
import { fileName } from "./DocumentEntryRow";

type CoverLoader = (path: string, signal: AbortSignal) => Promise<string>;

/** The picture for `path`, for as long as `path` is given. The bytes stay in
 *  the host's cache rather than in this window: a row that leaves the screen
 *  lets go of its picture, so a long list holds only what is showing. */
function useCover(path: string | null, version: string, load: CoverLoader) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (path === null) return;
    const controller = new AbortController();
    let loaded: string | null = null;
    load(path, controller.signal)
      .then((objectUrl) => {
        if (controller.signal.aborted) {
          URL.revokeObjectURL(objectUrl);
          return;
        }
        loaded = objectUrl;
        setUrl(objectUrl);
      })
      .catch((error) => {
        if (controller.signal.aborted) return;
        console.warn(`No cover for ${path}:`, error);
        setFailed(true);
      });
    return () => {
      controller.abort();
      if (loaded) URL.revokeObjectURL(loaded);
      setUrl(null);
      setFailed(false);
    };
  }, [path, version, load]);

  return { url, failed };
}

interface DocumentCoverProps {
  path: string;
  /** Changes when the file does, so a row in a live list draws the new cover. */
  version: string;
  /** Absent where clicking the picture should count as clicking the row. */
  onOpen?: () => void;
}

/** A document's cover in a list row. It sits inside the row's button, so it is
 *  a `role="button"` span rather than a button, as the row's tags are. */
export function DocumentCover({ path, version, onOpen }: DocumentCoverProps) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  const { url } = useCover(visible ? path : null, version, loadThumbnail);

  useEffect(() => {
    if (!anchorRef.current) return;
    return watchVisibility(anchorRef.current, setVisible);
  }, []);

  const open = url && onOpen ? onOpen : undefined;

  return (
    <span
      ref={anchorRef}
      role={open ? "button" : undefined}
      tabIndex={open ? 0 : undefined}
      aria-label={open ? `Show cover of ${fileName(path)}` : undefined}
      onClick={open ? (event) => { event.stopPropagation(); open(); } : undefined}
      onKeyDown={open ? (event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        event.stopPropagation();
        open();
      } : undefined}
      className={`flex h-10 w-7 flex-shrink-0 items-center justify-center overflow-hidden rounded-sm border border-[var(--border-main)] bg-[var(--bg-hover)] text-[var(--text-dim)] ${
        open ? "cursor-zoom-in hover:border-[var(--border-strong)]" : ""
      }`}
    >
      {url
        ? <img src={url} alt="" draggable={false} className="h-full w-full object-contain" />
        : <FileText size={12} aria-hidden="true" />}
    </span>
  );
}

interface CoverViewerProps {
  path: string;
  onClose: () => void;
}

/** One document's cover, as large as the window allows. */
export function CoverViewer({ path, onClose }: CoverViewerProps) {
  const { url, failed } = useCover(path, "", loadFullCover);

  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return createPortal(
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`Cover of ${fileName(path)}`}
      onClick={onClose}
      className="fixed inset-0 z-[1100] flex cursor-zoom-out items-center justify-center bg-black/70"
    >
      {url ? (
        <img
          src={url}
          alt={`Cover of ${fileName(path)}`}
          className="max-h-[92vh] max-w-[92vw] object-contain shadow-2xl"
        />
      ) : failed ? (
        <span className="text-sm text-white">This cover could not be drawn.</span>
      ) : (
        <div
          role="status"
          aria-label="Loading cover"
          className="h-6 w-6 animate-spin rounded-full border-2 border-white border-t-transparent"
        />
      )}
    </div>,
    document.body,
  );
}
