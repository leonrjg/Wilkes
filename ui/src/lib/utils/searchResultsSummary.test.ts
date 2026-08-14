import { describe, expect, it } from "vitest";
import type { FileMatches, Match } from "../types";
import {
  buildSearchResultsChatPrompt,
  buildSearchResultsSummaryInput,
  SUMMARY_MAX_PASSAGE_CHARS,
  SUMMARY_MAX_PASSAGES,
  SUMMARY_MAX_PASSAGES_PER_SOURCE,
  SUMMARY_MAX_SOURCES,
} from "./searchResultsSummary";

function match(text: string, score?: number): Match {
  return {
    text_range: null,
    matched_text: text,
    context_before: "",
    context_after: "",
    origin: { PdfPage: { page: 1, bbox: null } },
    ...(score === undefined ? {} : { score }),
  };
}

function file(
  path: string,
  matches: Match[],
  fileType: FileMatches["file_type"] = "Pdf",
): FileMatches {
  return { path, file_type: fileType, matches };
}

describe("buildSearchResultsSummaryInput", () => {
  it("restores global semantic order from file-grouped matches without reranking", () => {
    const input = buildSearchResultsSummaryInput("open ended query", [
      file("/papers/a.pdf", [
        match(
          "The highest ranked passage reports the primary measured outcome.",
          0.99,
        ),
        match(
          "The third ranked passage reports a later secondary measured outcome.",
          0.75,
        ),
      ]),
      file("/papers/b.pdf", [
        match(
          "The second ranked passage reports the replicated measured outcome.",
          0.9,
        ),
      ]),
    ]);

    expect(input.passages.map((passage) => passage.text)).toEqual([
      "The highest ranked passage reports the primary measured outcome.",
      "The second ranked passage reports the replicated measured outcome.",
      "The third ranked passage reports a later secondary measured outcome.",
    ]);
    expect(input.passages.map((passage) => passage.source_index)).toEqual([
      0, 1, 0,
    ]);
  });

  it("does not promote the passage at semantic rank 290", () => {
    const results = Array.from({ length: 300 }, (_, index) =>
      file(`/papers/rank-${index + 1}.pdf`, [
        match(
          index === 289
            ? "The two-way fixed effects estimator is commonly used in early econometric studies."
            : `The passage at semantic rank ${index + 1} reports a substantive research result.`,
          1 - index / 1000,
        ),
      ]),
    );

    const input = buildSearchResultsSummaryInput(
      "use of econometric methods in computer science research",
      results,
    );

    expect(input.passages.length).toBeGreaterThan(0);
    expect(input.passages.every((passage) => !passage.text.includes("two-way"))).toBe(
      true,
    );
    expect(input.sources.map((source) => source.title)).toEqual([
      "rank-1.pdf",
      "rank-2.pdf",
      "rank-3.pdf",
      "rank-4.pdf",
      "rank-5.pdf",
    ]);
  });

  it("removes dirty passages and keeps the remaining passages in rank order", () => {
    const input = buildSearchResultsSummaryInput("Bayesian learning", [
      file("/papers/references.pdf", [
        match(
          "References. Isaac Baley and Laura Veldkamp. Bayesian learning. NBER Working Paper 29338, 2021. Another Author. Related title. Journal of Economics, 2023.",
          0.99,
        ),
      ]),
      file("/papers/clean.pdf", [
        match(
          "The first clean passage describes a measured result from the study.",
          0.98,
        ),
      ]),
      file("/papers/noise.pdf", [
        match("Fig. 6. 1414141414141414 555 139137 14113141.", 0.97),
      ]),
      file("/papers/next.pdf", [
        match(
          "The next clean passage explains the observed result in the experiment.",
          0.96,
        ),
      ]),
    ]);

    expect(input.sources.map((source) => source.title)).toEqual([
      "clean.pdf",
      "next.pdf",
    ]);
    expect(input.passages.map((passage) => passage.text)).toEqual([
      "The first clean passage describes a measured result from the study.",
      "The next clean passage explains the observed result in the experiment.",
    ]);
  });

  it("dehyphenates PDF text, removes inline citations, and omits trailing fragments", () => {
    const input = buildSearchResultsSummaryInput("causal evidence", [
      {
        ...file("/papers/evidence.pdf", [
          match(
            "The quasi-experi-\nmental design estimates a causal effect across projects [1, 2]. This trailing fragment has no ending",
            0.9,
          ),
        ]),
      },
    ]);

    expect(input.passages).toEqual([
      {
        text:
          "The quasi-experimental design estimates a causal effect across projects.",
        source_index: 0,
      },
    ]);
  });

  it("enforces passage, source, per-source, character, and duplicate bounds", () => {
    const overlong = `Evidence ${"é".repeat(SUMMARY_MAX_PASSAGE_CHARS)} ends here.`;
    const results: FileMatches[] = [
      file("/papers/a.pdf", [
        match(
          "Source A passage one reports a substantive measured research outcome.",
          1,
        ),
        match(
          "Source A passage two reports another substantive measured research outcome.",
          0.99,
        ),
        match(
          "Source A passage three should exceed the per-source evidence limit.",
          0.98,
        ),
      ]),
      file("/papers/b.pdf", [
        match(
          "Source B passage one reports a substantive measured research outcome.",
          0.97,
        ),
        match(
          "Source B passage two reports another substantive measured research outcome.",
          0.96,
        ),
      ]),
      file("/papers/c.pdf", [
        match(
          "Source C passage one reports a substantive measured research outcome.",
          0.95,
        ),
        match(
          "Source C passage two reports another substantive measured research outcome.",
          0.94,
        ),
      ]),
      file("/papers/d.pdf", [
        match(
          "Source C passage two reports another substantive measured research outcome.",
          0.93,
        ),
        match(overlong, 0.92),
      ]),
    ];

    const input = buildSearchResultsSummaryInput("research outcomes", results);

    expect(input.passages).toHaveLength(SUMMARY_MAX_PASSAGES);
    expect(input.sources.length).toBeLessThanOrEqual(SUMMARY_MAX_SOURCES);
    for (let sourceIndex = 0; sourceIndex < input.sources.length; sourceIndex += 1) {
      expect(
        input.passages.filter(
          (passage) => passage.source_index === sourceIndex,
        ).length,
      ).toBeLessThanOrEqual(SUMMARY_MAX_PASSAGES_PER_SOURCE);
    }
    expect(new Set(input.passages.map((passage) => passage.text)).size).toBe(
      input.passages.length,
    );
    expect(
      input.passages.every(
        (passage) =>
          Array.from(passage.text).length <= SUMMARY_MAX_PASSAGE_CHARS,
      ),
    ).toBe(true);
  });

  it("preserves traversal order for grep matches that have no semantic score", () => {
    const input = buildSearchResultsSummaryInput("literal", [
      file(
        "/notes/first.txt",
        [
          match(
            "The first literal result contains enough substantive prose for evidence.",
          ),
        ],
        "PlainText",
      ),
      file(
        "/notes/second.txt",
        [
          match(
            "The second literal result also contains enough substantive prose.",
          ),
        ],
        "PlainText",
      ),
    ]);

    expect(input.sources.map((source) => source.title)).toEqual([
      "first.txt",
      "second.txt",
    ]);
  });

  it("returns an explicit empty evidence set when only references and noise remain", () => {
    const input = buildSearchResultsSummaryInput("Bayesian learning", [
      file("/references.pdf", [
        match(
          "References. Isaac Baley and Laura Veldkamp. Bayesian learning. NBER Working Paper 29338, 2021. Another Author. Related title. Journal of Economics, 2023.",
          0.9,
        ),
        match("Fig. 6. 1414141414141414 555 139137 14113141.", 0.8),
      ]),
    ]);

    expect(input.sources).toEqual([]);
    expect(input.passages).toEqual([]);
  });
});

describe("buildSearchResultsChatPrompt", () => {
  it("hands the open-ended query and result files to agent chat", () => {
    const prompt = buildSearchResultsChatPrompt("causal methods", [
      file("/papers/a.pdf", [match("A substantive result appears in this document.")]),
      {
        ...file("/papers/title-only.pdf", []),
        field_matches: [{
          field: "title",
          matched_text: "causal methods",
          context_before: "",
          context_after: "",
        }],
      },
      file("/papers/b.pdf", [match("Another substantive result appears here.")]),
    ]);

    expect(prompt).toContain("Search query: causal methods");
    expect(prompt).toContain("- /papers/a.pdf");
    expect(prompt).toContain("- /papers/b.pdf");
    expect(prompt).not.toContain("title-only.pdf");
  });
});
