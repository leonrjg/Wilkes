import { describe, expect, it } from "vitest";
import type { CitationGraph, FileEntry } from "../types";
import { layoutCitationGraph, MAX_GRAPH_DOCUMENTS, yearOf } from "./citationGraphLayout";

const entry = (path: string, publication_date: string | null, title: string | null = null) =>
  ({ path, file_type: "Pdf", title, publication_date }) as FileEntry;

const coverage = {
  files: 0,
  pending_metadata: 0,
  metadata_unavailable: 0,
  with_doi: 0,
  references_fetched: 0,
  references_provider: true,
};

function graph(overrides: Partial<CitationGraph>): CitationGraph {
  return {
    documents: [],
    edges: [],
    external: [],
    external_total: 0,
    without_doi: [],
    coverage,
    ...overrides,
  };
}

describe("layoutCitationGraph", () => {
  it("reads the year at the start of a date and nothing else", () => {
    expect(yearOf("2021-05")).toBe(2021);
    expect(yearOf("1999")).toBe(1999);
    expect(yearOf("May 2021")).toBeNull();
    expect(yearOf(null)).toBeNull();
  });

  it("places works by year, oldest left, and keeps undated ones in a named column", () => {
    const layout = layoutCitationGraph(
      graph({
        documents: [
          { doi: "10.1/new", entries: [entry("/l/new.pdf", "2024-01", "New")], cited_by: [], references_fetched: true },
          { doi: "10.1/old", entries: [entry("/l/old.pdf", "2017", "Old")], cited_by: [0, 2], references_fetched: true },
          { doi: "10.1/undated", entries: [entry("/l/u.pdf", null)], cited_by: [], references_fetched: true },
        ],
        edges: [
          { source: 0, target: 1 },
          { source: 2, target: 1 },
        ],
        external: [
          {
            reference: { doi: "10.9/x", title: "Outside", authors: "Ada", year: 2010, venue: null },
            cited_by: [0, 1],
            library_entry: null,
          },
        ],
      }),
    );

    expect(layout.columns.map((column) => column.label)).toEqual(["2010", "2017", "2024", "No date"]);
    const x = (key: string) => layout.nodes.find((node) => node.key === key)!.x;
    expect(x("e0")).toBeLessThan(x("d1"));
    expect(x("d1")).toBeLessThan(x("d0"));
    expect(x("d2")).toBe(layout.columns[3].x);
    // Library edges plus one per document citing the outside work.
    expect(layout.edges).toEqual([
      { from: "d0", to: "d1" },
      { from: "d2", to: "d1" },
      { from: "d0", to: "e0" },
      { from: "d1", to: "e0" },
    ]);
    const old = layout.nodes.find((node) => node.key === "d1")!;
    expect(old.labelled).toBe(true);
    expect(old.path).toBe("/l/old.pdf");
    // An outside work not in the library opens nothing.
    expect(layout.nodes.find((node) => node.key === "e0")!.path).toBeNull();
  });

  it("draws only the best-connected documents of a large graph and says so", () => {
    const count = MAX_GRAPH_DOCUMENTS + 5;
    const documents = Array.from({ length: count }, (_, index) => ({
      doi: `10.1/${index}`,
      entries: [entry(`/l/${index}.pdf`, "2020")],
      cited_by: index === 0 ? [1, 2] : [],
      references_fetched: true,
    }));
    const layout = layoutCitationGraph(
      graph({ documents, edges: [{ source: 1, target: 0 }, { source: 2, target: 0 }] }),
    );
    expect(layout.nodes).toHaveLength(MAX_GRAPH_DOCUMENTS);
    expect(layout.hiddenDocuments).toBe(5);
    expect(layout.nodes.some((node) => node.key === "d0")).toBe(true);
    expect(layout.edges).toHaveLength(2);
  });
});
