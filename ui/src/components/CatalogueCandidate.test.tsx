import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueHit } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueAcquire: vi.fn(),
    openPath: vi.fn(() => Promise.resolve()),
    updateSettings: vi.fn(() => Promise.resolve({})),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: {
    type: "desktop",
    deletionKind: "trash",
    importFiles: vi.fn(() => Promise.resolve(["/library/book.pdf"])),
  },
}));

import { api, source } from "../services";
import CatalogueCandidate from "./CatalogueCandidate";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

const HIT: CatalogueHit = {
  provider: "libretexts",
  external_id: "42",
  title: "Combinatorial Optimization",
  summary: "Computational complexity and polynomial-time reductions.",
  subject: "Mathematics",
  authors: "Someone",
  license: "CC-BY",
  landing_url: "https://example.invalid/book",
  pdf_url: "https://example.invalid/book.pdf",
  outline_url: null,
  grain: "textbook",
  pages: 412,
  recall_score: 8.5,
  acquisition: "file",
};

/// An MIT OpenCourseWare course: no `pdf_url`, because a course is not served
/// whole at any URL, and `acquisition: "course"` saying so explicitly rather
/// than leaving the row to infer it from the provider id.
const COURSE: CatalogueHit = {
  ...HIT,
  provider: "mit_ocw",
  external_id: "3308",
  title: "Water Quality Control",
  landing_url: "https://ocw.mit.edu/courses/1-77-water-quality-spring-2006/",
  pdf_url: null,
  grain: "course",
  pages: null,
  acquisition: "course",
};

const acquire = api.catalogueAcquire as unknown as ReturnType<typeof vi.fn>;
const importFiles = (source as unknown as { importFiles: ReturnType<typeof vi.fn> })
  .importFiles;

