import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CitationGraph, FileEntry } from "../lib/types";
import { useCatalogueStore } from "../stores/useCatalogueStore";
import { useCitationsStore } from "../stores/useCitationsStore";
import { useSearchStore } from "../stores/useSearchStore";
import { useSettingsStore } from "../stores/useSettingsStore";
import { useViewerStore } from "../stores/useViewerStore";
import LibraryCitationsPane from "./LibraryCitationsPane";
import { ToastProvider } from "./Toast";

vi.mock("../services", () => ({
  api: {
    citationGraph: vi.fn(),
    fetchMissingCitations: vi.fn(),
    onFileMetadataUpdated: vi.fn(() => Promise.resolve(vi.fn())),
    cancelSearch: vi.fn().mockResolvedValue(undefined),
    updateSettings: vi.fn().mockResolvedValue(undefined),
    listFiles: vi.fn().mockResolvedValue({ files: [], omitted: [] }),
  },
}));

import { api } from "../services";

const entry = (path: string, title: string, publication_date = "2020") =>
  ({ path, file_type: "Pdf", title, publication_date }) as FileEntry;

const graph: CitationGraph = {
  documents: [
    { doi: "10.1/a", entries: [entry("/lib/a.pdf", "Paper A", "2022")], cited_by: [], references_fetched: true },
    { doi: "10.1/b", entries: [entry("/lib/b.pdf", "Paper B", "2018")], cited_by: [0], references_fetched: true },
    { doi: "10.1/c", entries: [entry("/lib/c.pdf", "Paper C")], cited_by: [], references_fetched: false },
  ],
  edges: [{ source: 0, target: 1 }],
  external: [
    {
      reference: { doi: "10.9/x", title: "Outside work", authors: "Ada Lovelace", year: 1843, venue: "Memoirs" },
      cited_by: [0, 1],
      library_entry: null,
    },
    {
      reference: { doi: "10.9/y", title: null, authors: null, year: null, venue: null },
      cited_by: [0, 1],
      library_entry: entry("/other/y.pdf", "Elsewhere"),
    },
  ],
  external_total: 5,
  without_doi: [entry("/lib/scan.pdf", "A scan")],
  coverage: {
    files: 4,
    pending_metadata: 0,
    metadata_unavailable: 0,
    with_doi: 3,
    references_fetched: 2,
    references_provider: true,
  },
};

function renderPane() {
  return render(
    <ToastProvider>
      <LibraryCitationsPane />
    </ToastProvider>,
  );
}

describe("LibraryCitationsPane", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.citationGraph).mockResolvedValue(graph);
    vi.mocked(api.fetchMissingCitations).mockResolvedValue(1);
    useSettingsStore.setState({ directory: "/lib" });
    useCitationsStore.getState().resetForWorkspace();
    useCitationsStore.setState({ paneOpen: true, scope: "root", view: "list", minExternalCitations: 2 });
  });

  it("lists the most cited documents and the outside works the folder cites", async () => {
    const openFile = vi.fn();
    useViewerStore.setState({ openFile } as never);
    const searchInPane = vi.fn().mockResolvedValue(undefined);
    useCatalogueStore.setState({ searchInPane } as never);
    renderPane();

    expect(await screen.findByText("Paper B")).toBeInTheDocument();
    expect(api.citationGraph).toHaveBeenCalledWith({ root: "/lib", min_external_citations: 2 });
    fireEvent.click(screen.getByText("Cited by 1 here"));
    fireEvent.click(screen.getByText("Paper A"));
    expect(openFile).toHaveBeenCalledWith("/lib/a.pdf");

    expect(screen.getByText("Outside work")).toBeInTheDocument();
    expect(screen.getByText("Ada Lovelace · 1843 · Memoirs")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Find Outside work" }));
    expect(searchInPane).toHaveBeenCalledWith("Outside work");
    // A work elsewhere in the library opens instead of being searched for.
    expect(screen.getByText("in library")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open 10.9/y" }));
    expect(openFile).toHaveBeenCalledWith("/other/y.pdf");
  });

  it("says how much the graph covers and fetches what is missing", async () => {
    renderPane();
    expect(
      await screen.findByText(/3 of 4 files have a DOI, 2 of those have references/),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Fetch 1 missing" }));
    await waitFor(() =>
      expect(api.fetchMissingCitations).toHaveBeenCalledWith({ root: "/lib", min_external_citations: 2 }),
    );
    expect(await screen.findByText("Fetching references for 1 document")).toBeInTheDocument();
  });

  it("asks for the whole library and a new threshold when switched", async () => {
    renderPane();
    await screen.findByText("Paper B");
    fireEvent.click(screen.getByRole("button", { name: "Library" }));
    await waitFor(() =>
      expect(api.citationGraph).toHaveBeenLastCalledWith({ root: null, min_external_citations: 2 }),
    );
    fireEvent.change(screen.getByLabelText("Minimum citations for outside works"), {
      target: { value: "3" },
    });
    await waitFor(() =>
      expect(api.citationGraph).toHaveBeenLastCalledWith({ root: null, min_external_citations: 3 }),
    );
  });

  it("shows documents left out of the graph as a result set", async () => {
    const showResultSet = vi.fn().mockResolvedValue(undefined);
    useSearchStore.setState({ showResultSet } as never);
    renderPane();
    fireEvent.click(await screen.findByText(/1 without a DOI, left out/));
    expect(showResultSet).toHaveBeenCalledWith(
      [{ path: "/lib/scan.pdf", file_type: "Pdf", title: "A scan", matches: [] }],
      { kind: "citations", subject: "Documents without a DOI" },
    );
  });

  it("draws the graph by year", async () => {
    useCitationsStore.setState({ view: "graph" });
    renderPane();
    expect(
      await screen.findByRole("img", { name: "Citation graph by year: 5 works, 5 citations" }),
    ).toBeInTheDocument();
    expect(screen.getByText("2018")).toBeInTheDocument();
    expect(screen.getByText("No date")).toBeInTheDocument();
  });
});
