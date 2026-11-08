import { act, fireEvent, render as renderAlone, screen, waitFor, within } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LiteratureSearchResult } from "../lib/types";

vi.mock("./Toast", () => ({ useToasts: () => ({ addToast: vi.fn() }) }));

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    listSavedWorks: vi.fn(() => Promise.resolve([])),
    saveWork: vi.fn(),
    removeSavedWork: vi.fn(() => Promise.resolve()),
    markSavedWorkAcquired: vi.fn(() => Promise.resolve(null)),
    catalogueAcquire: vi.fn(),
    catalogueAcquireCourse: vi.fn(),
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
import {
  CatalogueSearchContext,
  createCatalogueSearch,
  useCatalogueStore,
} from "../stores/useCatalogueStore";
import { useSavedWorksStore } from "../stores/useSavedWorksStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

/** A row is rendered in a pane, whose search its filters refine. */
function InPane({ children }: { children: ReactNode }) {
  return (
    <CatalogueSearchContext.Provider value={createCatalogueSearch(null)}>
      {children}
    </CatalogueSearchContext.Provider>
  );
}

const render = (ui: ReactElement) => renderAlone(ui, { wrapper: InPane });

/// A textbook as a learning catalogue returns it: the result every provider
/// answers in, carrying what a catalogue knows.
const HIT: LiteratureSearchResult = {
  id: "42",
  doi: null,
  title: "Combinatorial Optimization",
  year: null,
  publication_date: null,
  venue: null,
  citation_count: null,
  is_open_access: true,
  pdf_url: "https://example.invalid/book.pdf",
  landing_page_url: "https://example.invalid/book",
  image_url: null,
  open_access_status: null,
  license: "CC-BY",
  authors: "Someone",
  publisher: null,
  language: null,
  file_format: null,
  file_size: null,
  summary: "Computational complexity and polynomial-time reductions.",
  subject: "Mathematics",
  kind: "textbook",
  pages: 412,
  recall_score: 8.5,
  acquisition: "direct",
};