describe("CatalogueCandidate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    acquire.mockResolvedValue({
      path: "/uploads/book.pdf",
      bytes: 1_024,
      already_present: false,
    });
    useCatalogueStore.getState().reset();
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
  });

  /// Grain and licence are what the thing is and what may be done with it. A
  /// row that hid either would be inviting a click it cannot describe.
  it("shows the grain, the provider and the licence", () => {
    render(<CatalogueCandidate hit={HIT} />);
    expect(screen.getByText("Textbook")).toBeTruthy();
    expect(screen.getByText("LibreTexts")).toBeTruthy();
    expect(screen.getByText("CC-BY")).toBeTruthy();
  });

  /// The fetch lands in Wilkes's own uploads directory; the import into the
  /// user's root is a second, separate step.
  it("fetches into uploads and then imports into the current directory", async () => {
    render(<CatalogueCandidate hit={HIT} />);
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    await waitFor(() => {
      expect(acquire).toHaveBeenCalledWith("https://example.invalid/book.pdf");
      expect(importFiles).toHaveBeenCalledWith(["/uploads/book.pdf"], "/library", "move");
    });
    expect(await screen.findByText("Added")).toBeTruthy();
  });

  /// Discovery and admission are not the same thing, and a button that fails
  /// on click is worse than no button.
  it("offers no Add for a record nothing can fetch", () => {
    render(<CatalogueCandidate hit={{ ...HIT, pdf_url: null, acquisition: "none" }} />);
    expect(screen.queryByLabelText(/Add Combinatorial Optimization/)).toBeNull();
    expect(screen.getByText(/does not publish a downloadable copy/i)).toBeTruthy();
    // The landing page is still offered: it is discoverable, just not fetchable.
    expect(screen.getByLabelText(/Open Combinatorial Optimization/)).toBeTruthy();
  });

  /// A course has no `pdf_url` and is still acquirable — the old rule, which
  /// read acquirability off `pdf_url`, would have hidden the button entirely.
  it("offers a course its own button and says what adding one does", () => {
    render(<CatalogueCandidate hit={COURSE} />);
    expect(screen.getByLabelText(/Add Water Quality Control to library/)).toBeTruthy();
    expect(screen.getByText(/Add course/)).toBeTruthy();
    expect(screen.getByText(/A course, not a file/i)).toBeTruthy();
  });

  /// Reading the manifest happens before a single document byte moves, and on
  /// a large course it is most of a minute. A row showing only the document
  /// counter would sit blank through all of it.
  it("counts the manifest walk and then the documents", () => {
    render(<CatalogueCandidate hit={COURSE} />);
    act(() => {
      useCatalogueStore.setState({ acquiring: "mit_ocw:3308" });
      useCatalogueStore.getState().noteCourseProgress({
        course_url: COURSE.landing_url as string,
        stage: "manifest",
        done: 7,
        total: 31,
        current: "Lecture 1",
      });
    });
    expect(screen.getByText(/Reading the course — 7 of 31/)).toBeTruthy();
    act(() => {
      useCatalogueStore.getState().noteCourseProgress({
        course_url: COURSE.landing_url as string,
        stage: "documents",
        done: 3,
        total: 20,
        current: "chapter1lecture.pdf",
      });
    });
    expect(screen.getByText(/Document 3 of 20/)).toBeTruthy();
  });

  /// A course arrives incomplete by design — its videos are not documents —
  /// so the row has to account for what is missing, not just say "added".
  it("reports what a finished course turned out to be", () => {
    render(<CatalogueCandidate hit={COURSE} />);
    act(() => {
      useCatalogueStore.setState({
        acquired: { "mit_ocw:3308": "/uploads/1-77-water-quality-spring-2006" },
        courses: {
          "mit_ocw:3308": {
            course_url: COURSE.landing_url as string,
            title: "Water Quality Control",
            directory: "/uploads/1-77-water-quality-spring-2006",
            document: "/uploads/1-77-water-quality-spring-2006/1-77.md",
            documents: [
              { filename: "a.pdf", path: "/u/a.pdf", bytes: 1, already_present: false },
              { filename: "b.pdf", path: "/u/b.pdf", bytes: 2, already_present: false },
            ],
            failures: [],
            skipped: [
              { title: "Lecture 1 video", reason: "audiovisual" },
              { title: "Lecture 2 video", reason: "audiovisual" },
            ],
          },
        },
      });
    });
    expect(screen.getByText(/2 documents and a syllabus, 2 skipped/)).toBeTruthy();
  });

  /// The desktop webview ignores a plain `target="_blank"`, so the open action
  /// has to go through the same bridge the rest of the app opens links with.
  it("opens the landing page through the application's opener", () => {
    render(<CatalogueCandidate hit={HIT} />);
    fireEvent.click(screen.getByLabelText(/Open Combinatorial Optimization/));
    expect(api.openPath).toHaveBeenCalledWith("https://example.invalid/book");
  });

  it("cannot add without a directory to add to", () => {
    useSettingsStore.setState({ directory: "" });
    render(<CatalogueCandidate hit={HIT} />);
    const button = screen.getByLabelText(/Add Combinatorial Optimization/) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  /// A textbook is tens of megabytes over a link nobody controls; between the
  /// click and the file appearing there has to be something other than "…".
  it("shows the bytes as they arrive, against the total when there is one", () => {
    render(<CatalogueCandidate hit={HIT} />);
    act(() => {
      useCatalogueStore.setState({ acquiring: "libretexts:42" });
      useCatalogueStore.getState().noteDownloadProgress({
        url: HIT.pdf_url as string,
        filename: "book.pdf",
        received_bytes: 1_048_576,
        total_bytes: 4_194_304,
        done: false,
      });
    });
    expect(screen.getByText("1.0 MB of 4.0 MB")).toBeTruthy();
    const bar = screen.getByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("1048576");
    expect(bar.getAttribute("aria-valuemax")).toBe("4194304");
  });

  /// A chunked response declares no length. A bar would sit at zero forever,
  /// so there is a figure and no bar rather than an invented denominator.
  it("shows what has arrived, and no bar, when the server declared no length", () => {
    render(<CatalogueCandidate hit={HIT} />);
    act(() => {
      useCatalogueStore.setState({ acquiring: "libretexts:42" });
      useCatalogueStore.getState().noteDownloadProgress({
        url: HIT.pdf_url as string,
        filename: "book.pdf",
        received_bytes: 524_288,
        total_bytes: null,
        done: false,
      });
    });
    expect(screen.getByText("512 KB so far")).toBeTruthy();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  /// Two rows can be added at once; neither may render the other's bytes.
  it("ignores progress belonging to another download", () => {
    render(<CatalogueCandidate hit={HIT} />);
    act(() => {
      useCatalogueStore.setState({ acquiring: "libretexts:42" });
      useCatalogueStore.getState().noteDownloadProgress({
        url: "https://example.invalid/other.pdf",
        filename: "other.pdf",
        received_bytes: 999_999,
        total_bytes: 1_000_000,
        done: false,
      });
    });
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.queryByText(/so far/)).toBeNull();
  });

  it("stops reporting bytes once the download is over", async () => {
    render(<CatalogueCandidate hit={HIT} />);
    act(() => {
      useCatalogueStore.getState().noteDownloadProgress({
        url: HIT.pdf_url as string,
        filename: "book.pdf",
        received_bytes: 4_194_304,
        total_bytes: 4_194_304,
        done: true,
      });
    });
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    await screen.findByText("Added");
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});
