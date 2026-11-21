import type { CitationGraph, CitationGraphDocument, ExternalCitedWork } from "../types";

/** Most documents drawn at once; beyond it the best-connected are shown. */
export const MAX_GRAPH_DOCUMENTS = 150;
/** Most outside works drawn beside them, most cited first. */
export const MAX_GRAPH_EXTERNAL = 30;
/** How many of the most cited nodes carry a label on the canvas. */
const LABELLED_NODES = 8;

const COLUMN_WIDTH = 48;
const MARGIN_X = 28;
const MARGIN_TOP = 16;
const AXIS_HEIGHT = 28;
const NODE_GAP = 6;

export interface GraphNode {
  key: string;
  kind: "document" | "external";
  /** Index into `documents` or `external`. */
  index: number;
  x: number;
  y: number;
  r: number;
  /** Times cited from within the scope. */
  citedBy: number;
  label: string;
  /** Shown on the canvas beside the node. */
  labelled: boolean;
  /** Full description for the tooltip. */
  description: string;
  /** What opens on click, when the work is a file in the library. */
  path: string | null;
}

export interface GraphEdge {
  /** Node key of the citing work. */
  from: string;
  /** Node key of the cited work. */
  to: string;
}

export interface GraphColumn {
  label: string;
  x: number;
}

export interface GraphLayout {
  nodes: GraphNode[];
  edges: GraphEdge[];
  columns: GraphColumn[];
  width: number;
  height: number;
  /** Plot area bottom, where the year axis sits. */
  axisY: number;
  hiddenDocuments: number;
  hiddenExternal: number;
}

/** The year at the start of a "YYYY", "YYYY-MM" or "YYYY-MM-DD" date. */
export function yearOf(date: string | null | undefined): number | null {
  const match = date ? /^(\d{4})/.exec(date) : null;
  return match ? Number(match[1]) : null;
}

export function documentYear(document: CitationGraphDocument): number | null {
  for (const entry of document.entries) {
    const year = yearOf(entry.publication_date);
    if (year !== null) return year;
  }
  return null;
}

function baseName(path: string) {
  return path.split(/[/\\]/).pop() ?? path;
}

export function documentLabel(document: CitationGraphDocument): string {
  return document.entries[0].title ?? baseName(document.entries[0].path);
}

export function externalLabel(work: ExternalCitedWork): string {
  return work.reference.title ?? work.reference.doi;
}

function radius(citedBy: number) {
  return Math.min(14, 4 + 2 * Math.sqrt(citedBy));
}

/**
 * Lay the graph out on a year axis: each work in the column of its
 * publication year, older to the left, so every citation points leftward or
 * within a column. Works without a date get a column of their own at the
 * right, named as such, rather than a guessed year. Within a column the most
 * cited work is on top. The same graph always gives the same picture.
 */
export function layoutCitationGraph(graph: CitationGraph): GraphLayout {
  const outgoing = new Array<number>(graph.documents.length).fill(0);
  for (const edge of graph.edges) outgoing[edge.source] += 1;

  const byDegree = graph.documents
    .map((document, index) => ({ index, degree: document.cited_by.length + outgoing[index] }))
    .sort((a, b) => b.degree - a.degree || a.index - b.index);
  const shownDocuments = new Set(
    byDegree.slice(0, MAX_GRAPH_DOCUMENTS).map((item) => item.index),
  );
  const shownExternal = graph.external.slice(0, MAX_GRAPH_EXTERNAL);

  type Placed = Omit<GraphNode, "x" | "y" | "labelled"> & { year: number | null };
  const placed: Placed[] = [];
  graph.documents.forEach((document, index) => {
    if (!shownDocuments.has(index)) return;
    const label = documentLabel(document);
    const year = documentYear(document);
    placed.push({
      key: `d${index}`,
      kind: "document",
      index,
      r: radius(document.cited_by.length),
      citedBy: document.cited_by.length,
      label,
      description: `${label}\n${year ?? "No date"} · cited by ${document.cited_by.length} here`,
      path: document.entries[0].path,
      year,
    });
  });
  shownExternal.forEach((work, index) => {
    const label = externalLabel(work);
    const year = work.reference.year ?? null;
    const byline = [work.reference.authors, year?.toString(), work.reference.venue]
      .filter(Boolean)
      .join(" · ");
    placed.push({
      key: `e${index}`,
      kind: "external",
      index,
      r: radius(work.cited_by.length),
      citedBy: work.cited_by.length,
      label,
      description: `${label}${byline ? `\n${byline}` : ""}\n${
        work.library_entry ? "In your library, outside this scope" : "Not in your library"
      } · cited by ${work.cited_by.length} here`,
      path: work.library_entry?.path ?? null,
      year,
    });
  });

  const years = [...new Set(placed.map((node) => node.year).filter((y): y is number => y !== null))]
    .sort((a, b) => a - b);
  const undated = placed.some((node) => node.year === null);
  const columnLabels = [...years.map(String), ...(undated ? ["No date"] : [])];
  const columnOf = (year: number | null) =>
    year === null ? years.length : years.indexOf(year);

  const stacks: Placed[][] = columnLabels.map(() => []);
  for (const node of placed) stacks[columnOf(node.year)].push(node);

  const labelled = new Set(
    [...placed]
      .sort((a, b) => b.citedBy - a.citedBy || a.key.localeCompare(b.key))
      .slice(0, LABELLED_NODES)
      .filter((node) => node.citedBy > 0)
      .map((node) => node.key),
  );

  const nodes: GraphNode[] = [];
  let plotBottom = MARGIN_TOP;
  stacks.forEach((stack, column) => {
    stack.sort((a, b) => b.citedBy - a.citedBy || a.key.localeCompare(b.key));
    let y = MARGIN_TOP;
    for (const { year: _year, ...node } of stack) {
      y += node.r;
      nodes.push({
        ...node,
        x: MARGIN_X + column * COLUMN_WIDTH,
        y,
        labelled: labelled.has(node.key),
      });
      y += node.r + NODE_GAP;
    }
    plotBottom = Math.max(plotBottom, y);
  });

  const shownKeys = new Set(nodes.map((node) => node.key));
  const edges: GraphEdge[] = [];
  for (const edge of graph.edges) {
    const from = `d${edge.source}`;
    const to = `d${edge.target}`;
    if (shownKeys.has(from) && shownKeys.has(to)) edges.push({ from, to });
  }
  shownExternal.forEach((work, index) => {
    for (const source of work.cited_by) {
      const from = `d${source}`;
      if (shownKeys.has(from)) edges.push({ from, to: `e${index}` });
    }
  });

  const axisY = plotBottom + 4;
  return {
    nodes,
    edges,
    columns: columnLabels.map((label, column) => ({
      label,
      x: MARGIN_X + column * COLUMN_WIDTH,
    })),
    width: MARGIN_X * 2 + Math.max(0, columnLabels.length - 1) * COLUMN_WIDTH,
    height: axisY + AXIS_HEIGHT,
    axisY,
    hiddenDocuments: graph.documents.length - shownDocuments.size,
    hiddenExternal: graph.external.length - shownExternal.length,
  };
}
