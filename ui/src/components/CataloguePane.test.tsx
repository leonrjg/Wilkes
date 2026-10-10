import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BrowseControls,
  FilterControl,
  CatalogueHit,
  LiteratureProviderInfo,
  LiteratureSearchResult,
} from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueSearch: vi.fn(),
    catalogueAcquire: vi.fn(),
    literatureProviders: vi.fn(),
    literatureSearch: vi.fn(),
    literatureBrowse: vi.fn(),
    literatureFilterSuggest: vi.fn(),
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

const control = (
  name: string,
  label: string,
  input: FilterControl["input"],
  rest: Partial<FilterControl> = {},
): FilterControl => ({
  name,
  label,
  group: null,
  input,
  options: [],
  modes: [],
  from_result: null,
  ...rest,
});

/** A small declaration with one filter of each plain kind, said the way the
 *  backend reports it. */
const OPENALEX_BROWSE: BrowseControls = {
  keyword: true,
  total: true,
  sort: {
    default: "cited_by_count:desc",
    keyword_default: "relevance_score:desc",
    options: [
      { value: "cited_by_count:desc", label: "Most cited" },
      { value: "publication_date:desc", label: "Newest first" },
    ],
  },
  modes: [
    { value: "any", label: "Any of" },
    { value: "none", label: "None of" },
  ],
  filters: [
    control("year_from", "From", "integer", { group: "Year" }),
    control("year_to", "To", "integer", { group: "Year" }),
    control("type", "Type", "options", {
      options: [
        { value: "article", label: "Article" },
        { value: "book", label: "Book" },
      ],
    }),
    control("open_access", "Open access only", "toggle"),
  ],
};

/** A declaration with the richer value sources: several values combined by a
 *  mode, values the provider suggests, and a value taken from a row. */
const RICH_BROWSE: BrowseControls = {
  ...OPENALEX_BROWSE,
  filters: [
    control("language", "Language", "options", {
      modes: ["any", "none"],
      options: [
        { value: "en", label: "English" },
        { value: "fr", label: "French" },
      ],
    }),
    control("author", "Author", "suggest", { modes: ["any", "none"] }),
    control("title", "Title mentions", "text"),
    control("cites", "Works citing", "result", { from_result: "id" }),
  ],
};

function provider(
  id: string,
  name: string,
  browse: BrowseControls | null = null,
  enabled = true,
): LiteratureProviderInfo {
  return {
    provider: id,
    name,
    enabled,
    builtin: true,
    base_url: `https://api.${id}.example`,
    default_base_url: `https://api.${id}.example`,
    capabilities: { paged_search: true, browse, lookup: true, references: false, resolve_download: false },
    secrets: [],
  };
}

