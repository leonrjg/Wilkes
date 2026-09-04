import { Check, Download, ExternalLink } from "react-feather";
import type { CatalogueHit } from "../lib/types";
import { api } from "../services";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import { hitKey, useCatalogueStore } from "../stores/useCatalogueStore";
import { Tooltip } from "@leonrjg/wilkes-reader";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

const GRAIN_LABELS: Record<string, string> = {
  textbook: "Textbook",
  course: "Course",
  reference: "Reference",
};

const PROVIDER_LABELS: Record<string, string> = {
  libretexts: "LibreTexts",
  openstax: "OpenStax",
  mit_ocw: "MIT OpenCourseWare",
  devdocs: "DevDocs",
};

interface Props {
  hit: CatalogueHit;
  /** Compact drops the blurb: the strip under a search result is an offer, not
   *  a reading list, and four two-line rows beat two paragraphs. */
  compact?: boolean;
}

/**
 * One catalogue candidate, shared by the browse pane and the search-result
 * strip so that a candidate looks and behaves the same wherever it is offered.
 *
 * The grain and the licence are shown rather than tucked away: they are what
 * the thing *is* and what may be done with it, and a row that hid either would
 * be inviting a click it cannot describe.
 */
export default function CatalogueCandidate({ hit, compact = false }: Props) {
  const { add, canAdd, needsDirectory, readOnly, isAdding, isAdded } = useCatalogueAdd();
  const adding = isAdding(hit);
  const added = isAdded(hit);
  // Keyed by the URL this row asked for, so two rows added at once do not
  // read each other's bytes.
  const download = useCatalogueStore((s) =>
    hit.pdf_url === null ? undefined : s.downloads[hit.pdf_url],
  );
  // A course reports its own two-stage progress; the byte stream above cannot
  // say which of forty documents it belongs to.
  const courseProgress = useCatalogueStore((s) =>
    hit.landing_url === null ? undefined : s.courseProgress[hit.landing_url],
  );
  const course = useCatalogueStore((s) => s.courses[hitKey(hit)]);
  const link = hit.landing_url ?? hit.outline_url;

  // What adding would do was decided in core, from the provider that published
  // the record. A record nothing can fetch says so rather than offering a
  // button that would fail on click.
  const isCourse = hit.acquisition === "course";
  const acquirable = hit.acquisition !== "none";

  const addTitle = readOnly
    ? "This workspace is read-only"
    : needsDirectory
      ? "Choose a directory before adding documents"
      : isCourse
        ? "Fetch every document this course publishes, with a generated syllabus, into the current directory"
        : "Fetch this document and add it to the current directory";

  return (
    <div className="flex items-start justify-between gap-3 py-2">
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] font-medium text-[var(--text-main)]">{hit.title}</span>
          <span className="px-1.5 py-0.5 rounded bg-[var(--bg-app)] border border-[var(--border-main)] text-[9px] uppercase tracking-wider text-[var(--text-dim)]">
            {GRAIN_LABELS[hit.grain] ?? hit.grain}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[10px] text-[var(--text-dim)]">
          <span>{PROVIDER_LABELS[hit.provider] ?? hit.provider}</span>
          {hit.subject && <span className="truncate">{hit.subject}</span>}
          {hit.license && <span className="uppercase tracking-wider">{hit.license}</span>}
          {hit.pages !== null && <span>{hit.pages.toLocaleString()} pp</span>}
        </div>
        {!compact && hit.summary && (
          <p className="text-[10px] leading-relaxed text-[var(--text-muted)] line-clamp-3">
            {hit.summary}
          </p>
        )}
        {adding && isCourse && courseProgress !== undefined && (
          <div className="flex flex-col gap-1 pt-0.5">
            <span className="text-[10px] text-[var(--text-dim)]">
              {courseProgress.stage === "manifest"
                ? `Reading the course${courseProgress.total !== null ? ` — ${courseProgress.done} of ${courseProgress.total}` : ""}`
                : `Document ${courseProgress.done}${courseProgress.total !== null ? ` of ${courseProgress.total}` : ""}`}
            </span>
            {courseProgress.total !== null && courseProgress.total > 0 && (
              <div
                role="progressbar"
                aria-label={`Fetching ${hit.title}`}
                aria-valuemin={0}
                aria-valuemax={courseProgress.total}
                aria-valuenow={courseProgress.done}
                className="h-0.5 w-full overflow-hidden rounded bg-[var(--bg-app)]"
              >
                <div
                  className="h-full bg-[var(--accent-blue)] transition-[width] duration-200"
                  style={{
                    width: `${Math.min(100, Math.round((courseProgress.done / courseProgress.total) * 100))}%`,
                  }}
                />
              </div>
            )}
          </div>
        )}
        {added && course !== undefined && (
          /* What a course actually turned out to be. A gap in the sequence has
             a reason, and saying "4 audiovisual" is how a reader learns the
             lectures they cannot find were never documents. */
          <span className="text-[10px] text-[var(--text-dim)]">
            {course.documents.length} document
            {course.documents.length === 1 ? "" : "s"} and a syllabus
            {course.skipped.length > 0 && `, ${course.skipped.length} skipped`}
            {course.failures.length > 0 && `, ${course.failures.length} failed`}
          </span>
        )}
        {adding && !isCourse && download !== undefined && (
          <div className="flex flex-col gap-1 pt-0.5">
            <span className="text-[10px] text-[var(--text-dim)]">
              {download.total_bytes !== null
                ? `${formatBytes(download.received_bytes)} of ${formatBytes(download.total_bytes)}`
                : /* A chunked response has no length, so there is a figure but
                     no fraction. Saying how much has arrived is still more than
                     an ellipsis says. */
                  `${formatBytes(download.received_bytes)} so far`}
            </span>
            {download.total_bytes !== null && download.total_bytes > 0 && (
              <div
                role="progressbar"
                aria-label={`Downloading ${hit.title}`}
                aria-valuemin={0}
                aria-valuemax={download.total_bytes}
                aria-valuenow={download.received_bytes}
                className="h-0.5 w-full overflow-hidden rounded bg-[var(--bg-app)]"
              >
                <div
                  className="h-full bg-[var(--accent-blue)] transition-[width] duration-200"
                  style={{
                    width: `${Math.min(100, Math.round((download.received_bytes / download.total_bytes) * 100))}%`,
                  }}
                />
              </div>
            )}
          </div>
        )}
        {!acquirable && (
          <span className="text-[10px] text-[var(--text-dim)]">
            This catalogue does not publish a downloadable copy — open it to read
            it where it lives.
          </span>
        )}
        {isCourse && !adding && !added && (
          <span className="text-[10px] text-[var(--text-dim)]">
            A course, not a file: adding fetches its documents and writes a
            syllabus from the pages OCW publishes only on the web.
          </span>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        {link !== null && (
          <Tooltip content="Open in your browser">
            <a
              href={link}
              rel="noreferrer noopener"
              // The desktop webview does not open a new window for a plain
              // anchor, so the href is here for the context menu and the click
              // goes through the same bridge every other external link uses.
              onClick={(e) => {
                e.preventDefault();
                api
                  .openPath(link)
                  .catch((err) => console.error("Open catalogue link failed:", err));
              }}
              aria-label={`Open ${hit.title}`}
              className="flex h-[26px] w-[26px] items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-muted)] transition-colors hover:text-[var(--text-main)]"
            >
              <ExternalLink size={12} />
            </a>
          </Tooltip>
        )}
        {acquirable && (
          <Tooltip content={added ? "Added to this library" : addTitle}>
            <button
              type="button"
              onClick={() => void add(hit)}
              disabled={!canAdd || adding || added}
              aria-label={`Add ${hit.title} to library`}
              className="flex h-[26px] items-center gap-1 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-2 text-[10px] font-bold uppercase tracking-wider text-[var(--text-main)] transition-colors hover:bg-[var(--bg-active)] disabled:opacity-50"
            >
              {added ? (
                <>
                  <Check size={11} /> Added
                </>
              ) : adding ? (
                <span aria-label="Downloading">…</span>
              ) : (
                <>
                  <Download size={11} /> {isCourse ? "Add course" : "Add"}
                </>
              )}
            </button>
          </Tooltip>
        )}
      </div>
    </div>
  );
}
