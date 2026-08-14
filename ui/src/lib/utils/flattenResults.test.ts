import { describe, it, expect } from "vitest";
import { buildRows, COLLAPSED_LIMIT } from "./flattenResults";
import type { FileMatches } from "../types";

describe("flattenResults", () => {
  it("should build rows for files and matches", () => {
    const results: FileMatches[] = [
      {
        path: "file1.txt",
        file_type: "PlainText",
        matches: [
          { matched_text: "m1", context_before: "", context_after: "", origin: { TextFile: { line: 1, col: 1 } }, text_range: { start: 0, end: 2 } },
        ],
      },
    ];

    const rows = buildRows(results, new Set());
    expect(rows).toHaveLength(2);
    expect(rows[0]).toEqual({ kind: "file", fileMatches: results[0], fileIndex: 0, path: "file1.txt" });
    expect(rows[1].kind).toBe("match");
  });

  it("should respect COLLAPSED_LIMIT and show expand row", () => {
    const matches = Array.from({ length: COLLAPSED_LIMIT + 2 }).map((_, i) => ({
      matched_text: `m${i}`,
      context_before: "",
      context_after: "",
      origin: { TextFile: { line: i + 1, col: 1 } },
      text_range: { start: 0, end: 2 },
    }));

    const results: FileMatches[] = [
      {
        path: "file1.txt",
        file_type: "PlainText",
        matches,
      },
    ];

    const rows = buildRows(results, new Set());
    // 1 file row + COLLAPSED_LIMIT match rows + 1 expand row
    expect(rows).toHaveLength(1 + COLLAPSED_LIMIT + 1);
    expect(rows[rows.length - 1]).toEqual({ kind: "expand", fileIndex: 0, totalMatches: matches.length });
  });

  it("should show all matches when file is expanded", () => {
    const matches = Array.from({ length: COLLAPSED_LIMIT + 2 }).map((_, i) => ({
      matched_text: `m${i}`,
      context_before: "",
      context_after: "",
      origin: { TextFile: { line: i + 1, col: 1 } },
      text_range: { start: 0, end: 2 },
    }));

    const results: FileMatches[] = [
      {
        path: "file1.txt",
        file_type: "PlainText",
        matches,
      },
    ];

    const rows = buildRows(results, new Set([0]));
    // 1 file row + all match rows
    expect(rows).toHaveLength(1 + matches.length);
    expect(rows.some(r => r.kind === "expand")).toBe(false);
  });

  it("places filename and title hits before content and counts them toward collapse", () => {
    const results: FileMatches[] = [{
      path: "report.txt",
      file_type: "PlainText",
      field_matches: [
        { field: "filename", matched_text: "report", context_before: "", context_after: ".txt" },
        { field: "title", matched_text: "Report", context_before: "Annual ", context_after: "" },
      ],
      matches: Array.from({ length: COLLAPSED_LIMIT }, (_, index) => ({
        matched_text: `m${index}`,
        context_before: "",
        context_after: "",
        origin: { TextFile: { line: index + 1, col: 1 } },
        text_range: { start: 0, end: 2 },
      })),
    }];

    const rows = buildRows(results, new Set());

    expect(rows.slice(1, 3).map((row) => row.kind)).toEqual([
      "field_match",
      "field_match",
    ]);
    expect(rows.filter((row) => row.kind === "match")).toHaveLength(
      COLLAPSED_LIMIT - 2,
    );
    expect(rows.at(-1)).toEqual({
      kind: "expand",
      fileIndex: 0,
      totalMatches: COLLAPSED_LIMIT + 2,
    });
  });
});
