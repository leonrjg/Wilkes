import type {
  FileMatches,
  Match,
  SearchResultsSummaryInput,
  SearchResultsSummaryPassage,
  SearchResultsSummarySource,
} from "../types";

export const SUMMARY_MAX_SOURCES = 5;
export const SUMMARY_MAX_PASSAGES = 6;
export const SUMMARY_MAX_PASSAGES_PER_SOURCE = 2;
export const SUMMARY_MAX_PASSAGE_CHARS = 700;
export const SUMMARY_MAX_CHAT_SOURCES = 8;
const SUMMARY_MAX_QUERY_CHARS = 500;

interface RankedMatch {
  result: FileMatches;
  match: Match;
  ordinal: number;
}

function truncateChars(text: string, maxChars: number): string {
  return Array.from(text).slice(0, maxChars).join("");
}

function normalizedText(text: string): string {
  return text
    .trim()
    .replace(/(\p{L})-\s+(?=\p{Ll})/gu, "$1")
    .replace(/\s+/g, " ");
}

function words(text: string): string[] {
  return text.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

function fileName(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

function repeatedSequence(text: string): boolean {
  return /([\p{L}\p{N}]{1,4})\1{3,}/iu.test(text);
}

function isBibliographyLike(text: string): boolean {
  const years =
    text.match(/\b(?:18|19|20)\d{2}[a-z]?\b/giu)?.length ?? 0;
  const publicationMarkers =
    text.match(
      /\b(?:arxiv|doi|journal|proceedings|technical report|working paper|university press|business review press|announcement|econometrica|transactions)\b/giu,
    )?.length ?? 0;
  return (
    /^\s*(?:references|bibliography)\b/iu.test(text) ||
    years >= 3 ||
    (years >= 2 && publicationMarkers >= 1)
  );
}

function isBoilerplateLike(text: string): boolean {
  return (
    /^(?:fig(?:ure)?|table)\.?\s*\d+/iu.test(text) ||
    /\bauthorized licensed use limited to\b/iu.test(text) ||
    /\bfor more information\b.*\bdigital library\b/iu.test(text) ||
    (text.match(/\b[\p{L}\p{N}._%+-]+@[\p{L}\p{N}.-]+\.[\p{L}]{2,}\b/giu)
      ?.length ?? 0) >= 2
  );
}

/**
 * Split normalized extraction text without treating decimal points as sentence
 * endings. Trailing fragments are deliberately omitted.
 */
function completeSentences(text: string): string[] {
  const normalized = normalizedText(text);
  if (!normalized) return [];

  const sentences: string[] = [];
  let start = 0;
  for (let index = 0; index < normalized.length; index += 1) {
    if (!".!?".includes(normalized[index])) continue;

    let end = index + 1;
    while (end < normalized.length && /["'’”)\]]/u.test(normalized[end])) {
      end += 1;
    }
    if (end < normalized.length && !/\s/u.test(normalized[end])) continue;

    let next = end;
    while (next < normalized.length && /\s/u.test(normalized[next])) next += 1;
    if (
      next < normalized.length &&
      !/[\p{Lu}\p{N}]/u.test(normalized[next])
    ) {
      continue;
    }

    const sentence = normalized.slice(start, end).trim();
    if (sentence) sentences.push(sentence);
    start = next;
    index = next - 1;
  }
  return sentences;
}

function withoutInlineCitations(text: string): string {
  return normalizedText(
    text
      .replace(/\[(?:\d+(?:\s*[,;–-]\s*\d+)*)\]/gu, "")
      .replace(/\s+([,.;:!?])/gu, "$1"),
  );
}

function isSubstantiveSentence(text: string): boolean {
  const sentenceWords = words(text);
  const alphaNumeric = Array.from(text).filter((character) =>
    /[\p{L}\p{N}]/u.test(character),
  );
  const digits = alphaNumeric.filter((character) =>
    /\p{N}/u.test(character),
  ).length;
  const digitRatio = alphaNumeric.length === 0 ? 1 : digits / alphaNumeric.length;
  return (
    Array.from(text).length <= SUMMARY_MAX_PASSAGE_CHARS &&
    sentenceWords.length >= 6 &&
    new Set(sentenceWords).size >= 5 &&
    Array.from(text).filter((character) => /\p{L}/u.test(character)).length >=
      20 &&
    digitRatio <= 0.35 &&
    !/^(?:fig(?:ure)?|table)\.?\s*\d+/iu.test(text) &&
    !repeatedSequence(text) &&
    !isBibliographyLike(text)
  );
}

function cleanPassage(raw: string): string | null {
  const passage = normalizedText(raw);
  if (
    !passage ||
    isBibliographyLike(passage) ||
    isBoilerplateLike(passage) ||
    repeatedSequence(passage)
  ) {
    return null;
  }

  const selected: string[] = [];
  let selectedChars = 0;
  for (const rawSentence of completeSentences(passage)) {
    const sentence = withoutInlineCitations(rawSentence);
    if (!isSubstantiveSentence(sentence)) continue;
    const separatorChars = selected.length > 0 ? 1 : 0;
    const sentenceChars = Array.from(sentence).length;
    if (
      selectedChars + separatorChars + sentenceChars >
      SUMMARY_MAX_PASSAGE_CHARS
    ) {
      break;
    }
    selected.push(sentence);
    selectedChars += separatorChars + sentenceChars;
  }
  return selected.length > 0 ? selected.join(" ") : null;
}

/**
 * `FileMatches` groups matches by file, so flatten and restore the semantic
 * provider's authoritative score order before cleaning. Cleaning may remove a
 * match, but no lexical or model-based score is allowed to promote one.
 * Grep matches carry no score and retain their original traversal order.
 */
function rankedMatches(results: FileMatches[]): RankedMatch[] {
  let ordinal = 0;
  const matches = results.flatMap((result) =>
    result.matches.map((match) => ({ result, match, ordinal: ordinal++ })),
  );
  return matches.sort((left, right) => {
    if (left.match.score !== undefined && right.match.score !== undefined) {
      return right.match.score - left.match.score || left.ordinal - right.ordinal;
    }
    return left.ordinal - right.ordinal;
  });
}

export function buildSearchResultsSummaryInput(
  query: string,
  results: FileMatches[],
): SearchResultsSummaryInput {
  const normalizedQuery = truncateChars(
    normalizedText(query),
    SUMMARY_MAX_QUERY_CHARS,
  );
  const sources: SearchResultsSummarySource[] = [];
  const passages: SearchResultsSummaryPassage[] = [];
  const sourceIndices = new Map<string, number>();
  const sourceCounts = new Map<string, number>();
  const seenPassages = new Set<string>();

  for (const { result, match } of rankedMatches(results)) {
    if (passages.length === SUMMARY_MAX_PASSAGES) break;
    const text = cleanPassage(
      [match.context_before, match.matched_text, match.context_after]
        .filter(Boolean)
        .join(" "),
    );
    if (!text) continue;
    const identity = text.toLowerCase();
    if (seenPassages.has(identity)) continue;

    const sourceCount = sourceCounts.get(result.path) ?? 0;
    if (sourceCount === SUMMARY_MAX_PASSAGES_PER_SOURCE) continue;
    let sourceIndex = sourceIndices.get(result.path);
    if (sourceIndex === undefined) {
      if (sources.length === SUMMARY_MAX_SOURCES) continue;
      sourceIndex = sources.length;
      sourceIndices.set(result.path, sourceIndex);
      sources.push({ title: fileName(result.path), path: result.path });
    }

    passages.push({ text, source_index: sourceIndex });
    sourceCounts.set(result.path, sourceCount + 1);
    seenPassages.add(identity);
  }

  return { query: normalizedQuery, sources, passages };
}

export function searchResultsSummaryKey(input: SearchResultsSummaryInput): string {
  return JSON.stringify(input);
}

export function buildSearchResultsChatPrompt(
  query: string,
  results: FileMatches[],
): string {
  const paths = [
    ...new Set(
      results
        .filter((result) => result.matches.length > 0)
        .map((result) => result.path),
    ),
  ].slice(0, SUMMARY_MAX_CHAT_SOURCES);
  const files =
    paths.length > 0
      ? `\n\nSearch result files:\n${paths.map((path) => `- ${path}`).join("\n")}`
      : "";
  return (
    "Investigate this library search using the result files below. Explain how " +
    "the evidence answers the query, distinguish direct evidence from inference, " +
    `and cite the relevant filenames.\n\nSearch query: ${normalizedText(query)}` +
    files
  );
}
