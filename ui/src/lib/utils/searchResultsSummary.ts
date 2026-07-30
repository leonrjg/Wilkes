import type {
  FileMatches,
  SearchResultsSummaryInput,
  SearchResultsSummaryFile,
} from "../types";

export const SUMMARY_MAX_FILES = 5;
export const SUMMARY_MAX_EXCERPTS_PER_FILE = 2;
export const SUMMARY_MAX_EXCERPT_CHARS = 420;
const SUMMARY_MAX_QUERY_CHARS = 500;
const QUERY_STOP_WORDS = new Set([
  "a",
  "an",
  "and",
  "are",
  "as",
  "at",
  "be",
  "by",
  "for",
  "from",
  "how",
  "in",
  "is",
  "of",
  "on",
  "or",
  "that",
  "the",
  "to",
  "use",
  "what",
  "with",
]);

function truncateChars(text: string, maxChars: number): string {
  return Array.from(text).slice(0, maxChars).join("");
}

function normalizedText(text: string): string {
  return text.trim().replace(/\s+/g, " ");
}

function words(text: string): string[] {
  return text.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

function queryTerms(query: string): Set<string> {
  return new Set(
    words(query).filter((word) => word.length >= 3 && !QUERY_STOP_WORDS.has(word)),
  );
}

function repeatedSequencePenalty(text: string): number {
  // OCR corruption and decoder-like numeric runs are especially distracting to
  // a small model. Penalize rather than reject so numeric research queries can
  // still use a passage when it is the best available evidence.
  return /([\p{L}\p{N}]{1,4})\1{3,}/iu.test(text) ? 12 : 0;
}

function passageScore(
  excerpt: string,
  matchedText: string,
  semanticScore: number | undefined,
  terms: Set<string>,
  originalIndex: number,
): number {
  const excerptWords = words(excerpt);
  const excerptWordSet = new Set(excerptWords);
  const matchedWordSet = new Set(words(matchedText));
  let queryCoverage = 0;
  let directCoverage = 0;
  for (const term of terms) {
    if (excerptWordSet.has(term)) queryCoverage += 1;
    if (matchedWordSet.has(term)) directCoverage += 1;
  }

  const alphaNumeric = Array.from(excerpt).filter((char) =>
    /[\p{L}\p{N}]/u.test(char),
  );
  const digits = alphaNumeric.filter((char) => /\p{N}/u.test(char)).length;
  const digitRatio = alphaNumeric.length === 0 ? 1 : digits / alphaNumeric.length;
  const captionPenalty = /^(?:fig(?:ure)?|table)\.?\s*\d+/iu.test(excerpt) ? 3 : 0;
  const numericPenalty = digitRatio > 0.45 ? 8 : digitRatio > 0.25 ? 3 : 0;
  const proseBonus =
    excerptWords.length >= 8 && new Set(excerptWords).size >= 6 ? 2 : 0;

  return (
    queryCoverage * 6 +
    directCoverage * 4 +
    (semanticScore ?? 0) * 5 +
    proseBonus -
    captionPenalty -
    numericPenalty -
    repeatedSequencePenalty(excerpt) -
    originalIndex * 0.01
  );
}

function fileName(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

export function buildSearchResultsSummaryInput(
  query: string,
  results: FileMatches[],
): SearchResultsSummaryInput {
  const seen = new Set<string>();
  const files: SearchResultsSummaryFile[] = [];
  const terms = queryTerms(query);

  for (const result of results) {
    if (files.length === SUMMARY_MAX_FILES) break;
    const excerpts: string[] = [];
    const candidates = result.matches
      .map((match, originalIndex) => {
        const excerpt = normalizedText(
          [match.context_before, match.matched_text, match.context_after]
            .filter(Boolean)
            .join(" "),
        );
        return {
          excerpt,
          score: passageScore(
            excerpt,
            match.matched_text,
            match.score,
            terms,
            originalIndex,
          ),
        };
      })
      .filter(({ excerpt }) => excerpt.length > 0)
      .sort((left, right) => right.score - left.score);

    for (const { excerpt } of candidates) {
      if (excerpts.length === SUMMARY_MAX_EXCERPTS_PER_FILE) break;
      const identity = excerpt.toLowerCase();
      if (seen.has(identity)) continue;
      seen.add(identity);
      excerpts.push(truncateChars(excerpt, SUMMARY_MAX_EXCERPT_CHARS));
    }
    if (excerpts.length > 0) {
      files.push({ title: fileName(result.path), excerpts, path: result.path });
    }
  }

  return {
    query: truncateChars(normalizedText(query), SUMMARY_MAX_QUERY_CHARS),
    files,
  };
}

export function searchResultsSummaryKey(input: SearchResultsSummaryInput): string {
  return JSON.stringify(input);
}