const search = api.catalogueSearch as unknown as ReturnType<typeof vi.fn>;
const providers = api.literatureProviders as unknown as ReturnType<typeof vi.fn>;
const literature = api.literatureSearch as unknown as ReturnType<typeof vi.fn>;
const browse = api.literatureBrowse as unknown as ReturnType<typeof vi.fn>;
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
      collapsed: [],
      providerOptions: [],
      providerOptionsError: null,
      browseOpen: false,
      refinements: {},
      listings: {},
    });
    useSettingsStore.setState({ directory: "/library", refreshFileList: vi.fn() as never });
    useWorkspaceStore.setState({ workspaces: [], activeWorkspaceId: null } as never);
    providers.mockResolvedValue([
      provider("semantic_scholar", "Semantic Scholar"),
      provider("openalex", "OpenAlex", OPENALEX_BROWSE),
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
    expect(literature).toHaveBeenCalledWith("python lists", 10, undefined);
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
    expect(literature).toHaveBeenCalledWith("python lists", 10, ["openalex"]);
    expect(JSON.parse(localStorage.getItem("wilkes.catalogue.filters") ?? "null")).toEqual({
      grains: null,
      providers: ["openalex"],
      collapsed: [],
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
      expect(literature).toHaveBeenLastCalledWith("python lists", 10, [
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

  /// Collapsing is a display choice, not a filter: the half is still asked,
  /// so opening it again shows the answer rather than a blank that needs a
  /// re-run. The count on the heading is what says results are in there.
  it("rolls a section up without stopping it being searched", async () => {
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    fireEvent.click(screen.getByRole("button", { name: /Literature/ }));
    expect(screen.queryByText("Persistent Lists Revisited")).toBeNull();
    expect(screen.getByRole("button", { name: /Literature/ })).toHaveProperty(
      "ariaExpanded",
      "false",
    );

    submit("python lists again");
    await waitFor(() => expect(literature).toHaveBeenCalledTimes(2));
    fireEvent.click(screen.getByRole("button", { name: /Literature/ }));
    expect(screen.getByText("Persistent Lists Revisited")).toBeTruthy();
  });

  /// The same standing-choice storage as the filters, because which half to
  /// look at is the same kind of choice as which sources to ask.
  it("remembers which sections were rolled up", async () => {
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "OpenAlex" });
    fireEvent.click(screen.getByRole("button", { name: /Open catalogues/ }));
    expect(JSON.parse(localStorage.getItem("wilkes.catalogue.filters") ?? "null")).toEqual({
      grains: null,
      providers: null,
      collapsed: ["catalogues"],
    });
    // A collapsed section hides its own filter with it.
    expect(screen.queryByRole("button", { name: "Textbooks" })).toBeNull();
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

  // ── Reading on in a keyword search ───────────────────────────────────────

  const hits = (count: number, from = 0): LiteratureSearchResult[] =>
    Array.from({ length: count }, (_, i) => ({
      ...WORK,
      id: `S${from + i}`,
      title: `Searched work ${from + i}`,
    }));
  const answerOf = (id: string, name: string, results: LiteratureSearchResult[]) => ({
    query: "python lists",
    providers: [{ provider: id, name, results, error: null }],
  });

  it("reads on in a keyword search, one provider at a time, appending to what it said", async () => {
    literature
      .mockResolvedValueOnce({
        query: "python lists",
        providers: [
          { provider: "semantic_scholar", name: "Semantic Scholar", results: hits(2), error: null },
          { provider: "openalex", name: "OpenAlex", results: hits(10), error: null },
        ],
      })
      .mockResolvedValueOnce(answerOf("openalex", "OpenAlex", hits(4, 10)));
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Searched work 9");

    // Semantic Scholar's page was short, so it has no next one; OpenAlex's was
    // full, so it has.
    expect(screen.queryByRole("button", { name: "Load more from Semantic Scholar" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Load more from OpenAlex" }));
    expect(await screen.findByText("Searched work 13")).toBeTruthy();
    // Only that provider was asked, for the next page of the same question.
    expect(literature).toHaveBeenLastCalledWith("python lists", 10, ["openalex"], 2);
    // And a short page was the last.
    expect(screen.queryByRole("button", { name: "Load more from OpenAlex" })).toBeNull();
  });

  /// A provider whose manifest names no page would answer its first again.
  it("lists a work once when two pages both return it", async () => {
    literature
      .mockResolvedValueOnce(answerOf("openalex", "OpenAlex", hits(10)))
      .mockResolvedValueOnce(answerOf("openalex", "OpenAlex", [...hits(1, 9), ...hits(2, 10)]));
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Searched work 9");
    fireEvent.click(screen.getByRole("button", { name: "Load more from OpenAlex" }));
    await screen.findByText("Searched work 11");
    expect(screen.getAllByText("Searched work 9")).toHaveLength(1);
  });

  it("offers no next page for a provider whose search cannot be read past its first", async () => {
    providers.mockResolvedValue([
      { ...provider("openalex", "OpenAlex", OPENALEX_BROWSE), capabilities: {
        ...provider("openalex", "OpenAlex", OPENALEX_BROWSE).capabilities,
        paged_search: false,
      } },
    ]);
    literature.mockResolvedValue(answerOf("openalex", "OpenAlex", hits(10)));
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Searched work 9");
    expect(screen.queryByRole("button", { name: /Load more from/ })).toBeNull();
  });

  it("says why the next page failed and keeps what was read", async () => {
    literature
      .mockResolvedValueOnce(answerOf("openalex", "OpenAlex", hits(10)))
      .mockResolvedValueOnce({
        query: "python lists",
        providers: [
          { provider: "openalex", name: "OpenAlex", results: null, error: "rate limit reached" },
        ],
      });
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Searched work 9");
    fireEvent.click(screen.getByRole("button", { name: "Load more from OpenAlex" }));
    expect(
      await screen.findByText(/OpenAlex could not read the next page: rate limit reached/),
    ).toBeTruthy();
    expect(screen.getByText("Searched work 0")).toBeTruthy();
    // The button is still there to try again.
    expect(screen.getByRole("button", { name: "Load more from OpenAlex" })).toBeTruthy();
  });

  it("names the source chips for what they decide, apart from the Filters button", async () => {
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "OpenAlex" });
    expect(screen.getAllByRole("group", { name: "Search in" })).toHaveLength(2);
    expect(screen.getByRole("button", { name: "Filters" })).toBeTruthy();
  });

  // ── Paper rows ───────────────────────────────────────────────────────────

  /// The line is clamped, so what comes first is what survives: a long author
  /// list must not push the citations and the access out of view.
  it("leads a paper's facts with year, citations and access, before its authors", async () => {
    literature.mockResolvedValue(
      answerOf("openalex", "OpenAlex", [
        { ...WORK, authors: "Ada Lovelace, Alan Turing, Grace Hopper" },
      ]),
    );
    render(<CataloguePane />);
    submit("python lists");
    const facts = await screen.findByTitle(/^2021 · 12 citations · Open access · Ada Lovelace/);
    expect(facts).toBeTruthy();
  });

  /// A provider that reported no count has not reported zero.
  it("shows no citation count for a work whose provider reported none", async () => {
    literature.mockResolvedValue(
      answerOf("openalex", "OpenAlex", [{ ...WORK, citation_count: null }]),
    );
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    expect(screen.queryByText(/citation/)).toBeNull();
    expect(screen.queryByTitle(/0 citations/)).toBeNull();
  });

  // ── Filters ──────────────────────────────────────────────────────────────

  const works = (count: number, from = 0): LiteratureSearchResult[] =>
    Array.from({ length: count }, (_, i) => ({
      ...WORK,
      id: `W${from + i}`,
      title: `Browsed work ${from + i}`,
    }));
  const listing = (results: LiteratureSearchResult[]) => ({
    provider: "openalex",
    name: "OpenAlex",
    results,
    error: null,
  });

  const openFilters = async () => {
    fireEvent.click(await screen.findByRole("button", { name: "Filters" }));
    return screen.getByRole("group", { name: "Filters" });
  };
  const searchBox = () => screen.getByLabelText("Search catalogues and literature");

  /// What a provider can be filtered by is its manifest's declaration. A pane
  /// that offered filters for a provider that declares none would be asking a
  /// question the provider cannot answer.
  it("offers filters only when an enabled provider declares something to refine", async () => {
    providers.mockResolvedValue([provider("semantic_scholar", "Semantic Scholar")]);
    render(<CataloguePane />);
    await screen.findByRole("button", { name: "Semantic Scholar" });
    expect(screen.queryByRole("button", { name: /^Filters/ })).toBeNull();
  });

  it("does not offer filters from a provider that is switched off", async () => {
    providers.mockResolvedValue([provider("openalex", "OpenAlex", OPENALEX_BROWSE, false)]);
    render(<CataloguePane />);
    await waitFor(() => expect(providers).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: /^Filters/ })).toBeNull();
  });

  it("puts the filters beside the search box, and says so before anything is asked", async () => {
    render(<CataloguePane />);
    expect(await screen.findByRole("button", { name: "Filters" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "open Filters" }));
    expect(screen.getByRole("group", { name: "Filters" })).toBeTruthy();
  });

  it("applies each change as it is made, asking for exactly what was set", async () => {
    browse.mockResolvedValue(listing(works(3)));
    render(<CataloguePane />);
    const panel = await openFilters();

    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "article" } });
    expect(await screen.findByText("Browsed work 0")).toBeTruthy();
    expect(browse).toHaveBeenLastCalledWith("openalex", {
      // No keyword was typed.
      query: null,
      filters: { type: ["article"] },
      modes: {},
      sort: null,
      page: 1,
      limit: 10,
    });

    fireEvent.click(within(panel).getByLabelText("Open access only"));
    fireEvent.change(within(panel).getByLabelText("Sort"), {
      target: { value: "publication_date:desc" },
    });
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith("openalex", {
        query: null,
        filters: { type: ["article"], open_access: ["true"] },
        modes: {},
        sort: "publication_date:desc",
        page: 1,
        limit: 10,
      }),
    );
    // The listing sits in the Literature section under the provider's name,
    // and nothing was asked of the keyword search.
    expect(
      within(screen.getByRole("region", { name: "Literature" })).getByLabelText(
        "OpenAlex listing",
      ),
    ).toBeTruthy();
    expect(literature).not.toHaveBeenCalled();
  });

  /// A range is typed a digit at a time. Asking the provider about "2", "20"
  /// and "202" on the way to 2024 would be three requests nobody wanted.
  it("applies a range when the field is left, once, and not per keystroke", async () => {
    browse.mockResolvedValue(listing(works(1)));
    render(<CataloguePane />);
    const panel = await openFilters();
    const min = within(panel).getByLabelText("Year from");
    fireEvent.focus(min);
    fireEvent.change(min, { target: { value: "2" } });
    fireEvent.change(min, { target: { value: "2020" } });
    expect(browse).not.toHaveBeenCalled();
    fireEvent.blur(min);
    await waitFor(() => expect(browse).toHaveBeenCalledTimes(1));
    expect(browse.mock.calls[0][1].filters).toEqual({ year_from: ["2020"] });
    // Leaving it again without a change asks nothing.
    fireEvent.focus(min);
    fireEvent.blur(min);
    expect(browse).toHaveBeenCalledTimes(1);
  });

  it("counts what is set on the button and shows it as removable chips when closed", async () => {
    browse.mockResolvedValue(listing(works(2)));
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "book" } });
    fireEvent.click(within(panel).getByLabelText("Open access only"));
    await screen.findByText("Browsed work 0");

    fireEvent.click(screen.getByRole("button", { name: "Filters, 2 set" }));
    expect(screen.queryByRole("group", { name: "Filters" })).toBeNull();
    const chips = screen.getByLabelText("Active filters");
    expect(within(chips).getByText("Type: Book")).toBeTruthy();

    // Removing a chip clears that filter and re-asks.
    fireEvent.click(within(chips).getByRole("button", { name: "Remove Type: Book" }));
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({ filters: { open_access: ["true"] } }),
      ),
    );
  });

  it("goes back to keyword search when the last filter is cleared", async () => {
    browse.mockResolvedValue(listing(works(2)));
    render(<CataloguePane />);
    fireEvent.change(searchBox(), { target: { value: "python lists" } });
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "book" } });
    await screen.findByText("Browsed work 0");
    expect(literature).not.toHaveBeenCalled();

    fireEvent.click(within(panel).getByRole("button", { name: "Reset OpenAlex filters" }));
    // OpenAlex was left out of the keyword search while it was a listing, so
    // it is asked now, and the listing is gone.
    await waitFor(() => expect(literature).toHaveBeenCalled());
    expect(literature.mock.calls[0][0]).toBe("python lists");
    expect(await screen.findByText("Persistent Lists Revisited")).toBeTruthy();
    expect(screen.queryByText("Browsed work 0")).toBeNull();
  });

  /// The keyword is combined with the filters when the provider's listing
  /// takes one, and the provider is then not searched a second time.
  it("combines the typed keyword with the filters on submit and lists the provider once", async () => {
    browse.mockResolvedValue(listing(works(1)));
    literature.mockResolvedValue({
      query: "persistent lists",
      providers: [
        { provider: "semantic_scholar", name: "Semantic Scholar", results: [], error: null },
      ],
    });
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "book" } });
    await screen.findByText("Browsed work 0");

    fireEvent.change(searchBox(), { target: { value: "persistent lists" } });
    submit("persistent lists");
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({ query: "persistent lists", filters: { type: ["book"] } }),
      ),
    );
    // The other provider is still searched; the filtered one is left out of it.
    await waitFor(() => expect(literature).toHaveBeenCalled());
    expect(literature).toHaveBeenCalledWith("persistent lists", 10, ["semantic_scholar"]);
    expect(screen.getAllByLabelText("OpenAlex listing")).toHaveLength(1);
  });

  it("reads on: the next page extends the listing and the last short page ends it", async () => {
    browse
      .mockResolvedValueOnce(listing(works(10)))
      .mockResolvedValueOnce(listing(works(3, 10)));
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "article" } });
    expect(await screen.findByText("Browsed work 9")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Load more filtered OpenAlex" }));
    expect(await screen.findByText("Browsed work 12")).toBeTruthy();
    // The same request, one page on; and what was read stays.
    expect(browse.mock.calls[1][1]).toMatchObject({
      filters: { type: ["article"] },
      page: 2,
      limit: 10,
    });
    expect(screen.getByText("Browsed work 0")).toBeTruthy();
    // A short page is the last one.
    expect(screen.queryByRole("button", { name: "Load more filtered OpenAlex" })).toBeNull();
  });

  it("says what a provider refused, under its name, and keeps what was already read", async () => {
    browse
      .mockResolvedValueOnce(listing(works(10)))
      .mockResolvedValueOnce({
        provider: "openalex",
        name: "OpenAlex",
        results: null,
        error: "OpenAlex rate limit reached",
      });
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "article" } });
    await screen.findByText("Browsed work 0");
    fireEvent.click(screen.getByRole("button", { name: "Load more filtered OpenAlex" }));
    expect(await screen.findByText(/OpenAlex could not list works: OpenAlex rate limit reached/)).toBeTruthy();
    expect(screen.getByText("Browsed work 9")).toBeTruthy();
  });

  it("says what a request the manifest refused was", async () => {
    browse.mockRejectedValue(new Error("OpenAlex integration is disabled."));
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "article" } });
    expect(await screen.findByText(/could not list works: Error: OpenAlex integration is disabled/)).toBeTruthy();
  });

  /// A provider that is off has no filters and no listing. Nothing about it is
  /// said, because nothing about it is shown.
  it("shows no filters for a provider the user has switched off, and asks it nothing", async () => {
    browse.mockResolvedValue(listing(works(2)));
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Type"), { target: { value: "article" } });
    await screen.findByText("Browsed work 0");
    expect(browse).toHaveBeenCalledTimes(1);

    fireEvent.click(await screen.findByRole("button", { name: "OpenAlex" }));
    expect(screen.queryByRole("group", { name: "OpenAlex filters" })).toBeNull();
    expect(screen.queryByLabelText("OpenAlex listing")).toBeNull();
    expect(within(panel).getByText(/None of the sources selected above can be filtered/)).toBeTruthy();

    // Switching it back on asks for the listing again, with its filters kept.
    fireEvent.click(screen.getByRole("button", { name: "OpenAlex" }));
    await waitFor(() => expect(browse).toHaveBeenCalledTimes(2));
    expect(browse.mock.calls[1][1].filters).toEqual({ type: ["article"] });
  });

  /// Filters belong to the provider that declared them. Two providers that
  /// declare some each get a set, and each is filtered on its own.
  it("gives every filterable provider its own filters, applied independently", async () => {
    providers.mockResolvedValue([
      provider("semantic_scholar", "Semantic Scholar"),
      provider("openalex", "OpenAlex", OPENALEX_BROWSE),
      provider("custom:crossref", "Crossref", {
        keyword: false,
        sort: {
          default: "issued",
          keyword_default: null,
          options: [
            { value: "issued", label: "Issued" },
            { value: "cited", label: "Most cited" },
          ],
        },
        filters: [],
      }),
    ]);
    browse.mockImplementation(async (id: string) => ({
      provider: id,
      name: id === "openalex" ? "OpenAlex" : "Crossref",
      results: works(1).map((w) => ({ ...w, id: `${id}-1`, title: `${id} work` })),
      error: null,
    }));
    render(<CataloguePane />);
    const panel = await openFilters();
    const openalex = within(panel).getByRole("group", { name: "OpenAlex filters" });
    const crossref = within(panel).getByRole("group", { name: "Crossref filters" });
    // A provider that declares nothing has no set.
    expect(within(panel).queryByRole("group", { name: "Semantic Scholar filters" })).toBeNull();

    fireEvent.change(within(openalex).getByLabelText("Type"), { target: { value: "book" } });
    await screen.findByText("openalex work");
    expect(browse).toHaveBeenCalledTimes(1);
    expect(browse).toHaveBeenLastCalledWith("openalex", expect.objectContaining({ filters: { type: ["book"] } }));
    // Crossref was not asked: nothing was set on it.
    expect(screen.queryByLabelText("Crossref listing")).toBeNull();

    fireEvent.change(within(crossref).getByLabelText("Sort"), { target: { value: "cited" } });
    await screen.findByText("custom:crossref work");
    expect(browse).toHaveBeenLastCalledWith(
      "custom:crossref",
      expect.objectContaining({ sort: "cited", filters: {} }),
    );
    // Both are listings now, in the registry's order, and each only once.
    expect(screen.getAllByLabelText(/listing$/).map((el) => el.getAttribute("aria-label"))).toEqual([
      "OpenAlex listing",
      "Crossref listing",
    ]);

    // Resetting one leaves the other.
    fireEvent.click(within(openalex).getByRole("button", { name: "Reset OpenAlex filters" }));
    await waitFor(() => expect(screen.queryByLabelText("OpenAlex listing")).toBeNull());
    expect(screen.getByLabelText("Crossref listing")).toBeTruthy();
  });

  it("tells a keyword-less listing that the keyword searches the other sources", async () => {
    providers.mockResolvedValue([
      provider("custom:crossref", "Crossref", {
        keyword: false,
        sort: {
          default: "issued",
          keyword_default: null,
          options: [{ value: "issued", label: "Issued" }],
        },
        filters: [],
      }),
    ]);
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(searchBox(), { target: { value: "ignored" } });
    expect(within(panel).getByText(/Crossref's listing does not take a keyword/)).toBeTruthy();
  });

  // ── Value sources: several values, suggestions, rows, totals ─────────────

  const suggest = api.literatureFilterSuggest as unknown as ReturnType<typeof vi.fn>;
  const rich = () =>
    providers.mockResolvedValue([provider("openalex", "OpenAlex", RICH_BROWSE)]);

  /// A filter that combines values is a checklist, and how they combine is
  /// the provider's declared modes, sent only when it is not the first.
  it("sets several values of one filter and says how they are combined", async () => {
    rich();
    browse.mockResolvedValue(listing(works(1)));
    render(<CataloguePane />);
    const panel = await openFilters();
    const language = within(panel).getByRole("group", { name: "Language" });
    fireEvent.click(within(language).getByLabelText("English"));
    fireEvent.click(within(language).getByLabelText("French"));
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({ filters: { language: ["en", "fr"] }, modes: {} }),
      ),
    );

    fireEvent.change(within(panel).getByLabelText("Language matches"), {
      target: { value: "none" },
    });
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({
          filters: { language: ["en", "fr"] },
          modes: { language: "none" },
        }),
      ),
    );
    // Closed, the chip says the values as a person would and how they combine.
    fireEvent.click(screen.getByRole("button", { name: "Filters, 1 set" }));
    expect(
      within(screen.getByLabelText("Active filters")).getByText(
        "Language (none of): English, French",
      ),
    ).toBeTruthy();
  });

  /// The provider is asked once typing pauses, not per keystroke, and a chosen
  /// suggestion is remembered by its name because the request carries its id.
  it("offers what the provider suggests and filters by the one chosen", async () => {
    rich();
    browse.mockResolvedValue(listing(works(1)));
    suggest.mockResolvedValue([
      { value: "https://openalex.org/A1", label: "Geoffrey Hinton", hint: "University of Toronto" },
      { value: "https://openalex.org/A2", label: "G. Hinton", hint: null },
    ]);
    render(<CataloguePane />);
    const panel = await openFilters();
    const box = within(panel).getByLabelText("Author");
    fireEvent.change(box, { target: { value: "h" } });
    fireEvent.change(box, { target: { value: "hin" } });
    fireEvent.change(box, { target: { value: "hinton" } });
    const choices = await within(panel).findByRole("list", { name: "Author suggestions" });
    expect(suggest).toHaveBeenCalledTimes(1);
    expect(suggest).toHaveBeenCalledWith("openalex", "author", "hinton");
    expect(within(choices).getByText("University of Toronto")).toBeTruthy();

    fireEvent.click(within(choices).getByText("Geoffrey Hinton"));
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({ filters: { author: ["https://openalex.org/A1"] } }),
      ),
    );
    // The chosen value reads as its name, in the panel and on the chip.
    expect(
      within(panel).getByRole("button", { name: "Remove Geoffrey Hinton from Author" }),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Filters, 1 set" }));
    expect(
      within(screen.getByLabelText("Active filters")).getByText("Author: Geoffrey Hinton"),
    ).toBeTruthy();
  });

  it("says why suggestions could not be read", async () => {
    rich();
    suggest.mockRejectedValue(new Error("OpenAlex rate limit reached"));
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.change(within(panel).getByLabelText("Author"), { target: { value: "hinton" } });
    expect(await within(panel).findByText(/OpenAlex rate limit reached/)).toBeTruthy();
  });

  /// "Works citing this one": the value is a row, not something typed. The
  /// row offers whatever the provider declared, and knows nothing more.
  it("sets a filter from a result row and lists by it", async () => {
    rich();
    literature.mockResolvedValue(answerOf("openalex", "OpenAlex", [WORK]));
    browse.mockResolvedValue(listing(works(2)));
    render(<CataloguePane />);
    submit("python lists");
    fireEvent.click(
      await screen.findByRole("button", { name: "Works citing Persistent Lists Revisited" }),
    );
    await screen.findByText("Browsed work 0");
    expect(browse).toHaveBeenCalledWith(
      "openalex",
      expect.objectContaining({ query: "python lists", filters: { cites: ["W1"] } }),
    );
    // It has no control of its own; set, it shows as what it was set to.
    fireEvent.click(screen.getByRole("button", { name: "Filters, 1 set" }));
    const panel = screen.getByRole("group", { name: "Filters" });
    expect(
      within(panel).getByRole("button", {
        name: "Remove Persistent Lists Revisited from Works citing",
      }),
    ).toBeTruthy();
  });

  it("offers no row filters from a provider that declares none", async () => {
    literature.mockResolvedValue(answerOf("openalex", "OpenAlex", [WORK]));
    render(<CataloguePane />);
    submit("python lists");
    await screen.findByText("Persistent Lists Revisited");
    expect(screen.queryByRole("button", { name: /^Works citing/ })).toBeNull();
  });

  /// Ten of twelve thousand is a different thing from ten in all.
  it("says how many works match in all when the provider says", async () => {
    browse.mockResolvedValue({ ...listing(works(10)), total: 12430 });
    render(<CataloguePane />);
    const panel = await openFilters();
    fireEvent.click(within(panel).getByLabelText("Open access only"));
    await screen.findByText("Browsed work 9");
    expect(screen.getByLabelText("OpenAlex listing").textContent).toContain("10 of 12,430");
  });

  /// Forty controls is not a panel: past a handful, only the filters in use
  /// are drawn and the rest are one menu away.
  it("shows the filters in use and offers the rest from a menu when there are many", async () => {
    const many: BrowseControls = {
      ...OPENALEX_BROWSE,
      filters: Array.from({ length: 12 }, (_, i) => control(`flag_${i}`, `Flag ${i}`, "toggle")),
    };
    providers.mockResolvedValue([provider("openalex", "OpenAlex", many)]);
    browse.mockResolvedValue(listing(works(1)));
    render(<CataloguePane />);
    const panel = await openFilters();
    expect(within(panel).queryByLabelText("Flag 3")).toBeNull();

    fireEvent.change(within(panel).getByLabelText("Add a OpenAlex filter"), {
      target: { value: "flag_3" },
    });
    fireEvent.click(within(panel).getByLabelText("Flag 3"));
    await waitFor(() =>
      expect(browse).toHaveBeenLastCalledWith(
        "openalex",
        expect.objectContaining({ filters: { flag_3: ["true"] } }),
      ),
    );
    // Now in use, it is no longer on the menu.
    const menu = within(panel).getByLabelText("Add a OpenAlex filter") as HTMLSelectElement;
    expect(Array.from(menu.options).map((option) => option.value)).not.toContain("flag_3");
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
    // What a provider reported about the file is named in the row's one line
    // of facts, so the user knows what an Add would fetch before clicking it.
    expect(await screen.findByText(/English · epub · 2 MB/)).toBeTruthy();
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

  /// A resolver is a request of its own, and a credentialed one is not fast.
  /// It used to run before anything was marked as adding, so the row sat
  /// completely inert — no spinner, no disabled button — until bytes moved.
  it("reports the resolve before any byte has moved", async () => {
    const providerWork: LiteratureSearchResult = {
      ...WORK,
      id: "A1",
      title: "Resolved Book",
      pdf_url: null,
      acquisition: "provider",
    };
    literature.mockResolvedValue({
      query: "resolved book",
      providers: [{ provider: "anna", name: "Anna", results: [providerWork], error: null }],
    });
    let finishResolve = (_: { url: string; filename: string | null }) => {};
    resolveDownload.mockImplementation(() => new Promise((resolve) => (finishResolve = resolve)));

    render(<CataloguePane />);
    submit("resolved book");
    fireEvent.click(await screen.findByLabelText("Add Resolved Book to library"));
    expect(await screen.findByText("Finding a copy…")).toBeTruthy();
    expect(screen.getByRole("progressbar")).toBeTruthy();
    expect(acquire).not.toHaveBeenCalled();

    acquire.mockResolvedValue({ path: "/uploads/r.pdf", bytes: 10, already_present: false });
    await act(async () => {
      finishResolve({ url: "https://files.example.invalid/r", filename: null });
    });
    await waitFor(() => expect(acquire).toHaveBeenCalled());
    expect(await screen.findByText("Added")).toBeTruthy();
  });

  /// A resolver that refuses is the add failing, and used to reject into
  /// nothing at all: no message, and an unhandled rejection in the console.
  it("reports a resolver that refuses", async () => {
    const providerWork: LiteratureSearchResult = {
      ...WORK,
      id: "A1",
      title: "Resolved Book",
      pdf_url: null,
      acquisition: "provider",
    };
    literature.mockResolvedValue({
      query: "resolved book",
      providers: [{ provider: "anna", name: "Anna", results: [providerWork], error: null }],
    });
    resolveDownload.mockRejectedValue(new Error("membership required"));

    render(<CataloguePane />);
    submit("resolved book");
    fireEvent.click(await screen.findByLabelText("Add Resolved Book to library"));
    expect(await screen.findByText(/membership required/)).toBeTruthy();
    expect(useCatalogueStore.getState().adding).toEqual({});
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
