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
  it("takes two unique excerpts from each of the top five ranked files", () => {
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
      excerpts: ["finding 0-1", "finding 0-2"],
      path: "/papers/file-0.pdf",
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

  it("prefers query-relevant prose over an earlier noisy figure caption", () => {
    const input = buildSearchResultsSummaryInput(
      "use of econometric methods in computer science research",
      [
        {
          path: "/papers/methods.pdf",
          file_type: "Pdf",
          matches: [
            match(
              "Fig. 6. The number of studies using metrics. 555 139137 14113141 1414141414141414",
            ),
            match(
              "Computer science research uses econometric methods to estimate causal effects in observational software data.",
            ),
            match(
              "Econometric models also measure how developer incentives affect project outcomes.",
            ),
          ],
        },
      ],
    );

    expect(input.files[0].excerpts).toEqual([
      "Computer science research uses econometric methods to estimate causal effects in observational software data.",
      "Econometric models also measure how developer incentives affect project outcomes.",
    ]);
  });

  it("backfills the source set when an earlier file has no usable match", () => {
    const results: FileMatches[] = [
      {
        path: "/papers/empty.pdf",
        file_type: "Pdf",
        matches: [match("   ")],
      },
      ...Array.from({ length: 5 }, (_, index) => ({
        path: `/papers/source-${index}.pdf`,
        file_type: "Pdf" as const,
        matches: [match(`Relevant research finding ${index}`)],
      })),
    ];

    const input = buildSearchResultsSummaryInput("research", results);

    expect(input.files).toHaveLength(5);
    expect(input.files.at(-1)?.path).toBe("/papers/source-4.pdf");
  });
});
