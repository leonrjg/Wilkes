import { describe, expect, it } from "vitest";
import type { FileMatches, Match } from "../types";
import {
  buildSearchResultsSummaryInput,
  SUMMARY_MAX_EXCERPT_CHARS,
} from "./searchResultsSummary";

function match(text: string): Match {
  return {
    text_range: null,
    matched_text: text,
    context_before: "",
    context_after: "",
    origin: { PdfPage: { page: 1, bbox: null } },
    score: 0.9,
  };
}

describe("buildSearchResultsSummaryInput", () => {
  it("takes three unique excerpts from each of the top five ranked files", () => {
    const results: FileMatches[] = Array.from({ length: 6 }, (_, fileIndex) => ({
      path: `/papers/file-${fileIndex}.pdf`,
      file_type: "Pdf",
      matches: [
        match(`finding ${fileIndex}-1`),
        match(`finding ${fileIndex}-1`),
        match(`finding ${fileIndex}-2`),
        match(`finding ${fileIndex}-3`),
        match(`finding ${fileIndex}-4`),
      ],
    }));

    const input = buildSearchResultsSummaryInput("  cache   behavior  ", results);

    expect(input.query).toBe("cache behavior");
    expect(input.files).toHaveLength(5);
    expect(input.files[0]).toEqual({
      title: "file-0.pdf",
      excerpts: ["finding 0-1", "finding 0-2", "finding 0-3"],
    });
    expect(input.files.at(-1)?.title).toBe("file-4.pdf");
  });

  it("combines grep context and truncates by characters", () => {
    const long = "é".repeat(SUMMARY_MAX_EXCERPT_CHARS + 20);
    const input = buildSearchResultsSummaryInput("query", [
      {
        path: "/paper.txt",
        file_type: "PlainText",
        matches: [
          {
            ...match(long),
            context_before: "before",
            context_after: "after",
          },
        ],
      },
    ]);

    expect(Array.from(input.files[0].excerpts[0])).toHaveLength(
      SUMMARY_MAX_EXCERPT_CHARS,
    );
    expect(input.files[0].excerpts[0]).toMatch(/^before é+/);
  });
});