/// An MIT OCW course: no `pdf_url`, because a course is not served
/// whole at any URL, and `acquisition: "course"` saying so explicitly rather
/// than leaving the row to infer it from the provider id.
const COURSE: LiteratureSearchResult = {
  ...HIT,
  id: "3308",
  title: "Water Quality Control",
  landing_page_url: "https://ocw.mit.edu/courses/1-77-water-quality-spring-2006/",
  image_url: null,
  pdf_url: null,
  kind: "course",
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
    useSavedWorksStore.setState({ works: [] });
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
  });

  /// Grain and licence are what the thing is and what may be done with it. A
  /// row that hid either would be inviting a click it cannot describe.
  /// The grain keeps its own chip — it is what the thing *is*. The rest are
  /// one run of text rather than a chip each, because a chip cannot be broken
  /// across lines and three of them took three lines in a pane this narrow.
  it("shows the kind, the subject and the licence", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    expect(screen.getByText("Textbook")).toBeTruthy();
    expect(screen.getByText(/CC-BY/)).toBeTruthy();
    // Under its provider's heading the row does not repeat whose it is, and an
    // open catalogue's licence says more than "open access" would.
    expect(screen.getByText("Mathematics · Someone · CC-BY · 412 pp")).toBeTruthy();
    expect(screen.getByText(HIT.summary as string)).toBeTruthy();
  });

  it("expands and collapses the full description independently for each result", () => {
    const description = "First paragraph.\n\n" + "A longer synopsis with more detail. ".repeat(30);
    render(<>
      <CatalogueCandidate provider="custom:kitsu" work={{ ...HIT, summary: description }} />
      <CatalogueCandidate provider="custom:kitsu" work={{ ...HIT, id: "other", title: "Other result" }} />
    </>);
    const expand = screen.getByRole("button", { name: `Expand description for ${HIT.title}` });
    const paragraph = screen.getByText(description, { normalizer: (text) => text });
    expect(expand.getAttribute("aria-expanded")).toBe("false");
    expect(expand.getAttribute("aria-controls")).toBe(paragraph.id);
    expect(paragraph.className).toContain("line-clamp-2");
    fireEvent.click(expand);
    const collapse = screen.getByRole("button", { name: `Collapse description for ${HIT.title}` });
    expect(collapse.getAttribute("aria-expanded")).toBe("true");
    expect(paragraph.className).not.toContain("line-clamp-2");
    expect(paragraph.className).toContain("whitespace-pre-wrap");
    expect(screen.getByRole("button", { name: "Expand description for Other result" }).getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(collapse);
    expect(paragraph.className).toContain("line-clamp-2");
  });

  it("offers no expansion for absent or blank descriptions or compact rows", () => {
    const { rerender } = render(<CatalogueCandidate provider="openalex" work={{ ...HIT, summary: null }} />);
    expect(screen.queryByRole("button", { name: /description for/ })).toBeNull();
    rerender(<CatalogueCandidate provider="openalex" work={{ ...HIT, summary: "   " }} />);
    expect(screen.queryByRole("button", { name: /description for/ })).toBeNull();
    rerender(<CatalogueCandidate provider="openalex" work={HIT} compact />);
    expect(screen.queryByRole("button", { name: /description for/ })).toBeNull();
  });

  it("shows an optional result image and removes it if loading fails", () => {
    render(
      <CatalogueCandidate
        provider="custom:kitsu"
        work={{ ...HIT, image_url: "https://media.example.invalid/cover.jpg" }}
      />,
    );
    const image = screen.getByRole("img", { name: `Image for ${HIT.title}` });
    expect(image.getAttribute("src")).toBe("https://media.example.invalid/cover.jpg");
    expect(image.getAttribute("loading")).toBe("lazy");
    expect(screen.getByText(HIT.summary as string)).toBeTruthy();
    fireEvent.error(image);
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByText(HIT.title as string)).toBeTruthy();
  });

  it("opens a result image as large as the window allows and closes it on Escape", () => {
    const url = "https://media.example.invalid/cover.jpg";
    render(<CatalogueCandidate provider="custom:kitsu" work={{ ...HIT, image_url: url }} />);
    fireEvent.click(screen.getByRole("button", { name: `Show image for ${HIT.title}` }));
    const dialog = screen.getByRole("dialog", { name: `Image for ${HIT.title}` });
    const large = within(dialog).getByRole("img", { name: `Image for ${HIT.title}` });
    expect(large.getAttribute("src")).toBe(url);
    expect(large.getAttribute("referrerpolicy")).toBe("no-referrer");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("keeps results without optional media and compact results free of image placeholders", () => {
    const { rerender } = render(
      <CatalogueCandidate provider="openalex" work={{ ...HIT, summary: null }} />,
    );
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.queryByText(HIT.summary as string)).toBeNull();
    rerender(
      <CatalogueCandidate
        provider="custom:kitsu"
        work={{ ...HIT, image_url: "https://media.example.invalid/cover.jpg" }}
        compact
      />,
    );
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.queryByText(HIT.summary as string)).toBeNull();
  });

  /// A list that mixes providers has no heading to say whose a row is, so the
  /// row says it; and an offer under a failed search is not a reading list.
  it("names its source and drops the blurb where it is one of a mixed strip", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} source="LibreTexts" compact />);
    expect(
      screen.getByText("LibreTexts · Mathematics · Someone · CC-BY · 412 pp"),
    ).toBeTruthy();
    expect(screen.queryByText(HIT.summary as string)).toBeNull();
  });

  /// A source that names no kind gets no badge: "Paper" over a provider's
  /// monographs and standards would be wrong for some of its rows.
  it("shows no kind for a result whose source named none", () => {
    render(
      <CatalogueCandidate
        provider="openalex"
        work={{ ...HIT, kind: null, year: 2021, citation_count: 3 }}
      />,
    );
    expect(screen.queryByText("Textbook")).toBeNull();
    expect(screen.getByText(/^2021 · 3 citations · Open access/)).toBeTruthy();
  });

  /// The fetch lands in Wilkes's own uploads directory; the import into the
  /// user's root is a second, separate step.
  it("fetches into uploads and then imports into the current directory", async () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    await waitFor(() => {
      expect(acquire).toHaveBeenCalledWith("https://example.invalid/book.pdf");
      // No folder for a single document: one file belongs in the root, and
      // wrapping every book in a directory of its own would be worse.
      expect(importFiles).toHaveBeenCalledWith(
        ["/uploads/book.pdf"],
        "/library",
        "move",
        undefined,
      );
    });
    expect(await screen.findByText("Added")).toBeTruthy();
  });

  /// The add says where the file landed, whether or not the result was saved:
  /// which saved work it is, if any, is the backend's to say, since it also
  /// knows one by its DOI.
  it("tells the saved works where an added result landed", async () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    await waitFor(() =>
      expect(api.markSavedWorkAcquired).toHaveBeenCalledWith(
        "libretexts",
        HIT,
        "/library/book.pdf",
      ),
    );
  });

  it("saves a result for later and lets it go again", async () => {
    const saved = {
      id: "s1",
      provider: "libretexts",
      work: HIT,
      note: null,
      saved_at: "2026-10-27T00:00:00Z",
      acquired: null,
    };
    (api.saveWork as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(saved);
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);

    fireEvent.click(screen.getByLabelText("Save Combinatorial Optimization"));
    await waitFor(() =>
      expect(api.saveWork).toHaveBeenCalledWith({ provider: "libretexts", work: HIT }),
    );
    const unsave = await screen.findByLabelText("Unsave Combinatorial Optimization");
    expect(unsave.getAttribute("aria-pressed")).toBe("true");

    fireEvent.click(unsave);
    await waitFor(() => expect(api.removeSavedWork).toHaveBeenCalledWith("s1"));
    expect(await screen.findByLabelText("Save Combinatorial Optimization")).toBeTruthy();
  });

  /// Another provider's record of the same DOI is the same work.
  it("shows a result saved from another provider as saved", () => {
    useSavedWorksStore.setState({
      works: [
        {
          id: "s1",
          provider: "openalex",
          work: { ...HIT, id: "W9", doi: "10.1000/ABC" },
          saved_at: "2026-10-27T00:00:00Z",
        },
      ],
    });
    render(<CatalogueCandidate provider="libretexts" work={{ ...HIT, doi: "10.1000/abc" }} />);
    expect(screen.getByLabelText("Unsave Combinatorial Optimization")).toBeTruthy();
  });

  /// A saved work added in an earlier session is still added in this one.
  it("shows a saved work recorded as added as added", () => {
    useSavedWorksStore.setState({
      works: [
        {
          id: "s1",
          provider: "libretexts",
          work: HIT,
          saved_at: "2026-10-27T00:00:00Z",
          acquired: { path: "/library/book.pdf", acquired_at: "2026-10-27T00:00:00Z" },
        },
      ],
    });
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    expect(screen.getByText("Added")).toBeTruthy();
  });

  /// Discovery and admission are not the same thing, and a button that fails
  /// on click is worse than no button.
  it("offers no Add for a record nothing can fetch", () => {
    render(<CatalogueCandidate provider="libretexts" work={{ ...HIT, pdf_url: null, acquisition: "none" }} />);
    expect(screen.queryByLabelText(/Add Combinatorial Optimization/)).toBeNull();
    expect(screen.getByText(/No downloadable copy/i)).toBeTruthy();
    // The landing page is still offered: it is discoverable, just not fetchable.
    expect(screen.getByLabelText(/Open Combinatorial Optimization/)).toBeTruthy();
  });

  /// A course has no `pdf_url` and is still acquirable — the old rule, which
  /// read acquirability off `pdf_url`, would have hidden the button entirely.
  /// The chip says it is a course and the tooltip says what fetching one
  /// does, so the button is labelled like every other and the row does not
  /// explain itself in a paragraph under every OCW result.
  it("offers a course the same Add as anything else", () => {
    render(<CatalogueCandidate provider="mit_ocw" work={COURSE} />);
    expect(screen.getByLabelText(/Add Water Quality Control to library/)).toBeTruthy();
    expect(screen.getByText("Course")).toBeTruthy();
    expect(screen.queryByText(/Add course/)).toBeNull();
    expect(screen.queryByText(/A course, not a file/i)).toBeNull();
  });

  /// Forty PDFs called `lecture5.pdf` and `ps1.pdf` loose in a library root
  /// are unattributable, and the second course to arrive collides with the
  /// first on exactly those names. A course imports as a folder named after
  /// itself — the same name it was staged under, so it does not change
  /// identity by being admitted.
  it("imports a course into a folder named after the course", async () => {
    const acquireCourse = api.catalogueAcquireCourse as unknown as ReturnType<typeof vi.fn>;
    acquireCourse.mockResolvedValue({
      course_url: COURSE.landing_page_url,
      title: "Water Quality Control",
      folder: "1.77 Water Quality Control (Spring 2006)",
      directory: "/uploads/1.77 Water Quality Control (Spring 2006)",
      document: "/uploads/1.77 Water Quality Control (Spring 2006)/1.77.md",
      documents: [
        {
          filename: "chapter1lecture.pdf",
          path: "/uploads/1.77 Water Quality Control (Spring 2006)/chapter1lecture.pdf",
          bytes: 10,
          already_present: false,
        },
      ],
      failures: [],
      skipped: [],
    });
    render(<CatalogueCandidate provider="mit_ocw" work={COURSE} />);
    fireEvent.click(screen.getByLabelText(/Add Water Quality Control to library/));
    await waitFor(() => {
      expect(importFiles).toHaveBeenCalledWith(
        [
          "/uploads/1.77 Water Quality Control (Spring 2006)/1.77.md",
          "/uploads/1.77 Water Quality Control (Spring 2006)/chapter1lecture.pdf",
        ],
        "/library",
        "move",
        "1.77 Water Quality Control (Spring 2006)",
      );
    });
  });

  /// Reading the manifest happens before a single document byte moves, and on
  /// a large course it is most of a minute. A row showing only the document
  /// counter would sit blank through all of it.
  it("counts the manifest walk and then the documents", () => {
    render(<CatalogueCandidate provider="mit_ocw" work={COURSE} />);
    act(() => {
      useCatalogueStore.setState({ adding: { "literature:mit_ocw:3308": "fetching" } });
      useCatalogueStore.getState().noteCourseProgress({
        course_url: COURSE.landing_page_url as string,
        stage: "manifest",
        done: 7,
        total: 31,
        current: "Lecture 1",
      });
    });
    expect(screen.getByText(/Reading the course — 7 of 31/)).toBeTruthy();
    act(() => {
      useCatalogueStore.getState().noteCourseProgress({
        course_url: COURSE.landing_page_url as string,
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
    render(<CatalogueCandidate provider="mit_ocw" work={COURSE} />);
    act(() => {
      useCatalogueStore.setState({
        acquired: { "literature:mit_ocw:3308": "/uploads/1-77-water-quality-spring-2006" },
        courses: {
          "literature:mit_ocw:3308": {
            course_url: COURSE.landing_page_url as string,
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
  /// goes where every other link does; outside the desktop app that is the
  /// system opener.
  it("opens the landing page through the application's link router", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    fireEvent.click(screen.getByLabelText(/Open Combinatorial Optimization/));
    expect(api.openPath).toHaveBeenCalledWith("https://example.invalid/book");
  });

  it("cannot add without a directory to add to", () => {
    useSettingsStore.setState({ directory: "" });
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    const button = screen.getByLabelText(/Add Combinatorial Optimization/) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  /// A textbook is tens of megabytes over a link nobody controls; between the
  /// click and the file appearing there has to be something other than "…".
  it("shows the bytes as they arrive, against the total when there is one", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    act(() => {
      useCatalogueStore.setState({ adding: { "literature:libretexts:42": "fetching" } });
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

  /// A chunked response declares no length. A filling bar would sit at zero
  /// forever, so what has arrived is stated and the bar says only that work is
  /// happening — indeterminate, which is what no `aria-valuenow` means.
  it("shows what has arrived, and no fraction, when the server declared no length", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    act(() => {
      useCatalogueStore.setState({ adding: { "literature:libretexts:42": "fetching" } });
      useCatalogueStore.getState().noteDownloadProgress({
        url: HIT.pdf_url as string,
        filename: "book.pdf",
        received_bytes: 524_288,
        total_bytes: null,
        done: false,
      });
    });
    expect(screen.getByText("512 KB so far")).toBeTruthy();
    const bar = screen.getByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBeNull();
    expect(bar.getAttribute("aria-valuemax")).toBeNull();
  });

  /// Two rows can be added at once; neither may render the other's bytes.
  it("ignores progress belonging to another download", () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    act(() => {
      useCatalogueStore.setState({ adding: { "literature:libretexts:42": "fetching" } });
      useCatalogueStore.getState().noteDownloadProgress({
        url: "https://example.invalid/other.pdf",
        filename: "other.pdf",
        received_bytes: 999_999,
        total_bytes: 1_000_000,
        done: false,
      });
    });
    expect(screen.queryByText(/so far/)).toBeNull();
    expect(screen.queryByText(/999/)).toBeNull();
    // It is still adding, so it says so — with no figures it does not have.
    expect(screen.getByText("Fetching…")).toBeTruthy();
  });

  /// The move out of uploads and into the library root is the last stage, and
  /// it used to be silent: the row dropped back to an enabled "Add" for as
  /// long as it took, inviting a second click on a file already downloaded.
  it("keeps reporting while the staged file is moved into the library", async () => {
    let finishImport = () => {};
    importFiles.mockImplementation(
      () => new Promise<string[]>((resolve) => (finishImport = () => resolve([]))),
    );
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    expect(await screen.findByText("Adding to the library…")).toBeTruthy();
    expect(screen.queryByText("Added")).toBeNull();
    await act(async () => {
      finishImport();
    });
    expect(await screen.findByText("Added")).toBeTruthy();
    expect(screen.queryByText("Adding to the library…")).toBeNull();
  });

  /// The stage is keyed per row like the byte counters are. It used to be a
  /// single key, so the second add to start erased the first row's indicator
  /// and the first to finish cleared the second's.
  it("keeps each row's progress its own when two are added at once", async () => {
    const other: LiteratureSearchResult = { ...HIT, id: "43", title: "Another Book" };
    // One resolver per call: two imports are in flight, and a single handle
    // would only ever release the second.
    const pendingImports: (() => void)[] = [];
    importFiles.mockImplementation(
      () => new Promise<string[]>((resolve) => pendingImports.push(() => resolve([]))),
    );
    render(
      <>
        <CatalogueCandidate provider="libretexts" work={HIT} />
        <CatalogueCandidate provider="libretexts" work={other} />
      </>,
    );
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    fireEvent.click(screen.getByLabelText(/Add Another Book/));
    await waitFor(() => {
      expect(screen.getAllByText("Adding to the library…")).toHaveLength(2);
    });
    expect(useCatalogueStore.getState().adding).toEqual({
      "literature:libretexts:42": "importing",
      "literature:libretexts:43": "importing",
    });
    await act(async () => {
      pendingImports.forEach((finish) => finish());
    });
    await waitFor(() => expect(useCatalogueStore.getState().adding).toEqual({}));
  });

  /// A move that fails is as much "it did not get added" as a fetch that
  /// fails, and used to reject into nothing at all.
  it("reports a failed import instead of rejecting silently", async () => {
    importFiles.mockRejectedValue(new Error("the root is not writable"));
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
    fireEvent.click(screen.getByLabelText(/Add Combinatorial Optimization/));
    await waitFor(() => {
      expect(useCatalogueStore.getState().acquireError).toMatch(/not writable/);
    });
    // And the row is not left spinning by the failure.
    expect(useCatalogueStore.getState().adding).toEqual({});
  });

  it("stops reporting bytes once the download is over", async () => {
    render(<CatalogueCandidate provider="libretexts" work={HIT} />);
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
