export const ZOOM_STEP = 0.1;

interface ZoomControlsProps {
  zoom: number;
  onZoomIn: () => void;
  onZoomOut: () => void;
}

/**
 * The −/percentage/+ zoom cluster shared by the PDF and Markdown viewers. It is
 * purely presentational: each viewer keeps its own zoom state, clamping, and the
 * meaning of a zoom step (canvas scale vs. font size), and passes the handlers.
 * Rendered as a fragment so callers own the surrounding container.
 */
export default function ZoomControls({ zoom, onZoomIn, onZoomOut }: ZoomControlsProps) {
  return (
    <>
      <button onClick={onZoomOut} aria-label="Zoom out" className="px-1.5 py-0.5 hover:text-[var(--accent-blue)]">
        −
      </button>
      <span className="w-12 text-center font-mono">{Math.round(zoom * 100)}%</span>
      <button onClick={onZoomIn} aria-label="Zoom in" className="px-1.5 py-0.5 hover:text-[var(--accent-blue)]">
        +
      </button>
    </>
  );
}
