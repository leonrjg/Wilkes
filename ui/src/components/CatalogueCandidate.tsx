import { Check, Download, ExternalLink } from "react-feather";
import type {
  CatalogueCourseProgress,
  CatalogueDownloadProgress,
  LiteratureSearchResult,
} from "../lib/types";
import { api } from "../services";
import { useCatalogueAdd } from "../hooks/useCatalogueAdd";
import { type AddStage, paperKey, useCatalogueStore } from "../stores/useCatalogueStore";
import { Tooltip } from "@leonrjg/wilkes-reader";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** What a learning catalogue calls the kind of thing a result is. A kind this
 *  build has no name for is shown as the source spelled it. */
const KIND_LABELS: Record<string, string> = {
  textbook: "Textbook",
  course: "Course",
  reference: "Reference",
};

/**
 * A row's facts, as one run of text rather than a row of chips.
 *
 * Each fact used to be its own flex item, and a flex item cannot be broken
 * across lines: three facts that did not fit side by side took three lines
 * even when two of them were one word. Joined, the line fills to the edge and
 * wraps where the text allows, which is the difference between a two-line row
 * and a four-line one. Clamped at two lines, with the whole thing on hover,
 * because these are facts to skim past, not to read.
 */
function MetaLine({ parts }: { parts: (string | null | undefined | false)[] }) {
  const shown = parts.filter((part): part is string => typeof part === "string" && part !== "");
  if (shown.length === 0) return null;
  const text = shown.join(" · ");
  return (
    <p
      title={text}
      className="line-clamp-2 text-[10px] leading-snug text-[var(--text-dim)]"
    >
      {text}
    </p>
  );
}

/** A title long enough to take four lines is a title nobody reads to the end
 *  of in a pane this wide. Two lines, and the rest on hover. */
function RowTitle({ title }: { title: string }) {
  return (
    <span
      title={title}
      className="line-clamp-2 text-[11px] font-medium leading-snug text-[var(--text-main)]"
    >
      {title}
    </span>
  );
}

interface Props {
  /** The provider that returned the result, as the registry names it. */
  provider: string;
  work: LiteratureSearchResult;
  /** Compact drops the blurb: the strip under a search result is an offer, not
   *  a reading list, and four two-line rows beat two paragraphs. */
  compact?: boolean;
  /** The provider's name, for a list that mixes providers and so has no
   *  heading to say whose row this is. */
  source?: string;
}

/**
 * One result a provider returned — a paper, a textbook, a course — shared by
 * the pane and the search-result strip so that it looks and behaves the same
 * wherever it is offered.
 *
 * One row for all of them, because they are one type: a result carries the
 * facts its source knows and the row shows the ones it carries. A paper has a
 * year, a venue and a citation count; a textbook has a kind, a subject, a
 * licence and a blurb; neither shows a blank for what the other has.
 */
