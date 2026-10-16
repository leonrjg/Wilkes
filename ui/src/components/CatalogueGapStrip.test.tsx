import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LiteratureProviderInfo, LiteratureSearchResult } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    literatureProviders: vi.fn(),
    literatureSearch: vi.fn(),
    catalogueAcquire: vi.fn(),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: { type: "desktop", deletionKind: "trash", importFiles: vi.fn() },
}));

import { api } from "../services";
import CatalogueGapStrip, { CatalogueGapPrompt } from "./CatalogueGapStrip";
import { useCatalogueStore } from "../stores/useCatalogueStore";

const BOOK: LiteratureSearchResult = {
  id: "7",
  doi: null,
  title: "Introductory Statistics",
  year: null,
  publication_date: null,
  venue: null,
  citation_count: null,
  is_open_access: true,
  pdf_url: "https://example.invalid/stats.pdf",
  landing_page_url: "https://example.invalid/stats",
  open_access_status: null,
  license: "CC-BY",
  authors: "OpenStax",
  publisher: null,
  language: null,
  file_format: null,
  file_size: null,
  summary: "Distributions, inference and regression.",
  subject: "Mathematics",
  kind: "textbook",
  pages: 900,
  recall_score: 4.2,
  acquisition: "direct",
};

function provider(
  id: string,
  name: string,
  mirrored: boolean,
  enabled = true,
): LiteratureProviderInfo {
  return {
    provider: id,
    name,
    enabled,
    builtin: true,
    base_url: `https://${id}.example`,
    default_base_url: null,
    capabilities: {
      paged_search: !mirrored,
      browse: null,
      lookup: !mirrored,
      references: false,
      resolve_download: false,
      mirrored,
    },
    secrets: [],
    mirror: mirrored
      ? { grain: "textbook", records: enabled ? 700 : 0, synced_at_ms: enabled ? 1 : null }
      : null,
  };
}

const providers = api.literatureProviders as unknown as ReturnType<typeof vi.fn>;
const search = api.literatureSearch as unknown as ReturnType<typeof vi.fn>;

const answer = (id: string, name: string, results: LiteratureSearchResult[]) => ({
  provider: id,
  name,
  results,
  error: null,
  total: null,
});

