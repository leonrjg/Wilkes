import type {
  FileMatches,
  SearchResultsSummaryInput,
  SearchResultsSummaryFile,
} from "../types";

export const SUMMARY_MAX_FILES = 5;
export const SUMMARY_MAX_EXCERPTS_PER_FILE = 3;
export const SUMMARY_MAX_EXCERPT_CHARS = 600;
const SUMMARY_MAX_QUERY_CHARS = 500;

function truncateChars(text: string, maxChars: number): string {
  return Array.from(text).slice(0, maxChars).join("");
}

function normalizedText(text: string): string {
  return text.trim().replace(/\s+/g, " ");
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

  for (const result of results.slice(0, SUMMARY_MAX_FILES)) {
    const excerpts: string[] = [];
    for (const match of result.matches) {
      if (excerpts.length === SUMMARY_MAX_EXCERPTS_PER_FILE) break;
      const excerpt = normalizedText(
        [match.context_before, match.matched_text, match.context_after]
          .filter(Boolean)
          .join(" "),
      );
      const identity = excerpt.toLowerCase();
      if (!excerpt || seen.has(identity)) continue;
      seen.add(identity);
      excerpts.push(truncateChars(excerpt, SUMMARY_MAX_EXCERPT_CHARS));
    }
    if (excerpts.length > 0) {
      files.push({ title: fileName(result.path), excerpts });
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
