import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueHit, LiteratureSearchResult } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueSearch: vi.fn(),
    catalogueAcquire: vi.fn(),
    literatureSearch: vi.fn(),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: {
    type: "desktop",
    deletionKind: "trash",
    importFiles: vi.fn(() => Promise.resolve([])),
  },
}));

import { api, source } from "../services";
import CataloguePane from "./CataloguePane";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useWorkspaceStore } from "../stores/useWorkspaceStore";

const HIT: CatalogueHit = {
  provider: "devdocs",
  external_id: "python~3.12",
  title: "Python 3.12",
  summary: "The language reference and standard library.",
  subject: "Programming",
  authors: "PSF",
  license: "PSF",
  landing_url: "https://example.invalid/python",
  pdf_url: null,
  outline_url: null,
  grain: "reference",
  pages: null,
  recall_score: 6.1,
  acquisition: "none",
};

const WORK: LiteratureSearchResult = {
  id: "W1",
  doi: "10.1000/lists",
  title: "Persistent Lists Revisited",
  year: 2021,
  publication_date: null,
  venue: "Journal of Functional Programming",
  citation_count: 12,
  is_open_access: true,
  pdf_url: "https://example.invalid/lists.pdf",
  landing_page_url: null,
  open_access_status: "gold",
  license: "cc-by",
};

const search = api.catalogueSearch as unknown as ReturnType<typeof vi.fn>;
const literature = api.literatureSearch as unknown as ReturnType<typeof vi.fn>;
const acquire = api.catalogueAcquire as unknown as ReturnType<typeof vi.fn>;
const importFiles = (source as unknown as { importFiles: ReturnType<typeof vi.fn> }).importFiles;

describe("CataloguePane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useCatalogueStore.getState().reset();
    useCatalogueStore.setState({ paneOpen: true, kinds: [] });
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
    search.mockResolvedValue({
      results: [{ key: "pane", terms: ["python", "lists"], hits: [HIT] }],
    });
    literature.mockResolvedValue({
      query: "python lists",
      providers: [{ provider: "openalex", name: "OpenAlex", results: [WORK], error: null }],
    });
  });

  const submit = (text: string) => {
    fireEvent.change(screen.getByLabelText("Search catalogues and literature"), {
      target: { value: text },
    });
    fireEvent.submit(screen.getByLabelText("Search catalogues and literature"));
  };

  it("asks the mirror and the literature providers the same question", async () => {
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText("Python 3.12")).toBeTruthy();
    expect(await screen.findByText("Persistent Lists Revisited")).toBeTruthy();
    expect(search.mock.calls[0][0]).toEqual([
      { key: "pane", text: "python lists", grains: null },
    ]);
    expect(literature).toHaveBeenCalledWith("python lists");
    // Listed apart, under headings, because they are different answers.
    expect(within(screen.getByRole("region", { name: "Open catalogues" })).getByText("Python 3.12")).toBeTruthy();
    expect(
      within(screen.getByRole("region", { name: "Papers" })).getByText("Persistent Lists Revisited"),
    ).toBeTruthy();
  });

  /// The catalogues publish at different grains and a question is rarely
  /// answerable at only one, so no selection means all of them — and a chosen
  /// grain is a filter the caller asked for, not a hint.
  it("passes only the grains the user selected, without asking the providers again", async () => {
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    fireEvent.click(screen.getByRole("button", { name: "Reference" }));
    await waitFor(() => {
      expect(search.mock.calls[1][0]).toEqual([
        { key: "pane", text: "python lists", grains: ["reference"] },
      ]);
    });
    // Reference alone excludes papers; nothing about the literature changed.
    expect(literature).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("Persistent Lists Revisited")).toBeNull();
  });

  it("searches only the literature when only papers are selected", async () => {
    useCatalogueStore.setState({ kinds: ["paper"] });
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText("Persistent Lists Revisited")).toBeTruthy();
    expect(search).not.toHaveBeenCalled();
    expect(screen.queryByRole("region", { name: "Open catalogues" })).toBeNull();
  });

  /// A provider that is down is reported beside the ones that answered, and
  /// does not take the catalogue results with it.
  it("reports a failing provider without hiding the others", async () => {
    literature.mockResolvedValue({
      query: "python lists",
      providers: [
        { provider: "semantic_scholar", name: "Semantic Scholar", results: null, error: "429 Too Many Requests" },
        { provider: "openalex", name: "OpenAlex", results: [WORK], error: null },
      ],
    });
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText(/Semantic Scholar could not answer: 429/)).toBeTruthy();
    expect(screen.getByText("Persistent Lists Revisited")).toBeTruthy();
    expect(screen.getByText("Python 3.12")).toBeTruthy();
  });

  /// No provider enabled is a setting to change, not "no papers found".
  it("says when no literature provider is enabled", async () => {
    literature.mockResolvedValue({ query: "python lists", providers: [] });
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText(/No literature provider is enabled/)).toBeTruthy();
  });

  it("adds a paper through uploads and into the library", async () => {
    acquire.mockResolvedValue({ path: "/uploads/lists.pdf", bytes: 10, already_present: false });
    render(<CataloguePane />);
    submit("python lists");
    fireEvent.click(await screen.findByLabelText("Add Persistent Lists Revisited to library"));
    await waitFor(() => {
      expect(acquire).toHaveBeenCalledWith("https://example.invalid/lists.pdf");
      expect(importFiles).toHaveBeenCalledWith(["/uploads/lists.pdf"], "/library", "move", undefined);
    });
    expect(await screen.findByText("Added")).toBeTruthy();
  });

  /// Two empty answers with different causes. Telling someone "nothing found"
  /// for a word that was never looked for would be a lie by omission.
  it("separates an unsearchable query from an empty catalogue", async () => {
    search.mockResolvedValue({ results: [{ key: "pane", terms: [], hits: [] }] });
    render(<CataloguePane />);
    submit("C");
    expect(
      await screen.findByText(/single letters and very common words are dropped/i),
    ).toBeTruthy();

    search.mockResolvedValue({
      results: [{ key: "pane", terms: ["topology"], hits: [] }],
    });
    submit("topology");
    expect(await screen.findByText(/No catalogue here holds anything matching/i)).toBeTruthy();
  });

  it("says why nothing can be added to a read-only workspace", async () => {
    useWorkspaceStore.setState({
      workspaces: [{ id: "w", name: "W", roots: [], active_root: null, read_only: true }],
      activeWorkspaceId: "w",
    } as never);
    render(<CataloguePane />);
    expect(screen.getByText(/read-only/i)).toBeTruthy();
  });
});