describe("CatalogueGapStrip", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useCatalogueStore.getState().reset();
    useCatalogueStore.setState({ providers: null, legacy: undefined, providerOptions: [] });
    providers.mockResolvedValue([
      provider("openstax", "OpenStax", true),
      provider("libretexts", "LibreTexts", true),
      provider("openalex", "OpenAlex", false),
    ]);
    search.mockResolvedValue({
      query: "confidence intervals",
      providers: [answer("openstax", "OpenStax", [BOOK])],
    });
  });

  it("offers what the catalogues hold on the question the library could not answer", async () => {
    render(<CatalogueGapStrip query="confidence intervals" />);
    expect(await screen.findByText("Introductory Statistics")).toBeTruthy();
    expect(screen.getByText("Related learning resources")).toBeTruthy();
    // The strip mixes catalogues, so a row says whose it is.
    expect(screen.getByText(/^OpenStax · Mathematics/)).toBeTruthy();
  });

  /// This runs unprompted after every empty search. A provider answered from
  /// this machine is the only kind that can be asked without sending the
  /// user's words somewhere they did not send them — and an index the user
  /// enabled is still one they have not asked *this* question.
  it("asks the mirrored providers and never a remote one", async () => {
    render(<CatalogueGapStrip query="confidence intervals" />);
    await screen.findByText("Introductory Statistics");
    expect(search).toHaveBeenCalledTimes(1);
    // The probe is the user's own words, passed through unchanged.
    expect(search).toHaveBeenCalledWith("confidence intervals", 5, ["openstax", "libretexts"]);
  });

  /// With no catalogue to ask, the search is not made at all: without a
  /// provider list it would be every enabled provider's.
  it("asks nobody when the only providers that are on are remote", async () => {
    providers.mockResolvedValue([
      provider("openstax", "OpenStax", true, false),
      provider("openalex", "OpenAlex", false),
    ]);
    render(<CatalogueGapStrip query="topology" />);
    expect(await screen.findByText(/Sync catalogues in Settings/i)).toBeTruthy();
    expect(search).not.toHaveBeenCalled();
  });

  /// The catalogues share one index, so their scores are one scale: the strip
  /// is the best five across them, not each one's best in turn.
  it("offers the best matches across the catalogues, five at most", async () => {
    const scored = (id: string, score: number): LiteratureSearchResult => ({
      ...BOOK,
      id,
      title: `Book ${id}`,
      recall_score: score,
    });
    search.mockResolvedValue({
      query: "statistics",
      providers: [
        answer("openstax", "OpenStax", [scored("a", 3), scored("b", 1)]),
        answer("libretexts", "LibreTexts", [
          scored("c", 9),
          scored("d", 5),
          scored("e", 4),
          scored("f", 2),
        ]),
      ],
    });
    render(<CatalogueGapStrip query="statistics" />);
    await screen.findByText("Book c");
    expect(screen.getAllByText(/^Book /).map((row) => row.textContent)).toEqual([
      "Book c",
      "Book d",
      "Book e",
      "Book a",
      "Book f",
    ]);
  });

  /// Recall is not a ranking, and the strip is the one place a user could
  /// mistake it for one.
  it("identifies the ranking as text relevance", async () => {
    render(<CatalogueGapStrip query="confidence" />);
    expect(await screen.findByText(/Ranked by text relevance/i)).toBeTruthy();
  });

  /// A query that reduced to nothing was never looked for, and each catalogue
  /// says so as an error. Offering that under a failed search would be noise
  /// about a search that never ran.
  it("renders nothing when no term in the query survived filtering", async () => {
    search.mockResolvedValue({
      query: "C",
      providers: [
        {
          provider: "openstax",
          name: "OpenStax",
          results: null,
          error: "nothing in “C” could be searched for",
          total: null,
        },
      ],
    });
    const { container } = render(<CatalogueGapStrip query="C" />);
    await waitFor(() => expect(search).toHaveBeenCalled());
    await waitFor(() => expect(container.textContent).toBe(""));
  });

  /// "Nothing matched" and "nothing has been fetched yet" look identical from
  /// here, and only one of them is the user's to fix.
  it("distinguishes an unfetched mirror from a genuine absence", async () => {
    providers.mockResolvedValue([provider("openstax", "OpenStax", true, false)]);
    render(<CatalogueGapStrip query="topology" />);
    expect(await screen.findByText(/Sync catalogues in Settings/i)).toBeTruthy();
  });

  it("stays silent when the catalogues are held and simply hold nothing", async () => {
    search.mockResolvedValue({
      query: "topology",
      providers: [answer("openstax", "OpenStax", []), answer("libretexts", "LibreTexts", [])],
    });
    const { container } = render(<CatalogueGapStrip query="topology" />);
    await waitFor(() => expect(search).toHaveBeenCalled());
    await waitFor(() => expect(container.textContent).toBe(""));
  });

  /// A catalogue that cannot answer must not turn an empty search into an error
  /// about a feature the user did not invoke.
  it("says nothing at all when the lookup itself fails", async () => {
    search.mockRejectedValue(new Error("mirror unavailable"));
    const { container } = render(<CatalogueGapStrip query="topology" />);
    await waitFor(() => expect(search).toHaveBeenCalled());
    expect(container.textContent).toBe("");
  });

  /// The offer under a search that did return something. Wilkes has no score
  /// to judge "thin" by, so it offers rather than decides — and asks nothing of
  /// the catalogues until the offer is taken.
  it("offers the catalogues on a non-empty search without querying them", () => {
    render(<CatalogueGapPrompt query="confidence intervals" />);
    expect(screen.getByRole("button", { name: "Search learning catalogues" })).toBeTruthy();
    expect(search).not.toHaveBeenCalled();
    expect(providers).not.toHaveBeenCalled();
  });

  it("opens the pane on that query when the offer is taken", async () => {
    render(<CatalogueGapPrompt query="confidence intervals" />);
    fireEvent.click(screen.getByRole("button", { name: "Search learning catalogues" }));
    await waitFor(() => {
      expect(useCatalogueStore.getState().paneOpen).toBe(true);
      expect(search).toHaveBeenCalledWith("confidence intervals", 10, undefined);
    });
  });
});
