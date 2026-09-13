import { Check, Download, ExternalLink } from "react-feather";
import type { CatalogueDownloadProgress, CatalogueHit, LiteratureSearchResult } from "../lib/types";
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
          <DownloadProgress download={download} title={hit.title} />
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
        {link !== null && <OpenLink link={link} title={hit.title} />}
        {acquirable && (
          <AddButton
            title={hit.title}
            tooltip={added ? "Added to this library" : addTitle}
            label={isCourse ? "Add course" : "Add"}
            onAdd={() => void add(hit)}
            disabled={!canAdd || adding || added}
            adding={adding}
            added={added}
          />
        )}
      </div>
    </div>
  );
}

function DownloadProgress({
  download,
  title,
}: {
  download: CatalogueDownloadProgress;
  title: string;
}) {
  return (
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
          aria-label={`Downloading ${title}`}
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
  );
}

function OpenLink({ link, title }: { link: string; title: string }) {
  return (
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
        aria-label={`Open ${title}`}
        className="flex h-[26px] w-[26px] items-center justify-center rounded border border-[var(--border-main)] bg-[var(--bg-app)] text-[var(--text-muted)] transition-colors hover:text-[var(--text-main)]"
      >
        <ExternalLink size={12} />
      </a>
    </Tooltip>
  );
}

function AddButton({
  title,
  tooltip,
  label,
  onAdd,
  disabled,
  adding,
  added,
}: {
  title: string;
  tooltip: string;
  label: string;
  onAdd: () => void;
  disabled: boolean;
  adding: boolean;
  added: boolean;
}) {
  return (
    <Tooltip content={tooltip}>
      <button
        type="button"
        onClick={onAdd}
        disabled={disabled}
        aria-label={`Add ${title} to library`}
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
            <Download size={11} /> {label}
          </>
        )}
      </button>
    </Tooltip>
  );
}

interface PaperProps {
  /** The literature provider that returned the work, as the registry names it. */
  provider: string;
  work: LiteratureSearchResult;
}

/**
 * One scholarly work a literature provider returned, beside the catalogue
 * candidates in the same pane.
 *
 * Its own row rather than a `CatalogueCandidate`: a paper is described by a
 * venue, a year and a citation count, and a textbook by a subject, a grain and
 * a licence, and a row that tried to be both would show blanks in either. The
 * link, the add button and the download progress are the same parts, because
 * adding either one is the same act.
 */
export function PaperCandidate({ provider, work }: PaperProps) {
  const { addPaper, canAdd, needsDirectory, readOnly, isPaperAdding, isPaperAdded } =
    useCatalogueAdd();
  const adding = isPaperAdding(provider, work);
  const added = isPaperAdded(provider, work);
  const download = useCatalogueStore((s) =>
    work.pdf_url === null ? undefined : s.downloads[work.pdf_url],
  );
  // A provider may return a record with no title. It is still a record the
  // user can open, so it is named by what identifies it rather than dropped.
  const title = work.title ?? (work.doi !== null ? `DOI ${work.doi}` : work.id);
  // A user-described provider may report the DOI already as a resolver URL.
  const doiLink =
    work.doi === null ? null : /^https?:\/\//i.test(work.doi) ? work.doi : `https://doi.org/${work.doi}`;
  const link = work.landing_page_url ?? doiLink;

  const addTitle = readOnly
    ? "This workspace is read-only"
    : needsDirectory
      ? "Choose a directory before adding documents"
      : "Fetch the open-access copy and add it to the current directory";

  return (
    <div className="flex items-start justify-between gap-3 py-2">
      <div className="flex min-w-0 flex-col gap-1">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="text-[11px] font-medium text-[var(--text-main)]">{title}</span>
          <span className="px-1.5 py-0.5 rounded bg-[var(--bg-app)] border border-[var(--border-main)] text-[9px] uppercase tracking-wider text-[var(--text-dim)]">
            Paper
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[10px] text-[var(--text-dim)]">
          {work.year !== null && <span>{work.year}</span>}
          {work.venue && <span className="truncate">{work.venue}</span>}
          <span>
            {work.citation_count.toLocaleString()} citation
            {work.citation_count === 1 ? "" : "s"}
          </span>
          {work.is_open_access && <span className="uppercase tracking-wider">Open access</span>}
          {work.license && <span className="uppercase tracking-wider">{work.license}</span>}
        </div>
        {adding && download !== undefined && <DownloadProgress download={download} title={title} />}
        {work.pdf_url === null && (
          <span className="text-[10px] text-[var(--text-dim)]">
            {work.is_open_access
              ? "Listed as open access, but no direct file was reported — open it to find one."
              : "No open-access copy was reported — open it to read it where it lives."}
          </span>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        {link !== null && <OpenLink link={link} title={title} />}
        {work.pdf_url !== null && (
          <AddButton
            title={title}
            tooltip={added ? "Added to this library" : addTitle}
            label="Add"
            onAdd={() => void addPaper(provider, work)}
            disabled={!canAdd || adding || added}
            adding={adding}
            added={added}
          />
        )}
      </div>
    </div>
  );
}
