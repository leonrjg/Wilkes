import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueHit, LiteratureSearchResult } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueSearch: vi.fn(),
    catalogueAcquire: vi.fn(),
    literatureProviders: vi.fn(),
    literatureSearch: vi.fn(),
    literatureResolveDownload: vi.fn(),
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
  authors: null,
  publisher: null,
  language: null,
  file_format: null,
  file_size: null,
  acquisition: "direct",
};

const search = api.catalogueSearch as unknown as ReturnType<typeof vi.fn>;
const providers = api.literatureProviders as unknown as ReturnType<typeof vi.fn>;
const literature = api.literatureSearch as unknown as ReturnType<typeof vi.fn>;
const resolveDownload = api.literatureResolveDownload as unknown as ReturnType<typeof vi.fn>;
const acquire = api.catalogueAcquire as unknown as ReturnType<typeof vi.fn>;
const importFiles = (source as unknown as { importFiles: ReturnType<typeof vi.fn> }).importFiles;

describe("CataloguePane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    useCatalogueStore.getState().reset();
    // The filters outlive a reset by design, so a test that means "nothing
    // filtered" says so rather than assuming the last test left it that way.
    useCatalogueStore.setState({
      paneOpen: true,
      grains: null,
      providers: null,
      providerOptions: [],
      providerOptionsError: null,
    });
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
    providers.mockResolvedValue([
      { provider: "semantic_scholar", name: "Semantic Scholar", enabled: true },
      { provider: "openalex", name: "OpenAlex", enabled: true },
    ]);
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
    // Ten records, not the backend's own default: the pane is a shortlist to
    // consider, not a second page of search results.
    expect(search.mock.calls[0][1]).toBe(10);
    expect(literature).toHaveBeenCalledWith("python lists", undefined, undefined);
    // Listed apart, under headings, because they are different answers. The
    // live sources are "Literature", not "Papers": a provider may index a
    // monograph or a standard, and several do.
    expect(
      within(screen.getByRole("region", { name: "Open catalogues" })).getByText("Python 3.12"),
    ).toBeTruthy();
    expect(
      within(screen.getByRole("region", { name: "Literature" })).getByText(
        "Persistent Lists Revisited",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Papers" })).toBeNull();
  });

  /// The catalogues publish at different grains and a question is rarely
  /// answerable at only one, so no selection means all of them — and a chosen
  /// grain is a filter the caller asked for, not a hint.
  it("passes only the grains the user selected, without asking the providers again", async () => {
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    // Everything is selected, so clicking a grain turns that one off.
    fireEvent.click(screen.getByRole("button", { name: "Textbooks" }));
    await waitFor(() => {
      expect(search.mock.calls[1][0]).toEqual([
        { key: "pane", text: "python lists", grains: ["course", "reference"] },
      ]);
    });
    // A grain chip is the mirror's business. Nothing about the live sources
    // changed, so they are not asked again.
    expect(literature).toHaveBeenCalledTimes(1);
    expect(screen.getByText("Persistent Lists Revisited")).toBeTruthy();
  });

  /// The grain filter belongs to the catalogues and is drawn inside their
  /// section, because it says nothing a literature provider could act on.
  it("keeps each filter inside the section it filters", async () => {
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "OpenAlex" });
    const catalogues = within(screen.getByRole("region", { name: "Open catalogues" }));
    expect(catalogues.getByRole("button", { name: "Textbooks" })).toBeTruthy();
    expect(catalogues.queryByRole("button", { name: "OpenAlex" })).toBeNull();
    const live = within(screen.getByRole("region", { name: "Literature" }));
    expect(live.getByRole("button", { name: "Semantic Scholar" })).toBeTruthy();
    expect(live.queryByRole("button", { name: "Textbooks" })).toBeNull();
  });

  /// Narrowing to one provider asks that provider alone. Filtering the answer
  /// afterwards would have spent the request on a rate-limited service for
  /// results it then threw away.
  it("asks only the selected providers and remembers the choice", async () => {
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "OpenAlex" });
    fireEvent.click(screen.getByRole("button", { name: "Semantic Scholar" }));
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    expect(literature).toHaveBeenCalledWith("python lists", undefined, ["openalex"]);
    expect(JSON.parse(localStorage.getItem("wilkes.catalogue.filters") ?? "null")).toEqual({
      grains: null,
      providers: ["openalex"],
    });
  });

  /// Turning a provider back off is free: its siblings' answers are held and
  /// the row is simply no longer listed.
  it("narrows without asking anything again", async () => {
    literature.mockResolvedValue({
      query: "python lists",
      providers: [
        { provider: "semantic_scholar", name: "Semantic Scholar", results: [], error: null },
        { provider: "openalex", name: "OpenAlex", results: [WORK], error: null },
      ],
    });
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "OpenAlex" });
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    fireEvent.click(screen.getByRole("button", { name: "OpenAlex" }));
    await waitFor(() => expect(screen.queryByText("Persistent Lists Revisited")).toBeNull());
    expect(literature).toHaveBeenCalledTimes(1);
  });

  /// Re-selecting a provider asks that provider, and only that provider: the
  /// siblings already answered this question.
  it("asks only the provider a widened filter adds", async () => {
    useCatalogueStore.setState({ providers: ["openalex"] });
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "Semantic Scholar" });
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    literature.mockResolvedValue({
      query: "python lists",
      providers: [
        { provider: "semantic_scholar", name: "Semantic Scholar", results: [], error: null },
      ],
    });
    fireEvent.click(screen.getByRole("button", { name: "Semantic Scholar" }));
    await waitFor(() => {
      expect(literature).toHaveBeenLastCalledWith("python lists", undefined, [
        "semantic_scholar",
      ]);
    });
    // The first provider's answer survived the second request.
    expect(screen.getByText("Persistent Lists Revisited")).toBeTruthy();
    expect(screen.getByText(/Semantic Scholar returned nothing/)).toBeTruthy();
  });

  /// A widening that fails must not take the results the user is already
  /// reading with it.
  it("keeps the answers it holds when a widening request fails", async () => {
    useCatalogueStore.setState({ providers: ["openalex"] });
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "Semantic Scholar" });
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    literature.mockRejectedValue(new Error("network is down"));
    fireEvent.click(screen.getByRole("button", { name: "Semantic Scholar" }));
    expect(await screen.findByText(/network is down/)).toBeTruthy();
    expect(screen.getByText("Persistent Lists Revisited")).toBeTruthy();
  });

  /// Deselecting every provider is a thing a user is allowed to mean. It is
  /// not "ask them all", and it is not a search.
  it("asks nobody when no provider is selected", async () => {
    useCatalogueStore.setState({ providers: [] });
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText("Python 3.12")).toBeTruthy();
    expect(literature).not.toHaveBeenCalled();
    expect(screen.getByText(/No provider selected/)).toBeTruthy();
  });

  /// A stored filter naming a provider the user has since removed would be
  /// refused by the backend on every search, so it is dropped on the way in.
  it("drops a stored provider that no longer exists", async () => {
    useCatalogueStore.setState({ providers: ["openalex", "custom:gone"] });
    render(<CataloguePane />);
    await waitFor(() => {
      expect(useCatalogueStore.getState().providers).toEqual(["openalex"]);
    });
  });

  /// The filter is offered from the registry, so a failure to read it is said
  /// rather than shown as an empty row that reads like "none configured".
  it("says when the provider list could not be read", async () => {
    providers.mockRejectedValue(new Error("no backend"));
    render(<CataloguePane />);
    expect(await screen.findByText(/providers could not be listed/)).toBeTruthy();
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

  /// No provider enabled is a setting to change, not "nothing found".
  it("says when no literature provider is enabled", async () => {
    providers.mockResolvedValue([]);
    literature.mockResolvedValue({ query: "python lists", providers: [] });
    render(<CataloguePane />);
    submit("python lists");
    expect(await screen.findByText(/No provider is enabled/)).toBeTruthy();
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

  it("resolves a provider download only after selection and preserves its filename", async () => {
    const providerWork: LiteratureSearchResult = {
      ...WORK,
      id: "A1",
      title: "Resolved Book",
      pdf_url: null,
      acquisition: "provider",
      language: "English",
      file_format: "epub",
      file_size: "2 MB",
    };
    literature.mockResolvedValue({
      query: "resolved book",
      providers: [{ provider: "anna", name: "Anna", results: [providerWork], error: null }],
    });
    resolveDownload.mockResolvedValue({
      url: "https://files.example.invalid/resolved",
      filename: "Resolved Book.epub",
    });
    acquire.mockResolvedValue({
      path: "/uploads/Resolved Book.epub",
      bytes: 10,
      already_present: false,
    });

    render(<CataloguePane />);
    submit("resolved book");
    expect(await screen.findByText("English")).toBeTruthy();
    expect(screen.getByText("epub")).toBeTruthy();
    expect(screen.getByText("2 MB")).toBeTruthy();
    expect(resolveDownload).not.toHaveBeenCalled();

    fireEvent.click(screen.getByLabelText("Add Resolved Book to library"));
    await waitFor(() => {
      expect(resolveDownload).toHaveBeenCalledWith("anna", providerWork);
      expect(acquire).toHaveBeenCalledWith(
        "https://files.example.invalid/resolved",
        "Resolved Book.epub",
      );
      expect(importFiles).toHaveBeenCalledWith(
        ["/uploads/Resolved Book.epub"],
        "/library",
        "move",
        undefined,
      );
    });
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
