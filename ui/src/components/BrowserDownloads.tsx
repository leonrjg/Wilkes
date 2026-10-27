// The pane's downloads, which go into the library root rather than the
// system's Downloads folder. A strip under the pane's header, like the cookie
// import, since nothing drawn over the page would be seen.
import { CheckCircle, Loader, X, XCircle } from "react-feather";
import { useBrowserStore, type BrowserDownload } from "../stores/useBrowserStore";
import { useViewerStore } from "../stores/useViewerStore";

function describe(download: BrowserDownload): string {
  switch (download.state) {
    case "saving":
      return `Saving ${download.name} to the library…`;
    case "saved":
      return `Saved ${download.name} to the library`;
    case "failed":
      return `Could not download ${download.name}`;
    case "refused":
      return `Did not save ${download.name}: ${download.reason}`;
  }
}

export default function BrowserDownloads() {
  const downloads = useBrowserStore((s) => s.downloads);
  const clearDownloads = useBrowserStore((s) => s.clearDownloads);
  if (downloads.length === 0) return null;

  return (
    <div className="flex items-start gap-1.5 border-b border-[var(--border-main)] px-2 py-1.5 text-[11px] text-[var(--text-muted)]">
      <ul aria-label="Downloads" className="min-w-0 flex-1 space-y-1">
        {downloads.slice(0, 3).map((download) => (
          <li key={download.url} className="flex min-w-0 items-center gap-1.5">
            {download.state === "saving" && <Loader size={11} className="shrink-0 animate-spin" />}
            {download.state === "saved" && <CheckCircle size={11} className="shrink-0 text-[var(--accent-blue)]" />}
            {(download.state === "failed" || download.state === "refused") && (
              <XCircle size={11} className="shrink-0 text-[var(--accent-red)]" />
            )}
            <span className="min-w-0 truncate" title={download.path ?? download.url}>
              {describe(download)}
            </span>
            {download.state === "saved" && download.path && (
              <button
                type="button"
                onClick={() => useViewerStore.getState().openFile(download.path!)}
                className="shrink-0 rounded px-1 text-[var(--accent-blue)] hover:bg-[var(--bg-active)]"
              >
                Open
              </button>
            )}
          </li>
        ))}
      </ul>
      <button
        type="button"
        aria-label="Clear downloads"
        onClick={clearDownloads}
        className="inline-flex shrink-0 rounded p-1 text-[var(--text-dim)] transition-colors hover:bg-[var(--bg-active)] hover:text-[var(--text-main)]"
      >
        <X size={12} />
      </button>
    </div>
  );
}
