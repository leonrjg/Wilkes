import type {
  FileSortDirection,
  ReadingOrder,
  ReadingOrderDocument,
  ReadingOrderReason,
  ReadingPlacement,
} from "./types";

/** The three parts a reading order lists a folder in. */
export type ReadingGroup = "ordered" | "any_time" | "not_placed";

export function readingGroup(placement: ReadingPlacement): ReadingGroup {
  if (placement === "standalone") return "any_time";
  if (placement === "no_subject" || placement === "no_text") return "not_placed";
  return "ordered";
}

export const READING_GROUP_LABELS: Record<Exclude<ReadingGroup, "ordered">, string> = {
  any_time: "Read at any time",
  not_placed: "Not placed",
};

export interface ReadingOrderIndex {
  byPath: Map<string, { document: ReadingOrderDocument; position: number }>;
  /** The theme the documents were also read for, if one was asked. */
  theme: string | null;
}

export function indexReadingOrder(order: ReadingOrder): ReadingOrderIndex {
  return {
    byPath: new Map(
      order.documents.map((document, position) => [document.path, { document, position }]),
    ),
    theme: order.theme,
  };
}

const GROUP_RANK: Record<ReadingGroup, number> = { ordered: 0, any_time: 1, not_placed: 2 };

/**
 * `entries` in reading order, or by how much each discusses the theme when
 * the order was asked with one.
 *
 * Descending reverses only what has an order: the documents in sequence, or
 * the ones that discuss the theme. What can be read at any time, and what
 * could not be placed, stays after them either way. An entry the order does
 * not mention (the listing is newer than the order) goes last, by name.
 */
export function sortByReadingOrder<T extends { path: string }>(
  entries: T[],
  index: ReadingOrderIndex | null,
  direction: FileSortDirection,
  compareNames: (left: string, right: string) => number,
): T[] {
  const flip = direction === "desc" ? -1 : 1;
  return [...entries].sort((left, right) => {
    const a = index?.byPath.get(left.path);
    const b = index?.byPath.get(right.path);
    if (!a || !b) {
      if (a) return -1;
      if (b) return 1;
      return compareNames(left.path, right.path);
    }
    if (index?.theme) {
      const shareA = a.document.theme_share ?? 0;
      const shareB = b.document.theme_share ?? 0;
      if (shareA > 0 || shareB > 0) {
        if (shareA === 0) return 1;
        if (shareB === 0) return -1;
        if (shareA !== shareB) return (shareB - shareA) * flip;
      }
      return a.position - b.position;
    }
    const groupA = GROUP_RANK[readingGroup(a.document.placement)];
    const groupB = GROUP_RANK[readingGroup(b.document.placement)];
    if (groupA !== groupB) return groupA - groupB;
    return (a.position - b.position) * (groupA === 0 ? flip : 1);
  });
}

function reasonText(reason: ReadingOrderReason): string {
  switch (reason.kind) {
    case "names":
      return `names ${reason.subjects.join(", ")}`;
    case "level":
      return "same subject, higher level";
    case "volume":
      return "later volume";
    case "narrower":
      return "narrower subject";
    case "cites":
      return "cites it";
  }
}

/** How many names of the documents a row builds on fit on its line. */
const NAMED_AFTER = 2;

function placementText(document: ReadingOrderDocument, nameOf: (path: string) => string): string {
  const after = () => {
    const named = document.after.slice(0, NAMED_AFTER).map((link) => nameOf(link.path));
    const more = document.after.length - named.length;
    return `after ${named.join(", ")}${more > 0 ? ` +${more}` : ""}`;
  };
  switch (document.placement) {
    case "foundation":
      return `Foundation · ${document.needed_by} build${document.needed_by === 1 ? "s" : ""} on it`;
    case "bridge":
      return `Bridge · ${after()}`;
    case "specialised":
      return `Specialised · ${after()}`;
    case "standalone":
      return "Read at any time";
    case "no_subject":
      return "Not placed · no subject found";
    case "no_text":
      return "Not placed · text not read";
  }
}

function sharePercent(share: number): string {
  const percent = Math.round(share * 100);
  return percent < 1 ? "<1%" : `${percent}%`;
}

/**
 * What a row says about where its document stands, and the longer account
 * shown on hover: its subjects, and each document it builds on with the
 * reason.
 */
export function describeReadingPlacement(
  document: ReadingOrderDocument,
  nameOf: (path: string) => string,
  theme: string | null,
): { value: string; title: string } {
  const placement = placementText(document, nameOf);
  let value = placement;
  if (theme) {
    const share = document.theme_share ?? 0;
    value = share > 0
      ? `${sharePercent(share)} of it on ${theme} · ${placement}`
      : `Does not discuss ${theme}`;
  }
  // One paragraph: a tooltip does not keep line breaks.
  const sentences: string[] = [];
  if (document.subjects.length > 0) sentences.push(`Subjects: ${document.subjects.join(", ")}.`);
  if (document.after.length > 0) {
    const links = document.after.map(
      (link) => `${nameOf(link.path)} (${link.reasons.map(reasonText).join(", ")})`,
    );
    sentences.push(`Builds on ${links.join("; ")}.`);
  }
  if (document.needed_by > 0) {
    sentences.push(
      `${document.needed_by} document${document.needed_by === 1 ? "" : "s"} here build${document.needed_by === 1 ? "s" : ""} on it.`,
    );
  }
  if (document.placement === "no_text") {
    sentences.push("Its text has not been read yet, or it has none.");
  }
  if (document.placement === "no_subject") {
    sentences.push(
      "Its title and folders name nothing its text uses, and it names no other document's subject.",
    );
  }
  return { value, title: sentences.join(" ") };
}

/** How many documents the order put in each part. */
export function readingOrderCounts(order: ReadingOrder): Record<ReadingGroup, number> & { discussing: number } {
  const counts = { ordered: 0, any_time: 0, not_placed: 0, discussing: 0 };
  for (const document of order.documents) {
    counts[readingGroup(document.placement)] += 1;
    if ((document.theme_share ?? 0) > 0) counts.discussing += 1;
  }
  return counts;
}
