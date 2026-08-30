import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CatalogueHit } from "../lib/types";

vi.mock("../services", () => ({
  isTauri: true,
  api: {
    catalogueSearch: vi.fn(),
    catalogueStatus: vi.fn(),
    catalogueAcquire: vi.fn(),
    listFiles: vi.fn(() => Promise.resolve({ files: [], omitted: [] })),
  },
  source: { type: "desktop", deletionKind: "trash", importFiles: vi.fn() },
}));

import { api } from "../services";
import CatalogueGapStrip, { CatalogueGapPrompt } from "./CatalogueGapStrip";
import { useCatalogueStore } from "../stores/useCatalogueStore";

const HIT: CatalogueHit = {
  provider: "openstax",
  external_id: "7",
  title: "Introductory Statistics",
  summary: "Distributions, inference and regression.",
  subject: "Mathematics",
  authors: "OpenStax",
  license: "CC-BY",
  landing_url: "https://example.invalid/stats",
  pdf_url: "https://example.invalid/stats.pdf",
  outline_url: null,
  grain: "textbook",
  pages: 900,
  recall_score: 4.2,
};

const search = api.catalogueSearch as unknown as ReturnType<typeof vi.fn>;
const status = api.catalogueStatus as unknown as ReturnType<typeof vi.fn>;

describe("CatalogueGapStrip", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useCatalogueStore.getState().reset();
  });

  it("offers what the catalogues hold on the question the library could not answer", async () => {
    search.mockResolvedValue({
      results: [{ key: "gap", terms: ["confidence", "intervals"], hits: [HIT] }],
    });
    render(<CatalogueGapStrip query="confidence intervals" />);
    expect(await screen.findByText("Introductory Statistics")).toBeTruthy();
    expect(screen.getByText("Nothing here teaches this")).toBeTruthy();
    // The probe is the user's own words, passed through unchanged.
    expect(search.mock.calls[0][0]).toEqual([{ key: "gap", text: "confidence intervals" }]);
  });

  /// Recall is not a ranking, and the strip is the one place a user could
  /// mistake it for one.
  it("says the order is a text match rather than a recommendation", async () => {
    search.mockResolvedValue({
      results: [{ key: "gap", terms: ["confidence"], hits: [HIT] }],
    });
    render(<CatalogueGapStrip query="confidence" />);
    expect(await screen.findByText(/not by which is the better place to start/i)).toBeTruthy();
  });

  /// A query that reduced to nothing was never looked for. Offering "we found
  /// nothing" under a failed search would be noise about a search that never ran.
  it("renders nothing when no term in the query survived filtering", async () => {
    search.mockResolvedValue({ results: [{ key: "gap", terms: [], hits: [] }] });
    const { container } = render(<CatalogueGapStrip query="C" />);
    await waitFor(() => expect(search).toHaveBeenCalled());
    expect(container.textContent).toBe("");
    expect(status).not.toHaveBeenCalled();
  });

  /// "Nothing matched" and "nothing has been fetched yet" look identical from
  /// here, and only one of them is the user's to fix.
  it("distinguishes an empty mirror from a genuine absence", async () => {
    search.mockResolvedValue({
      results: [{ key: "gap", terms: ["topology"], hits: [] }],
    });
    status.mockResolvedValue({ providers: [], total_records: 0 });
    render(<CatalogueGapStrip query="topology" />);
    expect(await screen.findByText(/have not been fetched yet/i)).toBeTruthy();
  });

  it("stays silent when the mirror is full and simply holds nothing", async () => {
    search.mockResolvedValue({
      results: [{ key: "gap", terms: ["topology"], hits: [] }],
    });
    status.mockResolvedValue({ providers: [], total_records: 4_000 });
    const { container } = render(<CatalogueGapStrip query="topology" />);
    await waitFor(() => expect(status).toHaveBeenCalled());
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
    expect(screen.getByText(/Nothing here teaches this\?/)).toBeTruthy();
    expect(search).not.toHaveBeenCalled();
  });

  it("opens the pane on that query when the offer is taken", async () => {
    search.mockResolvedValue({
      results: [{ key: "pane", terms: ["confidence"], hits: [HIT] }],
    });
    render(<CatalogueGapPrompt query="confidence intervals" />);
    fireEvent.click(screen.getByText(/Nothing here teaches this\?/));
    await waitFor(() => {
      expect(useCatalogueStore.getState().paneOpen).toBe(true);
      expect(search.mock.calls[0][0]).toEqual([
        { key: "pane", text: "confidence intervals", grains: null },
      ]);
    });
  });
});
