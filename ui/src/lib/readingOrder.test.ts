import { describe, expect, it } from "vitest";
import {
  describeReadingPlacement,
  indexReadingOrder,
  readingOrderCounts,
  sortByReadingOrder,
} from "./readingOrder";
import type { ReadingOrder, ReadingOrderDocument } from "./types";

function document(path: string, overrides: Partial<ReadingOrderDocument> = {}): ReadingOrderDocument {
  return {
    path,
    placement: "bridge",
    subjects: [],
    after: [],
    needed_by: 0,
    theme_share: null,
    ...overrides,
  };
}

function order(documents: ReadingOrderDocument[], theme: string | null = null): ReadingOrder {
  return { root: "/lib", documents, subjects: [], theme };
}

const byName = (left: string, right: string) => left.localeCompare(right);
const paths = (entries: { path: string }[]) => entries.map((entry) => entry.path);

const shelf = order([
  document("/lib/arithmetic.pdf", { placement: "foundation", needed_by: 2 }),
  document("/lib/algebra.pdf", { placement: "bridge" }),
  document("/lib/calculus.pdf", { placement: "specialised" }),
  document("/lib/topology.pdf", { placement: "standalone" }),
  document("/lib/scan.pdf", { placement: "no_text" }),
]);
const listed = [
  { path: "/lib/scan.pdf" },
  { path: "/lib/calculus.pdf" },
  { path: "/lib/topology.pdf" },
  { path: "/lib/algebra.pdf" },
  { path: "/lib/arithmetic.pdf" },
];

describe("sortByReadingOrder", () => {
  it("lists the ordered documents first to last, then the rest", () => {
    expect(paths(sortByReadingOrder(listed, indexReadingOrder(shelf), "asc", byName))).toEqual([
      "/lib/arithmetic.pdf",
      "/lib/algebra.pdf",
      "/lib/calculus.pdf",
      "/lib/topology.pdf",
      "/lib/scan.pdf",
    ]);
  });

  it("reverses only what has an order when descending", () => {
    expect(paths(sortByReadingOrder(listed, indexReadingOrder(shelf), "desc", byName))).toEqual([
      "/lib/calculus.pdf",
      "/lib/algebra.pdf",
      "/lib/arithmetic.pdf",
      "/lib/topology.pdf",
      "/lib/scan.pdf",
    ]);
  });

  it("puts a file the order does not mention last, and lists by name without an order", () => {
    const withNew = [...listed, { path: "/lib/added.pdf" }];
    expect(paths(sortByReadingOrder(withNew, indexReadingOrder(shelf), "asc", byName)).at(-1)).toBe(
      "/lib/added.pdf",
    );
    expect(paths(sortByReadingOrder(listed, null, "asc", byName))).toEqual([
      "/lib/algebra.pdf",
      "/lib/arithmetic.pdf",
      "/lib/calculus.pdf",
      "/lib/scan.pdf",
      "/lib/topology.pdf",
    ]);
  });

  it("ranks by the theme when one was asked, keeping reading order among the rest", () => {
    const themed = order(
      [
        document("/lib/arithmetic.pdf", { placement: "foundation", theme_share: 0 }),
        document("/lib/algebra.pdf", { theme_share: 0.1 }),
        document("/lib/calculus.pdf", { placement: "specialised", theme_share: 0.4 }),
        document("/lib/scan.pdf", { placement: "no_text", theme_share: null }),
      ],
      "limits",
    );
    const entries = [
      { path: "/lib/scan.pdf" },
      { path: "/lib/arithmetic.pdf" },
      { path: "/lib/algebra.pdf" },
      { path: "/lib/calculus.pdf" },
    ];
    expect(paths(sortByReadingOrder(entries, indexReadingOrder(themed), "asc", byName))).toEqual([
      "/lib/calculus.pdf",
      "/lib/algebra.pdf",
      "/lib/arithmetic.pdf",
      "/lib/scan.pdf",
    ]);
  });
});

describe("describeReadingPlacement", () => {
  const nameOf = (path: string) => path.split("/").pop()!.replace(".pdf", "");

  it("says what a document builds on and why", () => {
    const described = describeReadingPlacement(
      document("/lib/calculus.pdf", {
        placement: "specialised",
        subjects: ["calculus"],
        after: [
          { path: "/lib/arithmetic.pdf", reasons: [{ kind: "names", subjects: ["arithmetic"] }] },
          { path: "/lib/algebra.pdf", reasons: [{ kind: "names", subjects: ["algebra"] }, { kind: "cites" }] },
          { path: "/lib/precalculus.pdf", reasons: [{ kind: "volume" }] },
        ],
      }),
      nameOf,
      null,
    );
    expect(described.value).toBe("Specialised · after arithmetic, algebra +1");
    expect(described.title).toBe(
      "Subjects: calculus. Builds on arithmetic (names arithmetic); algebra (names algebra, cites it); precalculus (later volume).",
    );
  });

  it("names a foundation by how many build on it, and the unplaced by why", () => {
    expect(
      describeReadingPlacement(document("/a", { placement: "foundation", needed_by: 1 }), nameOf, null).value,
    ).toBe("Foundation · 1 builds on it");
    expect(describeReadingPlacement(document("/a", { placement: "standalone" }), nameOf, null).value).toBe(
      "Read at any time",
    );
    expect(describeReadingPlacement(document("/a", { placement: "no_text" }), nameOf, null).value).toBe(
      "Not placed · text not read",
    );
    expect(describeReadingPlacement(document("/a", { placement: "no_subject" }), nameOf, null).value).toBe(
      "Not placed · no subject found",
    );
  });

  it("leads with how much of a document is on the theme", () => {
    expect(
      describeReadingPlacement(
        document("/a", { placement: "foundation", needed_by: 3, theme_share: 0.123 }),
        nameOf,
        "limits",
      ).value,
    ).toBe("12% of it on limits · Foundation · 3 build on it");
    expect(describeReadingPlacement(document("/a", { theme_share: 0.001 }), nameOf, "limits").value).toMatch(
      /^<1% of it on limits/,
    );
    expect(describeReadingPlacement(document("/a", { theme_share: 0 }), nameOf, "limits").value).toBe(
      "Does not discuss limits",
    );
  });
});

describe("readingOrderCounts", () => {
  it("counts each part of the order", () => {
    expect(readingOrderCounts(shelf)).toEqual({ ordered: 3, any_time: 1, not_placed: 1, discussing: 0 });
  });
});