export default function CatalogueCandidate({ provider, work, compact = false, source }: Props) {
  const { add, canAdd, needsDirectory, readOnly, stageOf, isAdded } = useCatalogueAdd();
  const stage = stageOf(provider, work);
  const adding = stage !== undefined;
  // Staged is not added. `acquired` is written when the file lands in uploads,
  // which is before it is moved into the library, so a row that read it alone
  // said "Added" — and offered no way to see the move — while the move was
  // still running.
  const added = !adding && isAdded(provider, work);
  // Keyed by the URL this row asked for, so two rows added at once do not
  // read each other's bytes.
  const download = useCatalogueStore((s) =>
    work.pdf_url === null ? undefined : s.downloads[work.pdf_url],
  );
  // A course reports its own two-stage progress; the byte stream above cannot
  // say which of forty documents it belongs to.
  const courseProgress = useCatalogueStore((s) =>
    work.landing_page_url === null ? undefined : s.courseProgress[work.landing_page_url],
  );
  const course = useCatalogueStore((s) => s.courses[paperKey(provider, work)]);
  // A provider may return a record with no title. It is still a record the
  // user can open, so it is named by what identifies it rather than dropped.
  const title = work.title ?? (work.doi !== null ? `DOI ${work.doi}` : work.id);
  // A user-described provider may report the DOI already as a resolver URL.
  const doiLink =
    work.doi === null ? null : /^https?:\/\//i.test(work.doi) ? work.doi : `https://doi.org/${work.doi}`;
  const link = work.landing_page_url ?? doiLink;
  // What adding would do was decided by the source that returned the result.
  // A result nothing can fetch says so rather than offering a button that
  // would fail on click.
  const acquisition = work.acquisition ?? (work.pdf_url === null ? "none" : "direct");
  const isCourse = acquisition === "course";
  const acquirable = acquisition !== "none" && !(isCourse && work.landing_page_url === null);

  const addTitle = readOnly
    ? "This workspace is read-only"
    : needsDirectory
      ? "Choose a directory before adding documents"
      : isCourse
        ? "Fetch every document this course publishes, with a generated syllabus, into the current directory"
        : "Fetch this document and add it to the current directory";

  return (
    /* The buttons share a line with the title and nothing else. They used to
       stand beside the whole text column, so every line under the title — the
       facts, the blurb, the progress bar — was laid out in the width left over
       beside them: 186px of a 316px pane, and the blurb wrapped at 59% of the
       space it had. Below the title line, the row is the pane's full width. */
    <div className="flex flex-col gap-0.5 py-1.5">
      <div className="flex items-start justify-between gap-2">
        <div className="flex min-w-0 flex-1 items-start gap-1.5">
          <RowTitle title={title} />
          {/* Only where the source says what kind of thing it returned. A
              bibliographic index names none, and a badge reading "Paper" over
              its monographs, standards and theses would be wrong for some of
              its rows. */}
          {work.kind !== null && (
            <span className="mt-px shrink-0 rounded border border-[var(--border-main)] bg-[var(--bg-app)] px-1 text-[9px] uppercase tracking-wider text-[var(--text-dim)]">
              {KIND_LABELS[work.kind] ?? work.kind}
            </span>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-1">
          {link !== null && <OpenLink link={link} title={title} />}
          {acquirable && (
            <AddButton
              title={title}
              tooltip={added ? "Added to this library" : addTitle}
              label="Add"
              onAdd={() => void add(provider, work)}
              disabled={!canAdd || adding || added}
              adding={adding}
              added={added}
            />
          )}
        </div>
      </div>

      {/* What a person chooses by comes first — when, how cited, whether it
          can be had — because the line is clamped at two lines and a long
          author list used to push the citations and the access out of view. A
          count the provider did not report is left out rather than shown as
          zero, and "open access" is left to the licence where the whole source
          is an open catalogue. */}
      <MetaLine
        parts={[
          source,
          work.year !== null && `${work.year}`,
          work.citation_count !== null &&
            `${work.citation_count.toLocaleString()} citation${work.citation_count === 1 ? "" : "s"}`,
          work.is_open_access && work.kind === null && "Open access",
          work.subject,
          work.authors,
          work.venue,
          work.publisher !== work.venue && work.publisher,
          work.language,
          work.file_format,
          work.file_size,
          work.license,
          work.pages !== null && `${work.pages.toLocaleString()} pp`,
        ]}
      />
      {!compact && work.summary !== null && (
        <p className="line-clamp-2 text-[10px] leading-snug text-[var(--text-muted)]">
          {work.summary}
        </p>
      )}
      {!compact && <ResultFilters provider={provider} work={work} title={title} />}
      {stage !== undefined && (
        <AddProgress
          stage={stage}
          title={title}
          download={download}
          course={isCourse ? courseProgress : undefined}
        />
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
      {/* Why there is no add button, in the space a button would have
          taken. What adding a course *does* is the button's own tooltip:
          a paragraph of it under every course row was the same sentence
          repeated down the pane. */}
      {!acquirable && (
        <span className="text-[10px] leading-snug text-[var(--text-dim)]">
          {work.kind !== null
            ? "No downloadable copy — open it to read it where it lives."
            : work.is_open_access
              ? "Listed as open access, but no file was reported — open it to find one."
              : "No open-access copy reported — open it to read it where it lives."}
        </span>
      )}
    </div>
  );
}

/** What each stage of an add is called, in the order they happen. */
const STAGE_LABELS: Record<AddStage, string> = {
  resolving: "Finding a copy…",
  fetching: "Fetching…",
  importing: "Adding to the library…",
};

/**
 * One row's add, reported from the click until the file is in the library.
 *
 * The byte counter used to be the whole of this, so a row said nothing while
 * a provider was being asked where the file was, nothing between the request
 * going out and the first byte arriving, and nothing while the fetched file
 * was moved into the root — and that last silence flipped the button back to
 * an enabled "Add" for as long as the move took.
 *
 * Where there is a denominator the bar shows the fraction. Where there is not
 * — every stage but one, and a chunked response even in that one — it shows
 * that work is happening and does not pretend to know how much is left.
 */
function AddProgress({
  stage,
  title,
  download,
  course,
}: {
  stage: AddStage;
  title: string;
  download?: CatalogueDownloadProgress;
  /** A course reports its own two phases; the byte stream cannot say which of
   *  forty documents it belongs to. */
  course?: CatalogueCourseProgress;
}) {
  if (stage === "fetching" && course !== undefined) {
    return <CourseProgress course={course} title={title} />;
  }
  if (stage === "fetching" && download !== undefined) {
    return <DownloadProgress download={download} title={title} />;
  }
  return <Working label={STAGE_LABELS[stage]} title={title} />;
}

/** A stage that is under way with no way to say how far. The bar carries no
 *  `aria-valuenow`, which is how a progress bar says it is indeterminate. */
function Working({ label, title }: { label: string; title: string }) {
  return (
    <div className="flex flex-col gap-1 pt-0.5">
      <span className="text-[10px] leading-snug text-[var(--text-dim)]">{label}</span>
      <div
        role="progressbar"
        aria-label={`Adding ${title}`}
        className="animate-shimmer h-0.5 w-full overflow-hidden rounded bg-[var(--bg-active)]"
      />
    </div>
  );
}

function CourseProgress({
  course,
  title,
}: {
  course: CatalogueCourseProgress;
  title: string;
}) {
  const label =
    course.stage === "manifest"
      ? `Reading the course${course.total !== null ? ` — ${course.done} of ${course.total}` : ""}`
      : `Document ${course.done}${course.total !== null ? ` of ${course.total}` : ""}`;
  if (course.total === null || course.total <= 0) {
    return <Working label={label} title={title} />;
  }
  return (
    <div className="flex flex-col gap-1 pt-0.5">
      <span className="text-[10px] leading-snug text-[var(--text-dim)]">{label}</span>
      <div
        role="progressbar"
        aria-label={`Fetching ${title}`}
        aria-valuemin={0}
        aria-valuemax={course.total}
        aria-valuenow={course.done}
        className="h-0.5 w-full overflow-hidden rounded bg-[var(--bg-app)]"
      >
        <div
          className="h-full bg-[var(--accent-blue)] transition-[width] duration-200"
          style={{ width: `${Math.min(100, Math.round((course.done / course.total) * 100))}%` }}
        />
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
  /* A chunked response has no length, so there is a figure but no fraction.
     Saying how much has arrived is still more than an ellipsis says, and the
     bar below it moves without claiming to know the end. */
  if (download.total_bytes === null || download.total_bytes <= 0) {
    return <Working label={`${formatBytes(download.received_bytes)} so far`} title={title} />;
  }
  return (
    <div className="flex flex-col gap-1 pt-0.5">
      <span className="text-[10px] leading-snug text-[var(--text-dim)]">
        {formatBytes(download.received_bytes)} of {formatBytes(download.total_bytes)}
      </span>
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
          /* "Adding", not "Downloading": the button is busy for the resolve
             and the move as well, and only the middle of that is a download. */
          <span
            aria-label="Adding"
            className="h-[11px] w-[11px] animate-spin rounded-full border border-current border-t-transparent"
          />
        ) : (
          <>
            <Download size={11} /> {label}
          </>
        )}
      </button>
    </Tooltip>
  );
}

/**
 * The filters a provider can set from this row: "works citing this one".
 *
 * A provider declares them (`from_result` names the result field that supplies
 * the value), so the row knows nothing about citations or about any provider;
 * it offers whatever was declared, for a row that holds the field. Using one
 * sets that filter to this row and lists the provider by it, keeping the
 * row's title as what the filter's chip says.
 */
function ResultFilters({
  provider,
  work,
  title,
}: {
  provider: string;
  work: LiteratureSearchResult;
  title: string;
}) {
  const filters = useCatalogueStore(
    (s) =>
      s.providerOptions.find((option) => option.provider === provider)?.capabilities.browse
        ?.filters,
  );
  const setFilter = useCatalogueStore((s) => s.setBrowseFilter);
  const refine = useCatalogueStore((s) => s.refine);
  const offers = (filters ?? []).flatMap((filter) => {
    if (filter.from_result === null) return [];
    const value = (work as unknown as Record<string, unknown>)[filter.from_result];
    return typeof value === "string" && value !== "" ? [{ filter, value }] : [];
  });
  if (offers.length === 0) return null;
  return (
    <p className="flex flex-wrap gap-x-2 text-[10px] leading-snug">
      {offers.map(({ filter, value }) => (
        <button
          key={filter.name}
          type="button"
          onClick={() => {
            setFilter(provider, filter.name, [value], { [value]: title });
            void refine(useCatalogueStore.getState().query, provider);
          }}
          aria-label={`${filter.label} ${title}`}
          className="text-[var(--text-dim)] underline-offset-2 transition-colors hover:text-[var(--accent-blue)] hover:underline"
        >
          {filter.label}
        </button>
      ))}
    </p>
  );
}
